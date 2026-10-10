// Release builds are GUI-subsystem on Windows: no console window at login.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]
//! tapbridge: the desktop app. On Windows it runs the host service, attaches
//! usbip-win2, watches for the Windows Security prompt, wakes the phone via
//! the relay, and shows pairing/status windows through gpui. On macOS the
//! same service stack backs a PC/SC reader (see `macos`), with PIV/cert
//! flows only — WebAuthn is Track B.
#[cfg(target_os = "macos")]
mod mac_login;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod security_key;

mod app_state;
mod assets;
mod autostart;
mod card_tasks;
mod glass;
mod hostname;
mod logging;
#[cfg(windows)]
mod passkeys;
#[cfg(windows)]
mod prompt_watch;
#[cfg(windows)]
mod single;
mod text_input;
mod tray;
mod ui;
#[cfg(windows)]
mod usbip;
mod wake;
#[cfg(windows)]
mod win;

use std::path::PathBuf;

fn state_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("tapbridge");
    }
    #[cfg(target_os = "macos")]
    {
        macos::state_dir()
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        PathBuf::from("tapbridge-state")
    }
}

fn main() {
    // Dev aid: open the windows with sample data. No service, no state, no keychain.
    if std::env::args().any(|arg| arg == "--preview-ui") {
        app_state::preview_ui();
        return;
    }
    let trace_apdu = std::env::args().any(|arg| arg == "--trace-apdu");
    let dir = state_dir();
    logging::init(&logging::log_dir(&dir));
    // A GUI app has no console: panics must land in the log file.
    std::panic::set_hook(Box::new(|info| {
        logging::log(&format!(
            "panic: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        ));
    }));
    logging::log("tapbridge starting");
    #[cfg(windows)]
    let instance = single::acquire(single::Request::from_args(std::env::args().skip(1)));
    #[cfg(not(windows))]
    let instance = app_state::NoopInstance;
    app_state::run(dir, trace_apdu, instance);
}
