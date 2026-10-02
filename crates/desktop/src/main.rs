// Release builds are GUI-subsystem: no console window at login.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! tapbridge: the Windows tray app. Runs the host service, attaches
//! usbip-win2, watches for the Windows Security prompt, wakes the phone via
//! the relay, and shows pairing/status windows through gpui.
mod app_state;
mod assets;
mod autostart;
mod hostname;
mod logging;
mod prompt_watch;
mod single;
mod tray;
mod ui;
mod usbip;
mod wake;
mod win;

use std::path::PathBuf;

fn state_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("tapbridge");
    }
    PathBuf::from("tapbridge-state")
}

fn main() {
    let trace_apdu = std::env::args().any(|arg| arg == "--trace-apdu");
    let dir = state_dir();
    logging::init(&logging::log_dir(&dir));
    logging::log("tapbridge starting");
    let instance = single::acquire();
    app_state::run(dir, trace_apdu, instance);
}
