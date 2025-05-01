// Copyright The pipewire-rs Contributors.
// SPDX-License-Identifier: MIT

//! This file is a rustic interpretation of the [PipeWire audio-capture.c example][example]
//!
//! example: https://docs.pipewire.org/audio-capture_8c-example.html

use clap::Parser;
use pipewire as pw;
use pw::{properties::properties, spa};

use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

use log::{debug, info};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::Sender;
use std::thread;

// Import from the library instead of local modules
use scribe::WhisperProcessor;
use scribe::WhisperSegment;
use scribe::{audio_utils, pipewire_utils};

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    sample_sender: Sender<Vec<f32>>,
}

#[derive(Parser)]
#[clap(name = "audio-capture", about = "Audio stream capture example")]
struct Opt {
    #[clap(short, long, help = "The target object id to connect to")]
    target: Option<String>,

    #[clap(
        short,
        long,
        help = "The whisper model to use for inference",
        default_value = "models/ggml-base.en.bin"
    )]
    model: String,

    #[clap(
        short,
        long,
        help = "Number of seconds to keep in the audio buffer",
        default_value = "5"
    )]
    buffer_seconds: u32,

    #[clap(
        short = 'o',
        long = "output-dir",
        help = "Directory to save processed audio as WAV files"
    )]
    output_dir: Option<PathBuf>,
    #[clap(
        short = 'l',
        long = "language",
        help = "Language code for whisper model",
        default_value = "en"
    )]
    language: Option<String>,
}

pub fn main() -> Result<(), pw::Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    info!("Starting audio capture application");

    // Log that we're using whisper-rs with log_backend feature enabled
    info!("Using whisper-rs with log_backend feature enabled");

    pw::init();

    let opt = Opt::parse();

    // Determine target ID - either directly from number or by pattern matching
    let target_ids = if let Some(target) = &opt.target {
        if let Ok(direct_id) = target.parse::<u32>() {
            // If the target is a valid number, use it directly
            info!("Using direct target ID: {}", direct_id);
            vec![direct_id]
        } else {
            // Otherwise use the pattern matching routine
            let pattern_vec = vec![target.clone()];
            match pipewire_utils::find_pipewire_ids_by_pattern(pattern_vec) {
                None => {
                    eprintln!("Error: No matching PipeWire sources found");
                    std::process::exit(1)
                }
                Some(ids) => ids,
            }
        }
    } else {
        // If no target specified, use empty pattern to find defaults
        let pattern_vec = vec!["".to_string()];
        match pipewire_utils::find_pipewire_ids_by_pattern(pattern_vec) {
            None => {
                eprintln!("Error: No matching PipeWire sources found");
                std::process::exit(1)
            }
            Some(ids) => ids,
        }
    };

    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    // Create output directory if specified and doesn't exist
    if let Some(output_dir) = &opt.output_dir {
        if !output_dir.exists() {
            fs::create_dir_all(output_dir).expect("Failed to create output directory");
        }
    }

    // Calculate the ring buffer size based on the desired seconds
    // Assuming 16000Hz is the inference rate for Whisper
    let inference_rate = 16000;
    let ring_buffer_size = (inference_rate * opt.buffer_seconds) as usize;

    // Create channels for audio samples and transcription segments
    let (sample_sender, sample_receiver) = mpsc::channel::<Vec<f32>>();
    let (segment_sender, segment_receiver) = mpsc::channel::<WhisperSegment>();

    let data = UserData {
        format: Default::default(),
        sample_sender: sample_sender.clone(),
    };

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };

    thread::spawn(move || {
        while let Ok(segment) = segment_receiver.recv() {
            debug!(
                "[{} - {} ({})]: {}",
                segment.start_timestamp,
                segment.end_timestamp,
                segment.first_token_dtw_ts,
                segment.text
            );
            println!("{}", segment.text);
        }
    });

    // Create and start the WhisperProcessor with the channel receiver
    let processor = WhisperProcessor::new(
        &opt.model,
        sample_receiver,
        ring_buffer_size,
        opt.output_dir.clone(),
        opt.language.clone(),
        segment_sender,
    );

    let stream = pw::stream::Stream::new(&core, "audio-capture", props)?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(move |_, user_data, id, param| {
            // NULL means to clear the format
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }

            let (media_type, media_subtype) = match format_utils::parse_format(param) {
                Ok(v) => v,
                Err(_) => return,
            };

            // only accept raw audio
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }

            // call a helper function to parse the format for us.
            user_data
                .format
                .parse(param)
                .expect("Failed to parse param changed to AudioInfoRaw");

            // Access the audio format details:
            // 1. Get the sample rate from the format
            let sample_rate = user_data.format.rate();

            // 2. Get the number of channels
            let channels = user_data.format.channels();

            // 3. Get the audio format (F32LE, etc.)
            let format = user_data.format.format();

            info!("Audio format details:");
            info!("  - Sample rate: {} Hz", sample_rate);
            info!("  - Channels: {}", channels);
            info!("  - Format: {:?}", format);

            println!("capturing rate:{} channels:{}", sample_rate, channels);
        })
        .process(move |stream, user_data| match stream.dequeue_buffer() {
            None => println!("out of buffers"),
            Some(mut buffer) => {
                let datas = buffer.datas_mut();
                if datas.is_empty() {
                    return;
                }

                let data = &mut datas[0];
                let n_channels = user_data.format.channels();
                let chunk = data.chunk();

                // Extract all information from chunk before borrowing data mutably
                let start_offset = chunk.offset() as usize;
                let data_size = chunk.size() as usize;
                let stride = chunk.stride() as usize;

                if let Some(samples_bytes) = data.data() {
                    // Assume contiguous data (stride equals sample size, since format is F32LE)
                    debug_assert!(stride == std::mem::size_of::<f32>() || stride == 0);

                    let float_samples = {
                        unsafe {
                            let start_ptr = samples_bytes.as_ptr().add(start_offset) as *const f32;
                            let n_samples = data_size / std::mem::size_of::<f32>();
                            std::slice::from_raw_parts(start_ptr, n_samples)
                        }
                    };

                    // Use our audio_utils function instead of whisper_rs
                    let mono_samples = if n_channels == 2 {
                        audio_utils::convert_stereo_to_mono(float_samples)
                    } else {
                        float_samples.to_vec()
                    };

                    // Send the entire chunk of samples at once instead of individually
                    if let Err(e) = user_data.sample_sender.send(mono_samples) {
                        debug!("Failed to send audio samples: {}", e);
                    }
                }
            }
        })
        .register()?;

    /* Make one parameter with the supported formats. The SPA_PARAM_EnumFormat
     * id means that this is a format enumeration (of 1 value).
     * We leave the channels and rate empty to accept the native graph
     * rate and channels. */
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(16000);
    audio_info.set_channels(1);

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    info!("Connecting to target ID: {:?}", target_ids[0]);
    /* Now connect this stream. We ask that our process function is
     * called in a realtime thread. */
    stream.connect(
        spa::utils::Direction::Input,
        Some(target_ids[0]),
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    mainloop.run();
    processor.stop();

    Ok(())
}
