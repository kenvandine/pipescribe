# Pipescribe

[![Build and Test](https://github.com/mz2/pipescribe/actions/workflows/test.yaml/badge.svg)](https://github.com/mz2/pipescribe/actions/workflows/test.yaml)

Pipescribe is a real-time audio transcription tool that captures audio from PipeWire sources and transcribes it using the Whisper speech recognition model.

## Features

- Capture audio from any PipeWire source (source detection or manual selection via regex patterns)
- Real-time speech-to-text transcription
- Support for multiple languages

## Requirements

- Rust (cargo)
- PipeWire 
- Whisper model files


## Installation via `cargo`

You can install Pipescribe directly using Cargo:

```bash
# Basic installation
cargo install pipescribe

# Installation with CUDA support (if you have NVIDIA GPU and CUDA installed)
cargo install pipescribe --features cuda
```

When installing with CUDA support, ensure you have the CUDA toolkit installed on your system.

After installation, you'll still need to download a Whisper model file as described with the local build based installation steps below.


## Installation by building from source locally

1. Clone the repository:
   
```bash
git clone https://github.com/mz2/pipescribe.git
cd pipescribe
```

2. Download a Whisper model file:
   
```bash
mkdir -p models
./bin/download-ggml-models.sh medium.en
```

(model download script [whisper.cpp](lifted from https://github.com/ggml-org/whisper.cpp/blob/master/models/download-ggml-model.sh)

3. Install prerequisites

On Ubuntu 24.04 for example:

```bash
sudo apt-get install -y libpipewire-0.3-dev build-essential
```

4. Build the project:

```bash
cargo build --release --features cuda
```

## Usage

Basic usage:

```bash
pipescribe --buffer-seconds 5 --model ./models/ggml-medium.en.bin
```

... or to test out a local build:

```bash
cargo run --bin pipescribe --features cuda -- --buffer-seconds 5 --model ./models/ggml-medium.en.bin
```
