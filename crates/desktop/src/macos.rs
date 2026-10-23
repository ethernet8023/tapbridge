//! macOS desktop support: Dock policy, the per-user LaunchAgent for start at
//! login, and platform paths. The PC/SC reader socket lives in
//! `pcsc_reader` (shared with Linux).

use std::{io, path::PathBuf};

/// Dock-icon policy: the app is tray-only (no Dock icon) while no visible
/// window is open. `LSUIElement` in Info.plist hides it at launch; the UI
/// tick calls [`dock::sync`] with the count of live windows, which flips the
/// activation policy at runtime. Idempotent: state changes only on a
/// transition. All calls must come from the gpui main thread (AppKit).
#[cfg(target_os = "macos")]
pub mod dock {
    use std::sync::atomic::{AtomicU8, Ordering};

    const UNKNOWN: u8 = 0;
    const HIDDEN: u8 = 1;
    const SHOWN: u8 = 2;

    /// Starts UNKNOWN, not HIDDEN: gpui's `applicationDidFinishLaunching`
    /// forces the Regular policy (a Dock icon) over `LSUIElement`, so the
    /// first sync must always apply the policy, even with zero windows.
    static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

    /// Reports how many visible windows are open; shows/hides the Dock icon
    /// on transitions. The anchor window is excluded (it is never visible).
    pub fn sync(visible_windows: usize) {
        let want = if visible_windows > 0 { SHOWN } else { HIDDEN };
        if STATE.swap(want, Ordering::SeqCst) == want {
            return;
        }
        set_policy(want == SHOWN);
    }

    /// Shows the Dock icon and makes tapbridge the active app, so the window
    /// about to be opened or refreshed lands in front. Call before every
    /// open/refresh: a tray click does not activate the app, and a window
    /// already open behind other apps gets no policy transition to ride on.
    pub fn bring_to_front() {
        STATE.store(SHOWN, Ordering::SeqCst);
        set_policy(true);
    }

    fn set_policy(visible: bool) {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = MainThreadMarker::new() else {
            return; // Not the main thread: AppKit calls are invalid anyway.
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        });
        if visible {
            // Cooperative `activate()` is ignored when the request comes from
            // a status-item menu (no app yields to us); the forcing variant
            // is deprecated but is what actually raises a menu bar app.
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }
}

/// State dir: `~/Library/Application Support/tapbridge` (created lazily).
pub fn state_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Application Support/tapbridge");
    }
    PathBuf::from("tapbridge-state")
}

// ---------------------------------------------------------------
// Start at login: a per-user LaunchAgent plist the app writes itself
// (mirrors the Windows Run-key autostart; no SMAppService entitlements
// needed). Label computer.ethernet.tapbridge, RunAtLoad + KeepAlive off:
// the tray app quits when the user quits it.
// ---------------------------------------------------------------

const LAUNCH_AGENT_LABEL: &str = "computer.ethernet.tapbridge";

fn launch_agent_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

/// The LaunchAgent plist body for the given executable path.
fn launch_agent_plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCH_AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>
"#
    )
}

/// Whether the LaunchAgent exists (start-at-login enabled).
pub fn autostart_is_enabled() -> bool {
    launch_agent_path().is_some_and(|path| path.is_file())
}

/// Enables/disables start at login by writing/removing the LaunchAgent.
pub fn autostart_set_enabled(enabled: bool) -> Result<(), String> {
    let Some(path) = launch_agent_path() else {
        return Err("HOME not set; cannot manage LaunchAgent".into());
    };
    if enabled {
        let exe = std::env::current_exe().map_err(|error| format!("exe path: {error}"))?;
        let dir = path.parent().unwrap_or(&path).to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|error| format!("{error}"))?;
        std::fs::write(&path, launch_agent_plist(&exe.display().to_string()))
            .map_err(|error| format!("{error}"))?;
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{error}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_plist_names_the_label_and_exe() {
        let plist = launch_agent_plist("/usr/local/bin/tapbridge");
        assert!(plist.contains("<string>computer.ethernet.tapbridge</string>"));
        assert!(plist.contains("<string>/usr/local/bin/tapbridge</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn plist_validates_with_plutil_when_available() {
        // Skips silently when plutil is missing (it ships with macOS).
        let Ok(mut output) = std::process::Command::new("plutil")
            .args(["-lint", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return;
        };
        let mut stdin = output.stdin.take().unwrap();
        use std::io::Write as _;
        let _ = stdin.write_all(launch_agent_plist("/usr/local/bin/tapbridge").as_bytes());
        drop(stdin); // plutil parses stdin at EOF; EOF comes from dropping it
        let Ok(finished) = output.wait_with_output() else {
            return;
        };
        assert!(
            finished.status.success(),
            "plutil rejected the LaunchAgent plist: {}",
            String::from_utf8_lossy(&finished.stdout)
        );
    }
}
