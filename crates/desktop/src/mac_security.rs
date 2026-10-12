//! Mac smart card settings: everything in Apple's "Configure macOS for
//! smart card-only authentication" (support.apple.com/101438).
//!
//! - The `com.apple.security.smartcard` keys go into a configuration
//!   profile. macOS no longer lets a program install profiles (`profiles
//!   install` is gone), so tapbridge removes its old profile as root and
//!   opens the new one; the user approves it in System Settings.
//! - `sudo`, `su` and `login` get `pam_smartcard.so` in their PAM files.
//! - SSH: the client offers the card's key (`PKCS11Provider`), and the
//!   server can refuse passwords.
//!
//! Installed through nix-darwin (`programs.tapbridge`), the same settings
//! come from `/etc/tapbridge/config` and the app only shows them.
//!
//! Pure text in, text out where possible, so the generated files and
//! scripts are unit-tested.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::Path;

/// Written by the nix-darwin module; its presence means "managed".
pub const MANAGED_CONFIG: &str = "/etc/tapbridge/config";

pub const PROFILE_ID: &str = "computer.ethernet.tapbridge.smartcard";
const PROFILE_UUID: &str = "6C1F7A52-3D0B-4E36-9C8E-2A6B7F1D9E01";
const PAYLOAD_UUID: &str = "B3E95A07-58C4-4F12-A1D6-0E7C2F4B8A93";
const SMARTCARD_DOMAIN: &str = "com.apple.security.smartcard";

pub const PAM_SUDO: &str = "/etc/pam.d/sudo";
pub const PAM_SU: &str = "/etc/pam.d/su";
pub const PAM_LOGIN: &str = "/etc/pam.d/login";
const PAM_LINE: &str = "auth       sufficient     pam_smartcard.so # tapbridge";
/// `050-`: sshd keeps the first value it reads, so this must sort before
/// macOS's and nix-darwin's `100-` files.
pub const SSH_CLIENT_CONF: &str = "/etc/ssh/ssh_config.d/050-tapbridge.conf";
pub const SSHD_CONF: &str = "/etc/ssh/sshd_config.d/050-tapbridge.conf";
pub const SSH_CLIENT_TEXT: &str = "# Written by tapbridge: offer the smart card's key.\n\
    Host *\n    PKCS11Provider /usr/lib/ssh-keychain.dylib\n";
pub const SSHD_TEXT: &str = "# Written by tapbridge: no passwords over SSH (keys and smart cards only).\n\
    PasswordAuthentication no\nKbdInteractiveAuthentication no\n";

/// Every setting from Apple's article (defined with the UI, which builds
/// on every platform).
pub type SecurityConfig = crate::ui::SecurityOptions;

impl SecurityConfig {
    /// macOS's defaults.
    pub fn macos_default() -> Self {
        Self {
            allow_smart_card: true,
            user_pairing: true,
            ..Self::default()
        }
    }
}

impl SecurityConfig {
    /// True when the smart card keys differ from macOS's defaults, so a
    /// profile is needed.
    pub fn needs_profile(&self) -> bool {
        self.profile_keys() != Self::macos_default().profile_keys()
    }

    fn profile_keys(&self) -> (bool, bool, u8, bool, bool, bool) {
        (
            self.allow_smart_card,
            self.user_pairing,
            self.check_certificate_trust,
            self.one_card_per_user,
            self.lock_on_removal,
            self.enforce_smart_card,
        )
    }
}

/// tapbridge's own options, as set by nix-darwin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppConfig {
    pub start_at_login: bool,
    pub always_present: bool,
}

/// Reads the `key = value` file the nix-darwin module writes. Unknown keys
/// and bad lines are skipped; missing keys keep their defaults.
pub fn parse_config(text: &str) -> (SecurityConfig, AppConfig) {
    let mut security = SecurityConfig::macos_default();
    let mut app = AppConfig::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let flag = match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        let slot = match key.trim() {
            "start_at_login" => &mut app.start_at_login,
            "always_present" => &mut app.always_present,
            "allow_smart_card" => &mut security.allow_smart_card,
            "user_pairing" => &mut security.user_pairing,
            "one_card_per_user" => &mut security.one_card_per_user,
            "lock_on_removal" => &mut security.lock_on_removal,
            "enforce_smart_card" => &mut security.enforce_smart_card,
            "sudo" => &mut security.sudo,
            "su" => &mut security.su,
            "login" => &mut security.login,
            "ssh_client" => &mut security.ssh_client,
            "sshd_cards_only" => &mut security.sshd_cards_only,
            "check_certificate_trust" => {
                if let Ok(level @ 0..=3) = value.parse::<u8>() {
                    security.check_certificate_trust = level;
                }
                continue;
            }
            _ => continue,
        };
        if let Some(flag) = flag {
            *slot = flag;
        }
    }
    (security, app)
}

/// The nix-darwin settings, when tapbridge is installed that way.
pub fn managed() -> Option<(SecurityConfig, AppConfig)> {
    std::fs::read_to_string(MANAGED_CONFIG)
        .ok()
        .map(|text| parse_config(&text))
}

fn plist_bool(value: bool) -> &'static str {
    if value { "<true/>" } else { "<false/>" }
}

/// The configuration profile with the smart card keys.
pub fn profile(config: &SecurityConfig) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>PayloadContent</key>
    <array>
        <dict>
            <key>PayloadDescription</key>
            <string>Smart card settings chosen in tapbridge</string>
            <key>PayloadDisplayName</key>
            <string>Smart card</string>
            <key>PayloadIdentifier</key>
            <string>{PROFILE_ID}.payload</string>
            <key>PayloadType</key>
            <string>{SMARTCARD_DOMAIN}</string>
            <key>PayloadUUID</key>
            <string>{PAYLOAD_UUID}</string>
            <key>PayloadVersion</key>
            <integer>1</integer>
            <key>UserPairing</key>
            {}
            <key>allowSmartCard</key>
            {}
            <key>checkCertificateTrust</key>
            <integer>{}</integer>
            <key>oneCardPerUser</key>
            {}
            <key>tokenRemovalAction</key>
            <integer>{}</integer>
            <key>enforceSmartCard</key>
            {}
        </dict>
    </array>
    <key>PayloadDescription</key>
    <string>Smart card settings chosen in tapbridge. Remove this profile to undo them.</string>
    <key>PayloadDisplayName</key>
    <string>tapbridge smart card settings</string>
    <key>PayloadIdentifier</key>
    <string>{PROFILE_ID}</string>
    <key>PayloadOrganization</key>
    <string>tapbridge</string>
    <key>PayloadRemovalDisallowed</key>
    <false/>
    <key>PayloadScope</key>
    <string>System</string>
    <key>PayloadType</key>
    <string>Configuration</string>
    <key>PayloadUUID</key>
    <string>{PROFILE_UUID}</string>
    <key>PayloadVersion</key>
    <integer>1</integer>
</dict>
</plist>
"#,
        plist_bool(config.user_pairing),
        plist_bool(config.allow_smart_card),
        config.check_certificate_trust.min(3),
        plist_bool(config.one_card_per_user),
        u8::from(config.lock_on_removal),
        plist_bool(config.enforce_smart_card),
    )
}

/// Does this PAM file let a smart card authenticate?
pub fn pam_has_smartcard(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("auth") && line.contains("pam_smartcard.so")
    })
}

/// Shell to turn the card on or off in one PAM file. On: a backup, then
/// our line before the first `auth` line (after `sudo_local`, so Touch ID
/// still comes first). Off: every `pam_smartcard.so` line goes, macOS's
/// own one in `sudo` too.
fn pam_script(file: &str, on: bool) -> String {
    if on {
        format!(
            "if ! grep -q 'pam_smartcard.so' {file}; then \
             cp -p {file} {file}.tapbridge-backup; \
             awk -v l='{PAM_LINE}' '!d && /^auth/ && !/sudo_local/ {{print l; d=1}} {{print}}' {file} > {file}.tapbridge-new \
             && cat {file}.tapbridge-new > {file}; rm -f {file}.tapbridge-new; fi"
        )
    } else {
        format!(
            "if grep -q 'pam_smartcard.so' {file}; then \
             cp -p {file} {file}.tapbridge-backup; \
             grep -v 'pam_smartcard.so' {file} > {file}.tapbridge-new; \
             cat {file}.tapbridge-new > {file}; rm -f {file}.tapbridge-new; fi"
        )
    }
}

fn file_script(path: &str, text: Option<&str>) -> String {
    match text {
        // The text is ours and has no single quotes.
        Some(text) => format!("printf '%s' '{text}' > {path}; chmod 644 {path}"),
        None => format!("rm -f {path}"),
    }
}

/// The root shell script that moves the Mac from `live` to `want`. The
/// profile itself can't be installed by a script; this only removes the
/// old one (when the profile part changes) and the caller opens the new
/// one for the user to approve.
pub fn apply_script(want: &SecurityConfig, live: &SecurityConfig) -> String {
    let mut steps = vec!["set -e".to_owned()];
    if want.profile_keys() != live.profile_keys() {
        steps.push(format!(
            "/usr/bin/profiles remove -identifier {PROFILE_ID} >/dev/null 2>&1 || true"
        ));
    }
    for (file, on, was) in [
        (PAM_SUDO, want.sudo, live.sudo),
        (PAM_SU, want.su, live.su),
        (PAM_LOGIN, want.login, live.login),
    ] {
        if on != was {
            steps.push(pam_script(file, on));
        }
    }
    if want.ssh_client != live.ssh_client {
        steps.push(file_script(
            SSH_CLIENT_CONF,
            want.ssh_client.then_some(SSH_CLIENT_TEXT),
        ));
    }
    if want.sshd_cards_only != live.sshd_cards_only {
        steps.push(file_script(
            SSHD_CONF,
            want.sshd_cards_only.then_some(SSHD_TEXT),
        ));
        steps.push(
            "launchctl kickstart -k system/com.openssh.sshd >/dev/null 2>&1 || true".to_owned(),
        );
    }
    steps.join("\n")
}

/// Wraps a shell script in the AppleScript that runs it as admin.
pub fn admin_applescript(script: &str) -> String {
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{escaped}\" with administrator privileges")
}

/// Reads one smart card key: the profile's value (Managed Preferences),
/// else a `defaults write` in /Library/Preferences, else `None`.
fn read_smartcard_key(key: &str) -> Option<String> {
    for domain in [
        "/Library/Managed Preferences/com.apple.security.smartcard",
        "/Library/Preferences/com.apple.security.smartcard",
    ] {
        let output = std::process::Command::new("/usr/bin/defaults")
            .args(["read", domain, key])
            .output()
            .ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
    }
    None
}

/// What the Mac is set to now. PAM and SSH come from their files; the
/// smart card keys from what macOS reads.
pub fn read_live() -> SecurityConfig {
    let flag = |key: &str, default: bool| {
        read_smartcard_key(key).map_or(default, |value| value == "1" || value == "true")
    };
    let pam = |path: &str| std::fs::read_to_string(path).is_ok_and(|text| pam_has_smartcard(&text));
    SecurityConfig {
        allow_smart_card: flag("allowSmartCard", true),
        user_pairing: flag("UserPairing", true),
        check_certificate_trust: read_smartcard_key("checkCertificateTrust")
            .and_then(|value| value.parse().ok())
            .filter(|level| *level <= 3)
            .unwrap_or(0),
        one_card_per_user: flag("oneCardPerUser", false),
        lock_on_removal: read_smartcard_key("tokenRemovalAction").as_deref() == Some("1"),
        enforce_smart_card: flag("enforceSmartCard", false),
        sudo: pam(PAM_SUDO),
        su: pam(PAM_SU),
        login: pam(PAM_LOGIN),
        ssh_client: Path::new(SSH_CLIENT_CONF).is_file(),
        sshd_cards_only: Path::new(SSHD_CONF).is_file(),
    }
}

/// How applying went.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Everything is in place.
    Done,
    /// The profile is waiting for the user's approval in System Settings.
    ApproveProfile,
    Failed(String),
}

/// Applies `want`: one admin prompt for the files and the old profile,
/// then the new profile opens in System Settings. `profile_dir` is where
/// the profile file is written.
pub fn apply(want: &SecurityConfig, profile_dir: &Path) -> Applied {
    let live = read_live();
    let script = apply_script(want, &live);
    if script != "set -e" {
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", &admin_applescript(&script)])
            .output();
        match output {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stderr);
                return Applied::Failed(if text.contains("-128") {
                    "Canceled.".to_owned()
                } else {
                    text.trim().chars().take(200).collect()
                });
            }
            Err(error) => return Applied::Failed(error.to_string()),
        }
    }
    let now = read_live();
    if !want.needs_profile() || now == *want {
        return Applied::Done;
    }
    // Only the profile part is left to do.
    let path = profile_dir.join("tapbridge-smartcard.mobileconfig");
    if let Err(error) = std::fs::write(&path, profile(want)) {
        return Applied::Failed(format!("Couldn't write the profile: {error}"));
    }
    let opened = std::process::Command::new("/usr/bin/open")
        .arg(&path)
        .status()
        .is_ok_and(|status| status.success());
    // Opening a profile only "downloads" it; send the user to the pane
    // where it's installed.
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.Profiles-Settings.extension")
        .status();
    if opened {
        Applied::ApproveProfile
    } else {
        Applied::Failed("Couldn't open the profile.".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_nix_darwin_file() {
        let text = "# managed by nix-darwin\nstart_at_login = true\nalways_present=true\n\
                    enforce_smart_card = true\ncheck_certificate_trust = 2\nsudo = true\n\
                    check_certificate_trust = 9\nbogus = true\nsu = maybe\n";
        let (security, app) = parse_config(text);
        assert!(app.start_at_login && app.always_present);
        assert!(security.enforce_smart_card && security.sudo);
        assert_eq!(security.check_certificate_trust, 2, "9 is ignored");
        assert!(!security.su, "bad values keep the default");
        assert!(security.allow_smart_card, "missing keys keep the default");
    }

    #[test]
    fn profile_carries_every_key_and_validates() {
        let config = SecurityConfig {
            enforce_smart_card: true,
            user_pairing: false,
            check_certificate_trust: 3,
            lock_on_removal: true,
            ..SecurityConfig::macos_default()
        };
        assert!(config.needs_profile());
        assert!(!SecurityConfig::macos_default().needs_profile());
        assert!(
            !SecurityConfig {
                sudo: true,
                ..SecurityConfig::macos_default()
            }
            .needs_profile(),
            "PAM settings need no profile"
        );
        let text = profile(&config);
        for needle in [
            "<key>enforceSmartCard</key>\n            <true/>",
            "<key>UserPairing</key>\n            <false/>",
            "<key>checkCertificateTrust</key>\n            <integer>3</integer>",
            "<key>tokenRemovalAction</key>\n            <integer>1</integer>",
            PROFILE_ID,
        ] {
            assert!(text.contains(needle), "missing {needle}");
        }
        let path = std::env::temp_dir().join(format!("tb-profile-{}.plist", std::process::id()));
        std::fs::write(&path, &text).unwrap();
        if let Ok(output) = std::process::Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .output()
        {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn script_only_touches_what_changed() {
        let live = SecurityConfig::macos_default();
        assert_eq!(apply_script(&live, &live), "set -e");
        let want = SecurityConfig {
            sudo: true,
            sshd_cards_only: true,
            ..live
        };
        let script = apply_script(&want, &live);
        assert!(script.contains(PAM_SUDO));
        assert!(!script.contains(&format!("{PAM_SU} ")) && !script.contains(PAM_LOGIN));
        assert!(script.contains(SSHD_CONF) && script.contains("kickstart"));
        assert!(!script.contains("profiles remove"), "profile unchanged");
        let want = SecurityConfig {
            enforce_smart_card: true,
            ..live
        };
        assert!(apply_script(&want, &live).contains("profiles remove"));
    }

    /// Runs the generated PAM edits on copies of macOS 26's real files.
    #[test]
    fn pam_edits_insert_after_sudo_local_and_remove_cleanly() {
        let dir = std::env::temp_dir().join(format!("tb-pam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sudo = dir.join("sudo");
        let su = dir.join("su");
        std::fs::write(
            &sudo,
            "# sudo: auth account password session\nauth       include        sudo_local\n\
             auth       required       pam_opendirectory.so\naccount    required       pam_permit.so\n",
        )
        .unwrap();
        std::fs::write(
            &su,
            "# su: auth account session\nauth       sufficient     pam_rootok.so \n\
             auth       required       pam_opendirectory.so\n",
        )
        .unwrap();
        let run = |script: String| {
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", &script])
                .status()
                .unwrap();
            assert!(status.success());
        };
        let sudo_path = sudo.to_str().unwrap();
        run(pam_script(sudo_path, true));
        run(pam_script(sudo_path, true)); // Idempotent.
        let text = std::fs::read_to_string(&sudo).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "auth       include        sudo_local");
        assert_eq!(lines[2], PAM_LINE);
        assert_eq!(text.matches("pam_smartcard").count(), 1);
        assert!(pam_has_smartcard(&text));
        assert!(dir.join("sudo.tapbridge-backup").is_file());

        run(pam_script(su.to_str().unwrap(), true));
        let text = std::fs::read_to_string(&su).unwrap();
        assert_eq!(text.lines().nth(1), Some(PAM_LINE), "before rootok");

        run(pam_script(sudo_path, false));
        let text = std::fs::read_to_string(&sudo).unwrap();
        assert!(!pam_has_smartcard(&text));
        assert!(text.contains("pam_opendirectory.so"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn applescript_escapes_quotes() {
        let script = admin_applescript("echo \"a\\b\"");
        assert_eq!(
            script,
            "do shell script \"echo \\\"a\\\\b\\\"\" with administrator privileges"
        );
    }
}
