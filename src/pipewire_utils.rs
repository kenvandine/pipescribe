use log::{debug, error, info};
use pipewire as pw;
use regex::Regex;
use std::sync::mpsc;

/// Represents a PipeWire application with its ID and properties
pub struct PipewireApplication {
    pub id: u32,
    pub name: String,
    pub media_class: String,
}

/// Lists all available PipeWire audio applications
pub fn list_pipewire_applications() -> Vec<PipewireApplication> {
    let mut applications = Vec::new();

    let mainloop = pw::main_loop::MainLoop::new(None).expect("failed to create mainloop");
    let context = pw::context::Context::new(&mainloop).expect("failed to create context");
    let core = context.connect(None).expect("failed to connect");
    let registry = core.get_registry().expect("failed to get registry");

    let (tx, rx) = mpsc::channel();

    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if let Some(props) = global.props.as_ref() {
                if let Some(app_name) = props.get("application.name") {
                    // Only collect if it has media.class containing "Output" and "Audio"
                    if let Some(media_class) = props.get("media.class") {
                        if media_class.contains("Output") && media_class.contains("Audio") {
                            let app = PipewireApplication {
                                id: global.id,
                                name: app_name.to_string(),
                                media_class: media_class.to_string(),
                            };
                            let _ = tx.send(app);
                        }
                    }
                }
            }
        })
        .global_remove(|_| {})
        .register();

    // Setup core listener to detect sync completion
    let mainloop_ref = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .info(|_| {})
        .done(move |id, seq| {
            debug!("Core sync done for ID: {} seq: {}", id, seq.seq());
            if id == pw::core::PW_ID_CORE {
                mainloop_ref.quit();
            }
        })
        .register();

    // Request sync
    let _ = core.sync(0);
    mainloop.run();

    // Collect all applications
    while let Ok(app) = rx.try_recv() {
        applications.push(app);
    }

    applications
}

pub fn find_pipewire_ids_by_pattern(patterns: Vec<String>) -> Option<Vec<u32>> {
    let regexes: Vec<Regex> = patterns
        .iter()
        .filter_map(|p| {
            if p.is_empty() {
                return None;
            }
            match Regex::new(p) {
                Ok(re) => Some(re),
                Err(e) => {
                    error!("Invalid regex '{}': {}", p, e);
                    None
                }
            }
        })
        .collect();

    if regexes.is_empty() {
        info!("No valid patterns to search for");
        return None;
    }

    let applications = list_pipewire_applications();

    let matching_ids: Vec<u32> = applications
        .iter()
        .filter(|app| regexes.iter().any(|re| re.is_match(&app.name)))
        .map(|app| {
            debug!("MATCHED application ID: {} with name {}", app.id, app.name);
            app.id
        })
        .collect();

    debug!("Found {} matching IDs", matching_ids.len());

    // Return None if no matches were found
    if matching_ids.is_empty() {
        None
    } else {
        Some(matching_ids)
    }
}
