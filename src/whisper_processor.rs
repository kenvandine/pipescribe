use hound;
use log::{debug, error, info};
use ringbuf::{consumer::Consumer, traits::Observer};

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
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
    pub fn new<T>(
        model_path: &str,
        consumer: T,
        ring_buffer_size: usize,
        output_dir: Option<PathBuf>,
        language: Option<String>,
        segment_sender: Sender<WhisperSegment>,
    ) -> Self
    where
        T: Consumer + Observer<Item = f32> + Send + 'static,
    {
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

        let inference_rate = 16000;

        // Create processing thread that consumes from the ring buffer
        let thread_handle = Some(thread::spawn(move || {
            let mut state = state;
            let mut consumer = consumer;

            while running_clone.load(Ordering::SeqCst) {
                let available_samples = consumer.occupied_len();

                // Only process if we have enough samples
                // (at least 1 second of audio needed by whisper, but we can process more)
                if available_samples >= ring_buffer_size as usize {
                    // Set processing flag to provide backpressure
                    processing_clone.store(true, Ordering::SeqCst);

                    let batch_size = usize::min(available_samples, inference_rate as usize);
                    let mut buffer_samples: Vec<f32> = Vec::with_capacity(batch_size);

                    for _ in 0..available_samples {
                        if let Some(sample) = consumer.try_pop() {
                            buffer_samples.push(sample);
                        } else {
                            error!("Failed to pop sample from ring buffer");
                            break; // Should not happen, but just in case
                        }
                    }

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
                } else {
                    // Sleep longer when we don't have enough samples
                    thread::sleep(Duration::from_millis(100));
                    continue;
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
    use ringbuf::SharedRb;
    use ringbuf::storage::Heap;
    use ringbuf::traits::{Producer, Split};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn test_whisper_processor_with_jfk_speech() {
        // Set up basic logging
        let _ = env_logger::builder()
            .filter_level(LevelFilter::Info)
            .is_test(true)
            .try_init();

        println!("Starting test_whisper_processor_with_jfk_speech");

        // Set up the ring buffer - medium size to not overwhelm memory
        let ring_buffer_size = 48000; // 3 seconds at 16kHz
        let ring_buffer = SharedRb::<Heap<f32>>::new(ring_buffer_size);
        let (mut producer, consumer) = ring_buffer.split();

        // Load test audio file
        let wav_path = Path::new("fixtures/jfk_berlin_address_high_f32le.wav");
        let reader = hound::WavReader::open(wav_path).expect("Could not open test WAV file");
        let spec = reader.spec();
        println!("Test file specs: {:?}", spec);

        let raw_samples: Vec<f32> = reader.into_samples().filter_map(Result::ok).collect();
        println!("Loaded {} raw samples from file", raw_samples.len());

        let samples = audio_utils::preprocess_for_whisper(
            &raw_samples,
            spec.channels as u32,
            spec.sample_rate,
            16000,
        );
        println!("Preprocessed to {} samples at 16kHz", samples.len());

        // Track received segments with atomic flag - simple synchronization
        let received_segment = Arc::new(AtomicBool::new(false));
        let received_segment_clone = received_segment.clone();

        // Create channel for segments
        let (segment_sender, segment_receiver) = mpsc::channel::<WhisperSegment>();
        // Create a vector to store received segments
        // let segments_received = Arc::new(Mutex::new(Vec::new()));
        // let segments_clone = segments_received.clone();

        // Start thread to receive and track segments
        thread::spawn(move || {
            while let Ok(segment) = segment_receiver.recv() {
                println!(
                    "[{} - {}]: {}",
                    segment.start_timestamp, segment.end_timestamp, segment.text
                );

                // Store the segment
                // segments_clone.lock().unwrap().push(segment.clone());

                // Set flag that we've received a segment
                received_segment_clone.store(true, Ordering::SeqCst);
            }
        });

        // Later in the test, after processor.stop(), add:
        // let segments = segments_received.lock().unwrap();
        // assert!(!segments.is_empty(), "No segments were received");

        // Check for expected content
        /*
        let all_content = segments
            .iter()
            .map(|seg| seg.text.clone())
            .collect::<Vec<_>>()
            .join(" ");

        println!("All segments received: {}", all_content);
        */

        // Create processor - use small threshold to process data quickly
        println!("Creating whisper processor");
        let processor = WhisperProcessor::new(
            "models/ggml-base.en.bin",
            consumer,
            16000, // Process after 1 second of audio
            None,
            Some("en".to_string()),
            segment_sender,
        );

        let sample_limit = std::cmp::min(samples.len(), 16000 * 10); // 10 seconds of audio

        for i in 0..sample_limit {
            let mut retries = 0;
            while producer.try_push(samples[i]).is_err() {
                if retries >= 3 {
                    println!("Buffer full after {} retries, skipping sample", retries);
                    break;
                }
                retries += 1;
                thread::sleep(Duration::from_millis(50));
            }
        }

        let start_time = Instant::now();
        let max_wait = Duration::from_secs(15);

        while !received_segment.load(Ordering::SeqCst) {
            if start_time.elapsed() > max_wait {
                println!(
                    "Test timed out after {:?} without receiving segments",
                    max_wait
                );
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        processor.stop();

        if received_segment.load(Ordering::SeqCst) {
            assert!(true);
        } else {
            println!("Test failed - no segments received");
            assert!(false, "No segments were received from the WhisperProcessor");
        }
    }
}
