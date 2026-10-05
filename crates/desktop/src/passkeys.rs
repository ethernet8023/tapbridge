//! Which options the Windows passkey dialog offers, and switches for the ones
//! Windows lets us change. "Security key" (tapbridge) is never touched.
//!
//! - This Windows device (Windows Hello passkeys): the per-user toggle behind
//!   Settings > Accounts > Passkeys > Advanced options,
//!   `HKCU\Software\Microsoft\Passkeys\LocalToggleStatus`. No admin needed.
//! - iPhone, iPad, or Android device (hybrid): Windows has no switch. Hybrid
//!   needs the Bluetooth services 0xFFF9/0xFFFD (Microsoft's "Passkeys in
//!   Bluetooth-restricted environments" doc), so blocking applies the
//!   Bluetooth Policy CSP `ServicesAllowedList` with every common service
//!   except those two. The MDM WMI bridge only accepts SYSTEM, so the app
//!   elevates (UAC) and runs a one-shot SYSTEM task whose script is passed
//!   inline (no script file anyone could replace).
//! - Third-party passkey managers: listed from
//!   `HKLM\SOFTWARE\Microsoft\FIDO\<user SID>\Plugins`; switched in Windows
//!   Settings (their stored values are not documented).

use crate::win::wide;
use std::{io, os::windows::process::CommandExt, process::Command};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError, WAIT_OBJECT_0},
    System::{
        Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, RRF_RT_REG_DWORD,
            RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
            RegSetKeyValueW,
        },
        Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
    },
    UI::{
        Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_HIDE,
    },
};

const PASSKEYS_KEY: &str = "Software\\Microsoft\\Passkeys";
const BLUETOOTH_POLICY_KEY: &str = "SOFTWARE\\Microsoft\\PolicyManager\\current\\device\\Bluetooth";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// FIDO hybrid (caBLE) and FIDO service UUIDs: left out of the allow-list.
const FIDO_SERVICES: [&str; 2] = ["FFF9", "FFFD"];
/// Bluetooth services a PC normally uses: SDP, serial, OBEX, headset and
/// hands-free, A2DP, AVRCP, PAN, HID, PnP info, and the common LE services
/// (GAP, GATT, device info, battery, HID-over-GATT, LE Audio).
const ALLOWED_SERVICES: [&str; 33] = [
    "1000", "1101", "1105", "1106", "1108", "110A", "110B", "110C", "110D", "110E", "110F", "1112",
    "1115", "1116", "111E", "111F", "1124", "1200", "1203", "1800", "1801", "180A", "180F", "1812",
    "1844", "184D", "184E", "184F", "1850", "1853", "1854", "1855", "1858",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phone {
    /// No Bluetooth service restriction.
    Allowed,
    /// A service allow-list without the FIDO services is in force.
    Blocked,
    /// Someone else's allow-list that still includes FIDO: left alone.
    ManagedElsewhere,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub windows_hello: bool,
    pub phone: Phone,
    pub providers: Vec<Provider>,
}

pub fn read() -> Options {
    Options {
        // Absent means Windows' default: on.
        windows_hello: read_dword(HKEY_CURRENT_USER, PASSKEYS_KEY, "LocalToggleStatus") != Some(0),
        phone: classify_phone(
            read_string(
                HKEY_LOCAL_MACHINE,
                BLUETOOTH_POLICY_KEY,
                "ServicesAllowedList",
            )
            .as_deref(),
        ),
        providers: read_providers(),
    }
}

fn classify_phone(allow_list: Option<&str>) -> Phone {
    let Some(list) = allow_list.map(str::trim).filter(|list| !list.is_empty()) else {
        return Phone::Allowed;
    };
    let upper = list.to_ascii_uppercase();
    if FIDO_SERVICES
        .iter()
        .any(|id| upper.contains(&format!("0000{id}-")))
    {
        Phone::ManagedElsewhere
    } else {
        Phone::Blocked
    }
}

/// Turns Windows Hello passkeys ("This Windows device") on or off.
pub fn set_windows_hello(enabled: bool) -> io::Result<()> {
    write_dword(
        HKEY_CURRENT_USER,
        PASSKEYS_KEY,
        "LocalToggleStatus",
        u32::from(enabled),
    )?;
    write_dword(
        HKEY_CURRENT_USER,
        PASSKEYS_KEY,
        "LocalToggleEnabledByUser",
        1,
    )
}

/// Blocks or allows phone (hybrid) sign-in. Shows one UAC prompt.
pub fn set_phone(allowed: bool) -> Result<(), String> {
    run_as_system(&bluetooth_policy_script(!allowed))
}

/// Opens Settings > Accounts > Passkeys, where Windows switches providers.
pub fn open_windows_passkey_settings() {
    let _ = Command::new("explorer.exe")
        .arg("ms-settings:savedpasskeys")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

fn bluetooth_policy_script(block: bool) -> String {
    let allowed = ALLOWED_SERVICES
        .iter()
        .map(|id| format!("{{0000{id}-0000-1000-8000-00805F9B34FB}}"))
        .collect::<Vec<_>>()
        .join(";");
    let change = if block {
        format!(
            "if ($c) {{ $c.ServicesAllowedList = '{allowed}'; Set-CimInstance -CimInstance $c }} \
             else {{ New-CimInstance -Namespace $ns -ClassName $cl -Property @{{ ParentID = './Vendor/MSFT/Policy/Config'; InstanceID = 'Bluetooth'; ServicesAllowedList = '{allowed}' }} | Out-Null }}"
        )
    } else {
        "if ($c) { Remove-CimInstance -CimInstance $c }".to_owned()
    };
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $ns = 'root\\cimv2\\mdm\\dmmap'; $cl = 'MDM_Policy_Config01_Bluetooth02'\n\
         $c = Get-CimInstance -Namespace $ns -ClassName $cl -Filter \"ParentID='./Vendor/MSFT/Policy/Config' and InstanceID='Bluetooth'\" -ErrorAction SilentlyContinue\n\
         {change}\n"
    )
}

/// Runs `script` as SYSTEM: elevates once (UAC), registers a one-shot task
/// with the script inline, runs it, waits, removes it.
fn run_as_system(script: &str) -> Result<(), String> {
    let launcher = format!(
        "$ErrorActionPreference = 'Stop'\n\
         $task = 'tapbridge-passkey-options'\n\
         $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -NonInteractive -EncodedCommand {inner}'\n\
         $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest\n\
         Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Force | Out-Null\n\
         Start-ScheduledTask -TaskName $task\n\
         for ($i = 0; $i -lt 120; $i++) {{ Start-Sleep -Milliseconds 250; if ((Get-ScheduledTask -TaskName $task).State -ne 'Running') {{ break }} }}\n\
         $result = (Get-ScheduledTaskInfo -TaskName $task).LastTaskResult\n\
         Unregister-ScheduledTask -TaskName $task -Confirm:$false\n\
         exit $result\n",
        inner = encode_command(script)
    );
    let verb = wide("runas");
    let file = wide("powershell.exe");
    let parameters = wide(&format!(
        "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
        encode_command(&launcher)
    ));
    // SAFETY: all strings are NUL-terminated and outlive the call; the process
    // handle is closed on every path.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.nShow = SW_HIDE;
        if ShellExecuteExW(&mut info) == 0 {
            return Err(if GetLastError() == ERROR_CANCELLED {
                "Windows did not grant administrator access".into()
            } else {
                format!(
                    "could not start the elevated helper ({})",
                    io::Error::last_os_error()
                )
            });
        }
        if info.hProcess.is_null() {
            return Err("elevated helper did not start".into());
        }
        let waited = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if waited != WAIT_OBJECT_0 || code != 0 {
            return Err(format!("the SYSTEM policy step failed (exit code {code})"));
        }
    }
    Ok(())
}

/// `powershell -EncodedCommand` wants base64 of UTF-16LE.
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() {
                ALPHABET[(n >> shift) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

fn read_providers() -> Vec<Provider> {
    let Some(sid) = current_user_sid() else {
        return Vec::new();
    };
    let path = format!("SOFTWARE\\Microsoft\\FIDO\\{sid}\\Plugins");
    let mut providers = Vec::new();
    // SAFETY: NUL-terminated names; the key is closed before returning.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(&path).as_ptr(),
            0,
            KEY_READ,
            &mut key,
        ) != 0
        {
            return providers;
        }
        let mut index = 0;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let status = RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status != 0 {
                break;
            }
            let child = format!(
                "{path}\\{}",
                String::from_utf16_lossy(&name[..len as usize])
            );
            if let Some(display) = read_string(HKEY_LOCAL_MACHINE, &child, "Name") {
                providers.push(Provider { name: display });
            }
            index += 1;
        }
        RegCloseKey(key);
    }
    providers
}

fn current_user_sid() -> Option<String> {
    let output = Command::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let sid = text.trim().rsplit(',').next()?.trim_matches('"');
    sid.starts_with("S-1-").then(|| sid.to_owned())
}

fn read_dword(root: HKEY, path: &str, value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: NUL-terminated names; `data` holds a DWORD.
    let status = unsafe {
        RegGetValueW(
            root,
            wide(path).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

fn read_string(root: HKEY, path: &str, value: &str) -> Option<String> {
    let mut buffer = vec![0u16; 4096];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: NUL-terminated names; `size` is the buffer length in bytes.
    let status = unsafe {
        RegGetValueW(
            root,
            wide(path).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let chars = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..chars]))
}

fn write_dword(root: HKEY, path: &str, value: &str, data: u32) -> io::Result<()> {
    // SAFETY: NUL-terminated names; `data` is a 4-byte DWORD.
    let status = unsafe {
        RegSetKeyValueW(
            root,
            wide(path).as_ptr(),
            wide(value).as_ptr(),
            REG_DWORD,
            (&data as *const u32).cast(),
            4,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
    }

    #[test]
    fn allow_list_never_contains_fido_services() {
        let script = bluetooth_policy_script(true);
        for id in FIDO_SERVICES {
            assert!(!script.contains(&format!("0000{id}-")));
        }
        assert_eq!(classify_phone(None), Phone::Allowed);
        assert_eq!(classify_phone(Some("")), Phone::Allowed);
        let ours = ALLOWED_SERVICES
            .iter()
            .map(|id| format!("{{0000{id}-0000-1000-8000-00805F9B34FB}}"))
            .collect::<Vec<_>>()
            .join(";");
        assert_eq!(classify_phone(Some(&ours)), Phone::Blocked);
        assert_eq!(
            classify_phone(Some("{0000FFF9-0000-1000-8000-00805F9B34FB}")),
            Phone::ManagedElsewhere
        );
    }

    #[test]
    fn encoded_command_round_trips_through_real_powershell() {
        // Real PowerShell decodes our encoding and runs it.
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
            .arg(encode_command("Write-Output ('tap' + 'bridge ok')"))
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "tapbridge ok"
        );
    }

    #[test]
    fn reads_this_machine_without_panicking() {
        let options = read();
        assert!(matches!(
            options.phone,
            Phone::Allowed | Phone::Blocked | Phone::ManagedElsewhere
        ));
    }
}
