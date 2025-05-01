//! Audio processing utility functions

/// Convert stereo audio samples to mono by averaging the channels
pub fn convert_stereo_to_mono(stereo_samples: &[f32]) -> Vec<f32> {
    let mut mono = Vec::with_capacity(stereo_samples.len() / 2);
    for chunk in stereo_samples.chunks(2) {
        if chunk.len() == 2 {
            mono.push((chunk[0] + chunk[1]) / 2.0); // Average the stereo channels
        }
    }
    mono
}

/// Resample audio to a target sample rate using linear interpolation
pub fn resample_with_linear_interpolation(
    samples: &[f32],
    src_sample_rate: u32,
    target_sample_rate: u32,
) -> Vec<f32> {
    if src_sample_rate == target_sample_rate {
        return samples.to_vec();
    }

    let src_rate = src_sample_rate as f64;
    let target_rate = target_sample_rate as f64;
    let ratio = target_rate / src_rate;

    // Simple linear interpolation resampling
    let new_len = (samples.len() as f64 * ratio).round() as usize;
    let mut resampled = Vec::with_capacity(new_len);

    for i in 0..new_len {
        let src_idx = (i as f64 / ratio) as usize;
        if src_idx < samples.len() {
            resampled.push(samples[src_idx]);
        }
    }

    resampled
}

/**
 * Preprocess audio samples by converting stereo to mono and resampling
 */
pub fn preprocess_for_whisper(
    samples: &[f32],
    channels: u32,
    sample_rate: u32,
    target_rate: u32,
) -> Vec<f32> {
    // First convert to mono if needed
    let mono_samples = if channels == 2 {
        whisper_rs::convert_stereo_to_mono_audio(samples).unwrap()
    } else {
        samples.to_vec()
    };

    // Then resample if needed
    resample_with_linear_interpolation(&mono_samples, sample_rate, target_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stereo_to_mono() {
        let stereo = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let mono = convert_stereo_to_mono(&stereo);
        let expected = vec![0.15, 0.35, 0.55];
        assert_eq!(mono.len(), expected.len());
        for (a, b) in mono.iter().zip(expected.iter()) {
            assert!((a - b).abs() < 1e-5, "Values differ: {} vs {}", a, b);
        }
    }

    #[test]
    fn test_resample() {
        let samples = vec![0.1, 0.2, 0.3, 0.4];

        // Double the sample rate (should halve the samples)
        let resampled = resample_with_linear_interpolation(&samples, 8000, 16000);
        assert_eq!(resampled.len(), 8);

        // Half the sample rate (should double the samples)
        let resampled = resample_with_linear_interpolation(&samples, 16000, 8000);
        assert_eq!(resampled.len(), 2);
    }
}
