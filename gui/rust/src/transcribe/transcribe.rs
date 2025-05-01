use flutter_rust_bridge::frb;
use pipescribe::transcriber::transcribe;

#[derive(Clone, Debug)]
pub struct TranscriptionSegment {
    pub text: String,
    pub start_timestamp: f64,
    pub end_timestamp: f64,
}

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

use std::path::PathBuf;

#[frb]
pub fn start_transcribing(
    model_path: String,
    buffer_seconds: u32,
    target: Option<String>,
    output_dir: Option<String>,
    language: Option<String>,
) -> Result<(), String> {
    let output_dir_path = output_dir.map(PathBuf::from);

    match pipescribe::transcriber::find_target_ids(target) {
        Ok(ids) => match transcribe(
            &model_path,
            buffer_seconds,
            output_dir_path,
            language,
            ids[0],
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    }
}
