//! Linux desktop support: XDG paths and start at login.
//!
//! Start at login is an XDG autostart entry the app writes itself. Under
//! the Nix modules a systemd user unit starts the app instead (it sets
//! `TAPBRIDGE_MANAGED=1`); then the app must not write a second entry or
//! two copies would start.

use std::{io, path::PathBuf};

/// Set by the NixOS/Home Manager units that start the app themselves.
pub const MANAGED_ENV: &str = "TAPBRIDGE_MANAGED";

pub fn managed() -> bool {
    std::env::var_os(MANAGED_ENV).is_some_and(|value| value == "1")
}

/// `$XDG_STATE_HOME/tapbridge`, else `~/.local/state/tapbridge`.
pub fn state_dir() -> PathBuf {
    state_dir_from(
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    )
}

fn state_dir_from(
    xdg_state: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(dir) = xdg_state.filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir).join("tapbridge");
    }
    match home {
        Some(home) => PathBuf::from(home).join(".local/state/tapbridge"),
        None => PathBuf::from("tapbridge-state"),
    }
}

fn autostart_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("autostart/computer.ethernet.tapbridge.desktop"))
}

/// The autostart entry body for the given executable.
fn desktop_entry(exe: &str) -> String {
    // Exec values are shell-like: quote the path and escape what the spec
    // says to (backslash, quote, backtick, dollar).
    let quoted: String = exe
        .chars()
        .flat_map(|c| match c {
            '\\' | '"' | '`' | '$' => vec!['\\', c],
            _ => vec![c],
        })
        .collect();
    format!(
        "[Desktop Entry]\nType=Application\nName=tapbridge\n\
         Comment=Use your phone as the NFC reader for your security key\n\
         Exec=\"{quoted}\"\nIcon=tapbridge\nTerminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// The Nix modules say whether the unit starts at login.
pub const MANAGED_START_ENV: &str = "TAPBRIDGE_START_AT_LOGIN";

pub fn autostart_is_enabled() -> bool {
    if managed() {
        return std::env::var_os(MANAGED_START_ENV).is_none_or(|value| value != "0");
    }
    autostart_path().is_some_and(|path| path.is_file())
}

pub fn autostart_set_enabled(enabled: bool) -> Result<(), String> {
    if managed() {
        return Err("start at login is managed by your Nix configuration".into());
    }
    let Some(path) = autostart_path() else {
        return Err("HOME not set; cannot manage the autostart entry".into());
    };
    if enabled {
        let exe = std::env::current_exe().map_err(|error| format!("exe path: {error}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        std::fs::write(&path, desktop_entry(&exe.display().to_string()))
            .map_err(|error| error.to_string())?;
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn state_dir_prefers_xdg_then_home() {
        assert_eq!(
            state_dir_from(Some(OsString::from("/x/state")), Some(OsString::from("/h"))),
            PathBuf::from("/x/state/tapbridge")
        );
        assert_eq!(
            state_dir_from(Some(OsString::new()), Some(OsString::from("/h"))),
            PathBuf::from("/h/.local/state/tapbridge")
        );
        assert_eq!(state_dir_from(None, None), PathBuf::from("tapbridge-state"));
    }

    #[test]
    fn desktop_entry_quotes_and_escapes_the_exe() {
        let entry = desktop_entry("/opt/my \"app\"/$tapbridge");
        assert!(entry.contains("Exec=\"/opt/my \\\"app\\\"/\\$tapbridge\"\n"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
    }
}
