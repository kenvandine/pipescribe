// Copyright The pipewire-rs Contributors.
// SPDX-License-Identifier: MIT

//! This file is a rustic interpretation of the [PipeWire audio-capture.c example][example]
//!
//! example: https://docs.pipewire.org/audio-capture_8c-example.html

use clap::Parser;
use pipewire as pw;
use pw::{properties::properties, spa};
#[cfg(feature = "v0_3_44")]
use spa::WritableDict;
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use std::convert::TryInto;
use std::mem;
use std::sync::Arc;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use env_logger;
use log::info;

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    cursor_move: bool,
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
}

pub fn main() -> Result<(), pw::Error> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    info!("Starting audio capture application");

    // Log that we're using whisper-rs with log_backend feature enabled
    info!("Using whisper-rs with log_backend feature enabled");

    pw::init();

    let mainloop = pw::main_loop::MainLoop::new(None)?;
    let context = pw::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let data = UserData {
        format: Default::default(),
        cursor_move: false,
    };

    /* Create a simple stream, the simple stream manages the core and remote
     * objects for you if you don't need to deal with them.
     *
     * If you plan to autoconnect your stream, you need to provide at least
     * media, category and role properties.
     *
     * Pass your events and a user_data pointer as the last arguments. This
     * will inform you about the stream state. The most important event
     * you need to listen to is the process event where you need to produce
     * the data.
     */
    #[cfg(not(feature = "v0_3_44"))]
    let props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
    };
    #[cfg(feature = "v0_3_44")]
    let props = {
        let opt = Opt::parse();

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

    let model_path = Opt::parse().model;
    let context_params = WhisperContextParameters::default();
    let mut inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    inference_params.set_n_threads(1);
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
    // Create a state
    let mut state = ctx.create_state().expect("failed to create key");

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

            println!(
                "capturing rate:{} channels:{}",
                user_data.format.rate(),
                user_data.format.channels()
            );
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
                let n_samples = data.chunk().size() / (mem::size_of::<f32>() as u32);

                if let Some(samples_bytes) = data.data() {
                    // Since we requested F32LE format, we should directly interpret as f32
                    let float_samples = unsafe {
                        std::slice::from_raw_parts(
                            samples_bytes.as_ptr() as *const f32,
                            samples_bytes.len() / std::mem::size_of::<f32>(),
                        )
                    };

                    let mono_samples = whisper_rs::convert_stereo_to_mono_audio(float_samples)
                        .expect("Failed to convert samples to mono");

                    /*
                    if user_data.cursor_move {
                        print!("\x1B[{}A", n_channels + 1);
                    }
                    println!("captured {} samples", n_samples / n_channels);
                    for c in 0..n_channels {
                        let mut max: f32 = 0.0;
                        for n in (c..n_samples).step_by(n_channels as usize) {
                            let start = n as usize * mem::size_of::<f32>();
                            let end = start + mem::size_of::<f32>();
                            let chan = &samples_bytes[start..end];
                            let f = f32::from_le_bytes(chan.try_into().unwrap());
                            max = max.max(f.abs());
                        }

                        let peak = ((max * 30.0) as usize).clamp(0, 39);

                        println!(
                            "channel {}: |{:>w1$}{:w2$}| peak:{}",
                            c,
                            "*",
                            "",
                            max,
                            w1 = peak + 1,
                            w2 = 40 - peak
                        );
                    }
                    user_data.cursor_move = true;
                    */

                    // now we can run the model
                    let inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                    state
                        .full(inference_params, &mono_samples)
                        .expect("failed to run model");
                    let num_segments = state
                        .full_n_segments()
                        .expect("failed to get number of segments");
                    for i in 0..num_segments {
                        let segment = state
                            .full_get_segment_text(i)
                            .expect("failed to get segment");
                        let start_timestamp = state
                            .full_get_segment_t0(i)
                            .expect("failed to get start timestamp");
                        let end_timestamp = state
                            .full_get_segment_t1(i)
                            .expect("failed to get end timestamp");

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
                        println!(
                            "[{} - {} ({})]: {}",
                            start_timestamp, end_timestamp, first_token_dtw_ts, segment
                        );

                        // Format the segment information as a string.
                        let line =
                            format!("[{} - {}]: {}\n", start_timestamp, end_timestamp, segment);

                        log::info!("{}", line);
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

    /* Now connect this stream. We ask that our process function is
     * called in a realtime thread. */
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // and wait while we let things run
    mainloop.run();

    Ok(())
}
