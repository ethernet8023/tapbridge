//! File logging to `%LOCALAPPDATA%\tapbridge\logs` with simple rotation.
//! Never logs keys, capabilities, tokens or APDU bytes.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LOG_BYTES: u64 = 1 << 20;
const KEEP_ROTATED: usize = 3;
static LOGGER: OnceLock<Mutex<Option<fs::File>>> = OnceLock::new();

pub fn init(directory: &Path) {
    let _ = fs::create_dir_all(directory);
    let file = open_with_rotation(directory);
    let _ = LOGGER.set(Mutex::new(file));
}

fn open_with_rotation(directory: &Path) -> Option<fs::File> {
    let current = directory.join("tapbridge.log");
    if fs::metadata(&current)
        .map(|meta| meta.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        rotate(directory, &current);
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(current)
        .ok()
}

fn rotate(directory: &Path, current: &Path) {
    for index in (1..KEEP_ROTATED).rev() {
        let from = directory.join(format!("tapbridge.{index}.log"));
        if from.exists() {
            let _ = fs::rename(
                &from,
                directory.join(format!("tapbridge.{}.log", index + 1)),
            );
        }
    }
    let _ = fs::rename(current, directory.join("tapbridge.1.log"));
}

/// Append one timestamped line; failures are ignored (best effort logging).
pub fn log(line: &str) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Hold the lock for the whole write so concurrent lines are serialized,
    // never dropped.
    if let Some(mut guard) = LOGGER.get().and_then(|m| m.lock().ok())
        && let Some(file) = guard.as_mut()
    {
        let _ = writeln!(file, "{stamp} {line}");
        let _ = file.flush();
    }
}

#[macro_export]
macro_rules! app_log {
    ($($arg:tt)*) => { $crate::logging::log(&format!($($arg)*)) };
}

/// Log directory for the app: `%LOCALAPPDATA%\tapbridge\logs`.
pub fn log_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("logs")
}
