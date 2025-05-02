use log::{debug, info};
use pipewire as pw;
use pw::{properties::properties, spa};
use std::path::PathBuf;

use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

use std::fs;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::WhisperSegment;
use crate::audio_utils;
use crate::{WhisperProcessor, pipewire_utils};

pub struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    sample_sender: UnboundedSender<Vec<f32>>,
}

pub fn transcribe<F>(
    model_path: &str,
    buffer_seconds: u32,
    output_dir: Option<PathBuf>,
    language: Option<String>,
    target_id: u32,          // FIXME: Make this handle multiple targets
    mut segment_callback: F, // Make segment_callback mutable
) -> Result<(), pw::Error>
where
    F: FnMut(WhisperSegment) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + 'static,
{
    pipewire::init();

    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    // Create output directory if specified and doesn't exist
    if let Some(output_dir) = &output_dir {
        if !output_dir.exists() {
            fs::create_dir_all(output_dir).expect("Failed to create output directory");
        }
    }

    // 16KHz = inference rate for Whisper
    let inference_rate = 16000;
    let ring_buffer_size = (inference_rate * buffer_seconds) as usize;

    // Create channels for audio samples and transcription segments
    let (sample_sender, sample_receiver) = mpsc::unbounded_channel::<Vec<f32>>(); // Use unbounded channel
    let (segment_sender, mut segment_receiver) = mpsc::unbounded_channel::<WhisperSegment>();

    let data = UserData {
        format: Default::default(),
        sample_sender: sample_sender.clone(),
    };

    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };

    tokio::spawn(async move {
        while let Some(segment) = segment_receiver.recv().await {
            debug!(
                "[{} - {} ({})]: {}",
                segment.start_timestamp,
                segment.end_timestamp,
                segment.first_token_dtw_ts,
                segment.text
            );
            segment_callback(segment).await; // Await the async callback
        }
    });

    let processor = WhisperProcessor::new(
        model_path,
        sample_receiver,
        ring_buffer_size,
        output_dir,
        language,
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

    info!("Connecting to target ID: {}", target_id);
    /* Now connect this stream. We ask that our process function is
     * called in a realtime thread. */
    stream.connect(
        spa::utils::Direction::Input,
        Some(target_id),
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    mainloop.run();
    processor.stop();

    Ok(())
}

#[derive(Debug)]
pub struct TranscriberError(String);

impl std::fmt::Display for TranscriberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TranscriberError {}

pub fn find_target_ids(target: Option<String>) -> Result<Vec<u32>, TranscriberError> {
    if let Some(target) = target {
        if let Ok(direct_id) = target.parse::<u32>() {
            // If the target is a valid number, use it directly
            info!("Using direct target ID: {}", direct_id);
            Ok(vec![direct_id])
        } else {
            // Otherwise use the pattern matching routine
            let pattern_vec = vec![target.clone()];
            pipewire_utils::find_pipewire_ids_by_pattern(pattern_vec)
                .ok_or_else(|| TranscriberError("No matching PipeWire sources found".to_string()))
        }
    } else {
        // If no target specified, use empty pattern to find defaults
        let pattern_vec = vec!["".to_string()];
        pipewire_utils::find_pipewire_ids_by_pattern(pattern_vec)
            .ok_or_else(|| TranscriberError("No matching PipeWire sources found".to_string()))
    }
}
