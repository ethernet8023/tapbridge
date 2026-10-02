//! Start at login: the `tapbridge` value under HKCU `...\CurrentVersion\Run`
//! (REG_SZ, quoted exe path). The installer writes the same value.

use crate::win::wide;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE: &str = "tapbridge";
const ERROR_FILE_NOT_FOUND: u32 = 2;

pub fn is_enabled() -> bool {
    let key_name = wide(RUN_KEY);
    let value = wide(VALUE);
    // SAFETY: NUL-terminated names; `key` is closed on every path.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != 0
        {
            return false;
        }
        let present = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0;
        RegCloseKey(key);
        present
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let key_name = wide(RUN_KEY);
    let value = wide(VALUE);
    let command = std::env::current_exe()
        .map(|exe| wide(&format!("\"{}\"", exe.display())))
        .map_err(|error| format!("exe path: {error}"))?;
    // SAFETY: NUL-terminated names and data; `key` is closed on every path.
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let result = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if result != 0 {
            return Err(format!("open Run key failed ({result})"));
        }
        let result = if enabled {
            RegSetValueExW(
                key,
                value.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        } else {
            match RegDeleteValueW(key, value.as_ptr()) {
                ERROR_FILE_NOT_FOUND => 0,
                other => other,
            }
        };
        RegCloseKey(key);
        if result != 0 {
            return Err(format!("update Run value failed ({result})"));
        }
    }
    Ok(())
}
