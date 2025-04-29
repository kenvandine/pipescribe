use hound;
use log::{debug, error, info};
use ringbuf::{SharedRb, consumer::Consumer, storage::Heap, traits::Observer};

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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
    ) -> Self
    where
        T: Consumer + Observer<Item = f32> + Send + 'static,
    {
        // Create Whisper context and configure parameters
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

        // Set up a shared counter for WAV file naming
        let file_counter = Arc::new(Mutex::new(0));

        // Configure inference parameters
        let mut inference_params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
        inference_params.set_n_threads(
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(1) as i32,
        );
        inference_params.set_translate(true);

        // Set language if provided, otherwise default to English
        let lang = language.unwrap_or_else(|| "en".to_string());
        inference_params.set_language(Some(&lang));

        inference_params.set_print_special(true);
        inference_params.set_print_progress(false);
        inference_params.set_print_realtime(false);
        inference_params.set_print_timestamps(false);
        inference_params.set_token_timestamps(true);

        // Assuming 16000Hz is the inference rate for Whisper
        let inference_rate = 16000;

        // Create processing thread that consumes from the ring buffer
        let thread_handle = Some(thread::spawn(move || {
            let mut state = state;
            let mut consumer = consumer;

            while running_clone.load(Ordering::SeqCst) {
                let available_samples = consumer.occupied_len();

                // Only process if we have a meaningful number of samples
                if available_samples >= ring_buffer_size as usize {
                    // Define how many samples to process at once
                    let batch_size = usize::min(available_samples, inference_rate as usize); // Process up to 1 second of audio
                    let mut buffer_samples: Vec<f32> = Vec::with_capacity(batch_size);

                    // Collect the samples
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

                    // Save audio to WAV file if output directory is specified
                    if let Some(output_dir) = &output_dir {
                        // Get next file number
                        let file_num = {
                            let mut counter = file_counter.lock().unwrap();
                            let num = *counter;
                            *counter += 1;
                            num
                        };

                        // Create WAV file path
                        let wav_path = output_dir.join(format!("audio_{:04}.wav", file_num));

                        // Write WAV file
                        match Self::write_wav_file(&wav_path, &buffer_samples, 16000) {
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

                        // Print the segment to stdout.
                        debug!(
                            "[{} - {} ({})]: {}",
                            start_timestamp, end_timestamp, first_token_dtw_ts, segment
                        );

                        println!("{}", segment);
                    }
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
    use ringbuf::traits::{Producer, RingBuffer, Split};
    use std::fs::File;
    use std::io::BufReader;
    use tempfile::tempdir;

    #[test]
    fn test_whisper_processor_with_jfk_speech() {
        // Create a temporary directory for output files
        let output_dir = tempdir().expect("Failed to create temp directory");

        // Set up the ring buffer
        let ring_buffer_size = 32000; // 2 seconds at 16kHz
        let ring_buffer = SharedRb::<Heap<f32>>::new(ring_buffer_size * 2);
        let (mut producer, consumer) = ring_buffer.split();

        // Path to the test file
        let wav_path = Path::new("fixtures/jfk_berlin_address_high.wav");

        // Read the WAV file
        let reader = hound::WavReader::open(wav_path).expect("Could not open test WAV file");
        let spec = reader.spec();

        println!("Test file specs: {:?}", spec);
        assert_eq!(spec.sample_format, hound::SampleFormat::Float);

        // Get the samples from the WAV file
        let samples: Vec<f32> = reader.into_samples().filter_map(Result::ok).collect();

        // Path to your whisper model - update this to point to your model file
        let model_path = "models/ggml-base.en.bin"; // Adjust this path

        // Start the WhisperProcessor
        let processor = WhisperProcessor::new(
            model_path,
            consumer,
            ring_buffer_size,
            Some(output_dir.path().to_path_buf()),
            Some("en".to_string()),
        );

        // Push samples to the ring buffer
        for sample in samples {
            while producer.is_full() {
                std::thread::sleep(Duration::from_millis(10));
            }
            producer.try_push(sample).expect("Failed to push sample");
        }

        // Give the processor some time to process the audio
        std::thread::sleep(Duration::from_secs(5));

        // Stop the processor
        processor.stop();

        // Verify output files were created
        let files = std::fs::read_dir(output_dir.path())
            .expect("Failed to read output directory")
            .filter_map(Result::ok)
            .collect::<Vec<_>>();

        assert!(!files.is_empty(), "No output files were created");

        // Note: Since the current implementation prints to stdout rather than returning data,
        // we can't directly verify the transcript content in this test.
        // A real test would capture stdout or modify the processor to return/store results.
    }
}
