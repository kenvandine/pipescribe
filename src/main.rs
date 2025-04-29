// Copyright The pipewire-rs Contributors.
// SPDX-License-Identifier: MIT

//! This file is a rustic interpretation of the [PipeWire audio-capture.c example][example]
//!
//! example: https://docs.pipewire.org/audio-capture_8c-example.html

use clap::Parser;
use pipewire as pw;
use pw::{properties::properties, spa};

use ringbuf::storage::Heap;
use ringbuf::traits::Split;
use ringbuf::wrap::caching::Caching;
use ringbuf::{SharedRb, producer::Producer};

use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use std::mem;
use std::sync::Arc;

use env_logger;
use hound;
use log::info;
use std::fs;
use std::path::{Path, PathBuf};

mod pipewire_utils;
mod whisper_processor;

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    cursor_move: bool,
    ring_producer: Caching<Arc<SharedRb<Heap<f32>>>, true, false>,
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
    whisper_rs::install_logging_hooks();

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
    let ring_buffer = SharedRb::new(ring_buffer_size);

    let (producer, consumer) = ring_buffer.split();

    let data = UserData {
        format: Default::default(),
        cursor_move: false,
        ring_producer: producer,
    };

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };

    // Create and start the WhisperProcessor
    let processor = whisper_processor::WhisperProcessor::new(
        &opt.model,
        consumer,
        ring_buffer_size,
        opt.output_dir.clone(),
        opt.language.clone(),
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
                let n_samples = chunk.size() / (mem::size_of::<f32>() as u32);

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

                    let mono_samples = if n_channels == 2 {
                        whisper_rs::convert_stereo_to_mono_audio(float_samples)
                            .expect("Failed to convert samples to mono")
                    } else {
                        float_samples.to_vec()
                    };

                    // Add the new samples to the ring buffer
                    for &sample in mono_samples.iter() {
                        let _ = user_data.ring_producer.try_push(sample);
                    }

                    if user_data.cursor_move {
                        print!("\x1B[{}A", n_channels + 1);
                    }
                    // info!("captured {} samples", n_samples / n_channels);

                    let mut max: f32 = 0.0;
                    for &sample in mono_samples.iter() {
                        max = max.max(sample.abs());
                    }

                    // Display the peak meter
                    /*
                    let peak = ((max * 30.0) as usize).clamp(0, 39);
                    println!(
                        "mono: |{:>w1$}{:w2$}| peak:{}",
                        "*",
                        "",
                        max,
                        w1 = peak + 1,
                        w2 = 40 - peak
                    );
                    user_data.cursor_move = true;
                    */
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

    // and wait while we let things run
    mainloop.run();

    // Stop the processor when exiting
    processor.stop();

    Ok(())
}

/// Write audio samples to a WAV file
fn write_wav_file(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;

    for &sample in samples {
        writer.write_sample(sample)?;
    }

    writer.finalize()?;
    Ok(())
}
