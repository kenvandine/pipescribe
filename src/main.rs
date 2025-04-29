// Copyright The pipewire-rs Contributors.
// SPDX-License-Identifier: MIT

//! This file is a rustic interpretation of the [PipeWire audio-capture.c example][example]
//!
//! example: https://docs.pipewire.org/audio-capture_8c-example.html

use clap::Parser;
use pipewire as pw;
use pw::{properties::properties, spa};
use regex::Regex;
use ringbuf::storage::Heap;
use ringbuf::traits::{Observer, Split};
use ringbuf::wrap::caching::Caching;
use ringbuf::{SharedRb, consumer::Consumer, producer::Producer};
#[cfg(feature = "v0_3_44")]
use spa::WritableDict;
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use env_logger;
use log::{error, info};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
// Add hound crate for WAV file handling
use hound;

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
    // Try to find PipeWire IDs matching the target pattern
    let pattern_vec = vec![opt.target.clone().unwrap_or_default()];
    let target_ids = match find_pipewire_ids_by_pattern(pattern_vec) {
        None => {
            eprintln!("Error: No matching PipeWire sources found");
            std::process::exit(1)
        }
        Some(ids) => ids,
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

    #[cfg(not(feature = "v0_3_44"))]
    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };
    #[cfg(feature = "v0_3_44")]
    let props = {
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
        };
        if let Some(target) = opt.target {
            props.insert(*pw::keys::TARGET_OBJECT, target);
        }
        props
    };

    // uncomment if you want to capture from the sink monitor ports
    // props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");

    let model_path = opt.model;
    let context_params = WhisperContextParameters::default();
    let mut inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    inference_params.set_n_threads(10);
    inference_params.set_translate(true);
    inference_params.set_language(Some("en"));
    inference_params.set_print_special(true);
    inference_params.set_print_progress(false);
    inference_params.set_print_realtime(false);
    inference_params.set_print_timestamps(false);
    inference_params.set_token_timestamps(true);

    let ctx = Arc::new(
        WhisperContext::new_with_params(&model_path, context_params).expect("failed to load model"),
    );

    // Create a state in the main thread that will be moved to the processing thread
    let state = ctx.create_state().expect("failed to create state");

    // Setup thread termination flag
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    // Set up a shared counter for WAV file naming
    let file_counter = Arc::new(Mutex::new(0));

    // Pass output directory and file counter to the processing thread
    let output_dir = opt.output_dir.clone();
    let file_counter_clone = file_counter.clone();

    // Create processing thread that consumes from the ring buffer
    let processing_thread = thread::spawn(move || {
        let mut state = state;
        let mut consumer = consumer;

        while running_clone.load(Ordering::SeqCst) {
            let available_samples = consumer.occupied_len();
            // info!("Available samples: {}", available_samples);

            // Only process if we have a meaningful number of samples
            if available_samples >= ring_buffer_size as usize {
                // At least 0.25 seconds
                // Define how many samples to process at once
                let batch_size = usize::min(available_samples, inference_rate as usize); // Process up to 1 second of audio
                let mut buffer_samples = Vec::with_capacity(batch_size);

                // Collect the samples
                for _ in 0..available_samples {
                    if let Some(sample) = consumer.try_pop() {
                        buffer_samples.push(sample);
                    } else {
                        error!("Failed to pop sample from ring buffer");
                        break; // Should not happen, but just in case
                    }
                }

                println!(
                    "Running inference on {} accumulated samples",
                    buffer_samples.len()
                );

                // Save audio to WAV file if output directory is specified
                if let Some(output_dir) = &output_dir {
                    // Get next file number
                    let file_num = {
                        let mut counter = file_counter_clone.lock().unwrap();
                        let num = *counter;
                        *counter += 1;
                        num
                    };

                    // Create WAV file path
                    let wav_path = output_dir.join(format!("audio_{:04}.wav", file_num));

                    // Write WAV file
                    match write_wav_file(&wav_path, &buffer_samples, 16000) {
                        Ok(_) => info!("Saved audio to {}", wav_path.display()),
                        Err(e) => eprintln!("Failed to save WAV file: {}", e),
                    }
                }

                // Run the model
                let inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                if let Err(e) = state.full(inference_params, &buffer_samples) {
                    eprintln!("Failed to run model: {}", e);
                    continue;
                }

                let num_segments = match state.full_n_segments() {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("Failed to get number of segments: {}", e);
                        continue;
                    }
                };

                for i in 0..num_segments {
                    let segment = match state.full_get_segment_text(i) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    let start_timestamp = match state.full_get_segment_t0(i) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    let end_timestamp = match state.full_get_segment_t1(i) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };

                    println!("[{} - {}]: {}", start_timestamp, end_timestamp, segment);

                    let first_token_dtw_ts = if let Ok(token_count) = state.full_n_tokens(i) {
                        if token_count > 0 {
                            if let Ok(token_data) = state.full_get_token_data(i, 0) {
                                token_data.t_dtw
                            } else {
                                -1i64
                            }
                        } else {
                            -1i64
                        }
                    } else {
                        -1i64
                    };

                    // Print the segment to stdout.
                    log::info!(
                        "[{} - {} ({})]: {}",
                        start_timestamp,
                        end_timestamp,
                        first_token_dtw_ts,
                        segment
                    );

                    // Format the segment information as a string.
                    let line = format!("[{} - {}]: {}\n", start_timestamp, end_timestamp, segment);

                    log::info!("{}", line);
                }
            } else {
                // Sleep longer when we don't have enough samples
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            // Short sleep between processing batches
            // thread::sleep(Duration::from_millis(10));
        }
    });

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
        Some(target_ids[0]), // Convert target string to u32 ID if provided
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // and wait while we let things run
    mainloop.run();

    // Signal the processing thread to stop and wait for it
    running.store(false, Ordering::SeqCst);
    if let Err(e) = processing_thread.join() {
        eprintln!("Error joining processing thread: {:?}", e);
    }

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

pub fn find_pipewire_ids_by_pattern(patterns: Vec<String>) -> Option<Vec<u32>> {
    let mut matching_ids = Vec::new();

    // Initialize PipeWire
    let mainloop = pw::main_loop::MainLoop::new(None).expect("failed to create mainloop");
    let context = pw::context::Context::new(&mainloop).expect("failed to create context");
    let core = context.connect(None).expect("failed to connect");
    let registry = core.get_registry().expect("failed to get registry");

    // Compile regexes once
    let regexes: Vec<Regex> = patterns
        .iter()
        .filter_map(|p| {
            if p.is_empty() {
                return None;
            }
            match Regex::new(p) {
                Ok(re) => Some(re),
                Err(e) => {
                    error!("Invalid regex '{}': {}", p, e);
                    None
                }
            }
        })
        .collect();

    // If no valid patterns, return early
    if regexes.is_empty() {
        info!("No valid patterns to search for");
        return None;
    }

    // Set up a channel to communicate IDs from the registry callback
    let (tx, rx) = mpsc::channel();
    let tx_clone = tx.clone();

    // Register a listener for registry objects
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if let Some(props) = global.props.as_ref() {
                // Check if port.name exists and matches any pattern
                if let Some(port_name) = props.get("object.path") {
                    // Only proceed if port.direction is "out"
                    if let Some(port_direction) = props.get("port.direction") {
                        if port_direction == "out" {
                            for regex in &regexes {
                                if regex.is_match(port_name) {
                                    info!(
                                        "Checking port.name: {} for global ID: {}",
                                        port_name, global.id
                                    );

                                    info!(
                                        "MATCHED {} ID: {} with port.name {}",
                                        global.type_, global.id, port_name
                                    );
                                    let _ = tx.send(global.id);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        })
        .global_remove(|_| {})
        .register();

    // Setup core listener to detect sync completion
    let mainloop_ref = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .info(|_| {})
        .done(move |id, seq| {
            info!("Core sync done for ID: {} seq: {}", id, seq.seq());
            if id == pw::core::PW_ID_CORE {
                // Registry sync complete, signal to quit the mainloop
                let _ = tx_clone.send(0); // Special value to signal completion
                mainloop_ref.quit();
            }
        })
        .register();

    // Explicitly request a sync to ensure we get a done callback
    let _ = core.sync(0);

    mainloop.run();

    // Collect all IDs that were sent through the channel
    while let Ok(id) = rx.try_recv() {
        info!("Received ID: {}", id);

        if id != 0 {
            // Skip our special signal value
            matching_ids.push(id);
        }
    }

    info!("Found {} matching IDs", matching_ids.len());

    // Return None if no matches were found
    if matching_ids.is_empty() {
        None
    } else {
        Some(matching_ids)
    }
}
