//! Windows: the accessory interface needs Microsoft's inbox WinUSB driver
//! (and a composite accessory needs the inbox usbccgp parent driver). We
//! bind them with `accessory-driver.ps1`, ported from android-nfc-bridge
//! (MIT): inbox `usb.inf`/`winusb.inf` only, no custom INF or certificate,
//! scoped to the one Google accessory device, elevated through UAC.
//!
//! The user is asked once per run with a plain dialog, not a terminal
//! prompt. Declining (or a failed setup) stops USB on this run; the
//! network path is unaffected.
//!
//! UNTESTED on Windows hardware ("fix Windows later").

use std::{
    io,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Output},
};

use nusb::DeviceInfo;

use crate::logging;

const SCRIPT: &str = include_str!("accessory-driver.ps1");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default, PartialEq, Eq)]
enum Consent {
    #[default]
    Unasked,
    Granted,
    /// Declined, or setup failed: don't ask again this run.
    Off,
}

#[derive(Default)]
pub struct DriverSetup {
    consent: Consent,
}

impl DriverSetup {
    /// Whether the accessory `info` can be opened now. `false` while the
    /// driver is missing (setup declined/failed) or was just rebound (the
    /// device re-enumerates; the watcher retries the new one).
    pub fn ready(&mut self, info: &DeviceInfo) -> bool {
        if self.consent == Consent::Off {
            return false;
        }
        let product = format!("{:04x}", info.product_id());
        let serial = info.serial_number().unwrap_or("").to_owned();
        let run = |mode: &str| run_script(mode, &product, &serial);
        match run("Check") {
            Ok(output) if output.status.success() => return true,
            Ok(output) if output.status.code() == Some(2) => {}
            Ok(output) => {
                logging::log(&format!(
                    "usb: accessory driver check failed: {}",
                    script_error(&output)
                ));
                self.consent = Consent::Off;
                return false;
            }
            Err(error) => {
                logging::log(&format!("usb: accessory driver check unavailable: {error}"));
                self.consent = Consent::Off;
                return false;
            }
        }
        if self.consent == Consent::Unasked {
            self.consent = if ask() {
                Consent::Granted
            } else {
                Consent::Off
            };
            if self.consent == Consent::Off {
                logging::log("usb: accessory driver setup declined");
                return false;
            }
        }
        match run("Prepare") {
            Ok(output) if output.status.success() => {
                logging::log("usb: accessory driver configured; waiting for the phone to reappear");
            }
            Ok(output) => {
                logging::log(&format!(
                    "usb: accessory driver setup failed or UAC cancelled: {}",
                    script_error(&output)
                ));
                self.consent = Consent::Off;
            }
            Err(error) => {
                logging::log(&format!("usb: accessory driver setup unavailable: {error}"));
                self.consent = Consent::Off;
            }
        }
        false
    }
}

fn work_dir() -> PathBuf {
    std::env::temp_dir().join(format!("tapbridge-usb-{}", std::process::id()))
}

fn run_script(mode: &str, product: &str, serial: &str) -> io::Result<Output> {
    let directory = work_dir();
    std::fs::create_dir_all(&directory)?;
    let script = directory.join("accessory-driver.ps1");
    std::fs::write(&script, SCRIPT)?;
    let _ = std::fs::remove_file(directory.join("accessory-error.txt"));
    let root =
        std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is missing"))?;
    let powershell = PathBuf::from(root).join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .args(["-Mode", mode, "-Product", product, "-Serial", serial])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
}

fn script_error(output: &Output) -> String {
    let detail =
        std::fs::read_to_string(work_dir().join("accessory-error.txt")).unwrap_or_default();
    format!("exit {:?} {}", output.status.code(), detail.trim())
}

/// The one-time question. Yes → UAC prompt next.
fn ask() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW,
    };
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let title = wide("tapbridge: USB cable");
    let text = wide(
        "Your phone is connected by USB. To use tapbridge over the cable, \
         Windows needs its built-in USB drivers (WinUSB) set up for the \
         phone's accessory connection.\n\n\
         This uses only drivers that ship with Windows, changes only this \
         phone's accessory connection (ADB and file transfer are kept), and \
         asks for administrator approval next. The phone may reconnect.\n\n\
         Set it up now?",
    );
    // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call;
    // no owner window.
    let answer = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND,
        )
    };
    answer == IDYES
}
