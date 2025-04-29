use log::{debug, error, info};
use pipewire as pw;
use regex::Regex;
use std::sync::mpsc;

pub fn find_pipewire_ids_by_pattern(patterns: Vec<String>) -> Option<Vec<u32>> {
    let mut matching_ids = Vec::new();

    let mainloop = pw::main_loop::MainLoop::new(None).expect("failed to create mainloop");
    let context = pw::context::Context::new(&mainloop).expect("failed to create context");
    let core = context.connect(None).expect("failed to connect");
    let registry = core.get_registry().expect("failed to get registry");

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

    let (tx, rx) = mpsc::channel();
    let tx_clone = tx.clone();

    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if let Some(props) = global.props.as_ref() {
                if let Some(port_name) = props.get("application.name") {
                    // Only proceed if port.direction is "out"
                    for regex in &regexes {
                        if regex.is_match(port_name) {
                            info!(
                                "Checking port.name: {} for global ID: {}",
                                port_name, global.id
                            );

                            // Enumerate all properties for debugging
                            if let Some(props) = global.props.as_ref() {
                                info!(
                                    "Global ID: {} of type: {} properties:",
                                    global.id, global.type_
                                );
                                for (key, value) in props.iter() {
                                    info!("  {}: {}", key, value);
                                }
                            }

                            // Check if media.class contains both "Output" and "Audio"
                            if let Some(media_class) = props.get("media.class") {
                                if !media_class.contains("Output") || !media_class.contains("Audio")
                                {
                                    info!(
                                        "Skipping global ID: {} because media.class does not match: {}",
                                        global.id,
                                        media_class
                                    );
                                    continue;
                                }
                            } else {
                                // Skip if media.class is not present
                                continue;
                            }

                            info!(
                                "MATCHED {} ID: {} with port.name {}",
                                global.type_, global.id, port_name
                            );
                            let _ = tx.send(global.id);
                            break;
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
                // Registry sync complete, signal to quit the mainloop
                let _ = tx_clone.send(0); // Special value to signal completion
                mainloop_ref.quit();
            }
        })
        .register();

    // Explicitly request a sync to ensure we get a done callback
    let _ = core.sync(0);

    mainloop.run();

    // Collect all IDs that were sent through the channel
    while let Ok(id) = rx.try_recv() {
        info!("Received ID: {}", id);

        if id != 0 {
            // Skip our special signal value
            matching_ids.push(id);
        }
    }

    info!("Found {} matching IDs", matching_ids.len());

    // Return None if no matches were found
    if matching_ids.is_empty() {
        None
    } else {
        Some(matching_ids)
    }
}
