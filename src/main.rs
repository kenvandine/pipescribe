// Copyright The pipewire-rs Contributors.
// SPDX-License-Identifier: MIT

//! This file is a rustic interpretation of the [PipeWire audio-capture.c example][example]
//!
//! example: https://docs.pipewire.org/audio-capture_8c-example.html

use clap::Parser;

use pipescribe::transcriber::{find_target_ids, transcribe};
use std::path::PathBuf;

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

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let opt = Opt::parse();
    let target_ids = find_target_ids(opt.target)?;

    transcribe(
        &opt.model,
        opt.buffer_seconds,
        opt.output_dir,
        opt.language,
        target_ids[0],
        |segment| {
            Box::pin(async move {
                let text = segment.text.clone();
                println!("{}", text);
            })
        },
    )?;
    Ok(())
}
