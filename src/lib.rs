// Library entry point for Scribe
// Exports common modules used by both the CLI and tray application

pub mod audio_utils;
pub mod pipewire_utils;
pub mod whisper_processor;

pub use whisper_processor::WhisperProcessor;
pub use whisper_processor::WhisperSegment;
