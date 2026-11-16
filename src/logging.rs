//! Logging: stderr plus a rotating state-directory log file.

use std::fs::OpenOptions;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Rotate once the existing log exceeds this size.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Initialise tracing. Safe to call once; later calls are ignored.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    match open_log_file() {
        Some(file) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(Mutex::new(file));
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(file_layer)
                .try_init();
        }
        None => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .try_init();
        }
    }
}

fn open_log_file() -> Option<std::fs::File> {
    let path = crate::config::log_path();
    if let Some(dir) = path.parent()
        && let Err(err) = std::fs::create_dir_all(dir)
    {
        eprintln!("cannot create log directory {}: {err}", dir.display());
        return None;
    }
    if let Ok(meta) = std::fs::metadata(&path)
        && meta.len() > MAX_LOG_BYTES
    {
        let rotated = path.with_extension("txt.1");
        if let Err(err) = std::fs::rename(&path, &rotated) {
            eprintln!("cannot rotate log to {}: {err}", rotated.display());
        }
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => Some(file),
        Err(err) => {
            eprintln!("cannot open log {}: {err}", path.display());
            None
        }
    }
}
