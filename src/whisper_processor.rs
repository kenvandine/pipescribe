use hound;
use log::{debug, error, info};

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Represents a transcription segment from Whisper
#[derive(Debug, Clone)]
pub struct WhisperSegment {
    pub text: String,
    pub start_timestamp: i64,
    pub end_timestamp: i64,
    pub first_token_dtw_ts: i64,
}

pub struct WhisperProcessor {
    running: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl WhisperProcessor {
    pub fn new(
        model_path: &str,
        sample_receiver: Receiver<Vec<f32>>,
        buffer_size: usize,
        output_dir: Option<PathBuf>,
        language: Option<String>,
        segment_sender: Sender<WhisperSegment>,
    ) -> Self {
        whisper_rs::install_logging_hooks();

        let context_params = WhisperContextParameters::default();
        let ctx = Arc::new(
            WhisperContext::new_with_params(model_path, context_params)
                .expect("failed to load model"),
        );

        // Create a state in the main thread that will be moved to the processing thread
        let state = ctx.create_state().expect("failed to create state");

        // Setup thread termination flag
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Setup processing flag for backpressure
        let processing = Arc::new(AtomicBool::new(false));
        let processing_clone = processing.clone();

        // Setup condition variable for synchronization
        let condition = Arc::new((Mutex::new(false), Condvar::new()));
        let condition_clone = condition.clone();

        // Set up a shared counter for WAV file naming
        let file_counter = Arc::new(Mutex::new(0));

        let mut inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
        inference_params.set_n_threads(
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(1) as i32,
        );
        inference_params.set_translate(true);

        let lang = language.unwrap_or_else(|| "en".to_string());
        inference_params.set_language(Some(&lang));

        inference_params.set_print_special(true);
        inference_params.set_print_progress(false);
        inference_params.set_print_realtime(false);
        inference_params.set_print_timestamps(false);
        inference_params.set_token_timestamps(true);

        // Create processing thread that consumes from the channel
        let thread_handle = Some(thread::spawn(move || {
            let mut state = state;
            let mut buffer_samples: Vec<f32> = Vec::with_capacity(buffer_size);

            while running_clone.load(Ordering::SeqCst) {
                // Collect samples until we have enough for processing
                while buffer_samples.len() < buffer_size {
                    match sample_receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(samples) => {
                            // Add the entire chunk of samples to our buffer
                            buffer_samples.extend_from_slice(&samples);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            // Check if we should continue running
                            if !running_clone.load(Ordering::SeqCst) {
                                break;
                            }
                            continue;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // Channel is closed, exit the thread
                            debug!("Audio sample channel disconnected");
                            return;
                        }
                    }
                }

                // If we don't have enough samples and thread is stopping, exit
                if buffer_samples.len() < buffer_size && !running_clone.load(Ordering::SeqCst) {
                    break;
                }

                // Only process if we have enough samples
                if buffer_samples.len() >= buffer_size {
                    // Set processing flag to provide backpressure
                    processing_clone.store(true, Ordering::SeqCst);

                    debug!(
                        "Running inference on {} accumulated samples",
                        buffer_samples.len()
                    );

                    // Diagnostics: Save audio to WAV file if output directory is specified
                    if let Some(output_dir) = &output_dir {
                        // Get next file number
                        let file_num = {
                            let mut counter = file_counter.lock().unwrap();
                            let num = *counter;
                            *counter += 1;
                            num
                        };

                        let wav_path = output_dir.join(format!("audio_{:04}.wav", file_num));

                        match Self::write_wav_file(&wav_path, &buffer_samples, 16000) {
                            Ok(_) => info!("Saved audio to {}", wav_path.display()),
                            Err(e) => eprintln!("Failed to save WAV file: {}", e),
                        }
                    }

                    let inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                    if let Err(e) = state.full(inference_params, &buffer_samples) {
                        eprintln!("Failed to run model: {}", e);

                        // Reset processing flag even if inference failed
                        processing_clone.store(false, Ordering::SeqCst);

                        // Signal that processing has finished
                        let (lock, cvar) = &*condition_clone;
                        let mut finished = lock.lock().unwrap();
                        *finished = true;
                        cvar.notify_all();

                        // Clear buffer and continue
                        buffer_samples.clear();
                        continue;
                    }

                    // Clear buffer for next batch after processing
                    buffer_samples.clear();

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

                        debug!("[{} - {}]: {}", start_timestamp, end_timestamp, segment);

                        // Send the segment through the channel instead of printing
                        let whisper_segment = WhisperSegment {
                            text: segment,
                            start_timestamp,
                            end_timestamp,
                            first_token_dtw_ts,
                        };

                        if let Err(e) = segment_sender.send(whisper_segment) {
                            error!("Failed to send segment through channel: {}", e);
                        }
                    }

                    // Reset processing flag now that we're done
                    processing_clone.store(false, Ordering::SeqCst);

                    // Signal that processing has finished
                    let (lock, cvar) = &*condition_clone;
                    let mut finished = lock.lock().unwrap();
                    *finished = true;
                    cvar.notify_all();
                }
            }
        }));

        Self {
            running,
            thread_handle,
        }
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

    /// Stop the processing thread and wait for it to complete
    pub fn stop(mut self) {
        self.running.store(false, Ordering::SeqCst);

        if let Some(handle) = self.thread_handle.take() {
            if let Err(e) = handle.join() {
                eprintln!("Error joining processing thread: {:?}", e);
            }
        }
    }
}

impl Drop for WhisperProcessor {
    fn drop(&mut self) {
        // Ensure thread is stopped if processor is dropped without calling stop()
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_utils;
    use env_logger;
    use log::LevelFilter;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn test_whisper_processor_with_jfk_speech() {
        let _ = env_logger::builder()
            .filter_level(LevelFilter::Info)
            .is_test(true)
            .try_init();

        println!("Starting test_whisper_processor_with_jfk_speech");

        let (sample_sender, sample_receiver) = mpsc::channel::<Vec<f32>>();

        let wav_path = Path::new("fixtures/jfk_berlin_address_high_f32le.wav");
        let reader = hound::WavReader::open(wav_path).expect("Could not open test WAV file");
        let spec = reader.spec();

        let raw_samples: Vec<f32> = reader.into_samples().filter_map(Result::ok).collect();
        debug!("Loaded {} raw samples from file", raw_samples.len());

        let samples = audio_utils::preprocess_for_whisper(
            &raw_samples,
            spec.channels as u32,
            spec.sample_rate,
            16000,
        );
        debug!("Preprocessed to {} samples at 16kHz", samples.len());

        // Define expected segments
        let expected_segments = vec![
            "who for so many years",
            "committed Germany to democracy.",
            "and freedom and progress.",
            "and to come here in the company.", // the tiny model variant makes more mistakes.
        ];

        let received_segments = Arc::new(Mutex::new(Vec::new()));
        let received_segments_clone = received_segments.clone();

        let (segment_sender, segment_receiver) = mpsc::channel::<WhisperSegment>();

        // Start thread to receive and track segments
        thread::spawn(move || {
            while let Ok(segment) = segment_receiver.recv() {
                println!(
                    "[{} - {}]: {}",
                    segment.start_timestamp, segment.end_timestamp, segment.text
                );

                let mut segments = received_segments_clone.lock().unwrap();
                segments.push(segment.text.clone());
            }
        });

        println!("Creating whisper processor");
        let processor = WhisperProcessor::new(
            // need to have downloaded this with `./bin/download-ggml-models.sh tiny.en && mv ggml-tiny.en.bin models/`
            "models/ggml-tiny.en.bin",
            sample_receiver,
            16000, // Process after 1 second of audio
            None,
            Some("en".to_string()),
            segment_sender,
        );

        let sample_limit = std::cmp::min(samples.len(), 16000 * 15); // 15 seconds of audio

        // Send samples through the channel in chunks instead of individually
        println!("Sending {} samples to processor", sample_limit);

        const CHUNK_SIZE: usize = 64000;
        for chunk_start in (0..sample_limit).step_by(CHUNK_SIZE) {
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE, sample_limit);
            let chunk = samples[chunk_start..chunk_end].to_vec();

            if let Err(e) = sample_sender.send(chunk) {
                println!("Failed to send sample chunk: {}", e);
                break;
            }
        }

        let start_time = Instant::now();
        let max_wait = Duration::from_secs(20); // Longer timeout to allow processing

        let all_segments_received = |received: &[String]| -> bool {
            expected_segments
                .iter()
                .all(|expected| received.iter().any(|received| received.contains(expected)))
        };

        // Wait for all expected segments or timeout
        let mut success = false;
        while start_time.elapsed() < max_wait {
            {
                let segments = received_segments.lock().unwrap();
                if all_segments_received(&segments) {
                    success = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        processor.stop();

        let final_segments = received_segments.lock().unwrap();
        println!("Received {} segments total:", final_segments.len());
        for (i, seg) in final_segments.iter().enumerate() {
            println!("  {}. {}", i + 1, seg);
        }

        if success {
            assert!(true);
        } else {
            println!("Test failed - not all expected segments were received");
            println!("Expected to receive segments containing:");
            for expected in &expected_segments {
                println!("  - {}", expected);
            }
            assert!(false, "Not all expected segments were received");
        }
    }
}
