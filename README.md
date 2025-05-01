# Scribe

[![Build and Test](https://github.com/mz2/scribe/actions/workflows/test.yaml/badge.svg)](https://github.com/mz2/scribe/actions/workflows/test.yaml)

Scribe is a real-time audio transcription tool that captures audio from PipeWire sources and transcribes it using the Whisper speech recognition model.

## Features

- Capture audio from any PipeWire source (source detection or manual selection via regex patterns)
- Real-time speech-to-text transcription
- Support for multiple languages

## Requirements

- Rust (cargo)
- PipeWire 
- Whisper model files

## Installation

1. Clone the repository:
   
```bash
git clone https://github.com/mz2/scribe.git
cd scribe
```

2. Download a Whisper model file:
   
```bash
mkdir -p models
./bin/download-ggml-models.sh medium.en
```

(model download script [whisper.cpp](lifted from https://github.com/ggml-org/whisper.cpp/blob/master/models/download-ggml-model.sh)

3. Build the project:

```bash
cargo build --release
```

## Usage

Basic usage:

```bash
scribe --buffer-seconds 5 --model ./models/ggml-medium.en.bin
```

... or to test out a local build:

```bash
cargo run --bin scribe -- --buffer-seconds 5 --model ./models/ggml-medium.en.bin
```

## Tray Application

Scribe also includes a GTK4-based system tray application that displays recently captured and transcribed segments in a graphical interface.

### Usage

To run the tray application:

```bash
cargo run --bin scribe-tray
```

This will launch a window displaying the most recent transcriptions in real-time.