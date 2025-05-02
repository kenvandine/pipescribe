use env_logger;
use flutter_rust_bridge::{DartFnFuture, frb};
use pipescribe::transcriber::transcribe;

use log::{error, info};

#[derive(Clone, Debug)]
#[frb(opaque)]
pub struct TranscriptionSegment {
    pub text: String,
    pub start_timestamp: f64,
    pub end_timestamp: f64,
}

// New struct to represent PipeWire applications with all fields
#[derive(Clone, Debug)]
pub struct PipewireApp {
    pub id: u32,
    pub name: String,
    pub media_class: String,
}

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
}

use std::path::PathBuf;

use std::sync::Arc;

#[tokio::main]
pub async fn pipewire_applications() -> Vec<PipewireApp> {
    let applications = pipescribe::pipewire_utils::list_pipewire_applications();
    applications
        .iter()
        .map(|app| PipewireApp {
            id: app.id,
            name: app.name.clone(),
            media_class: app.media_class.clone(),
        })
        .collect()
}

#[tokio::main]
pub async fn start_transcribing(
    model_path: String,
    buffer_seconds: u32,
    target: Option<String>,
    output_dir: Option<String>,
    language: Option<String>,
    segment_callback: impl Fn(TranscriptionSegment) -> DartFnFuture<()> + Send + Sync + 'static,
) -> Result<(), String> {
    let output_dir_path = output_dir.map(PathBuf::from);
    let callback = Arc::new(segment_callback);

    let ids = match pipescribe::transcriber::find_target_ids(target) {
        Ok(ids) => ids,
        Err(e) => return Err(e.to_string()),
    };

    let callback = Arc::clone(&callback);
    match transcribe(
        &model_path,
        buffer_seconds,
        output_dir_path,
        language,
        ids[0], // FIXME: Make this handle multiple targets
        move |segment| {
            let callback_clone = Arc::clone(&callback);
            Box::pin(async move {
                let text = segment.text.clone();
                println!("{}", text);
                callback_clone(TranscriptionSegment {
                    text: text.clone(),
                    start_timestamp: segment.start_timestamp as f64,
                    end_timestamp: segment.end_timestamp as f64,
                })
                .await;
            })
        },
    ) {
        Ok(_) => {
            info!("Transcription completed");
            Ok(())
        }
        Err(e) => {
            error!("Error during transcription: {}", e);
            Err(e.to_string())
        }
    }
}
