//! This computer's display name: the host name in `Accepted` (what the phone
//! shows for this PC) and in wakes.
#[cfg(windows)]
pub fn computer_name() -> String {
    // ComputerNamePhysicalNetBIOS fallback; DNS hostname is the friendly name.
    use windows_sys::Win32::System::SystemInformation::ComputerNamePhysicalDnsHostname;
    use windows_sys::Win32::System::SystemInformation::GetComputerNameExW;
    unsafe {
        let mut size: u32 = 256;
        let mut buffer = vec![0u16; 256];
        if GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            buffer.as_mut_ptr(),
            &mut size,
        ) != 0
        {
            buffer.truncate(size as usize);
            return String::from_utf16_lossy(&buffer);
        }
        "Windows PC".into()
    }
}

/// The Computer Name from System Settings → General → Sharing (e.g.
/// "Ari's MacBook Pro"), falling back to the network host name.
#[cfg(target_os = "macos")]
pub fn computer_name() -> String {
    use core_foundation::{base::TCFType, string::CFString};
    use objc2_foundation::NSProcessInfo;
    use system_configuration_sys::dynamic_store_copy_specific::SCDynamicStoreCopyComputerName;
    // SAFETY: a null store asks configd directly; the returned string follows
    // the Copy rule (owned, released by the wrapper), or is null.
    let computer = unsafe {
        let name = SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut());
        (!name.is_null()).then(|| CFString::wrap_under_create_rule(name).to_string())
    };
    let name = computer
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            let host = NSProcessInfo::processInfo().hostName().to_string();
            host.strip_suffix(".local").unwrap_or(&host).to_owned()
        });
    fit(name.trim(), "Mac")
}

#[cfg(all(not(windows), not(target_os = "macos")))]
pub fn computer_name() -> String {
    "PC".into()
}

/// The wire contract caps host names at 64 UTF-8 bytes; cut at a char
/// boundary. Empty falls back to `default`.
#[cfg(target_os = "macos")]
fn fit(name: &str, default: &str) -> String {
    if name.is_empty() {
        return default.to_owned();
    }
    let mut end = name.len().min(64);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn names_fit_the_wire_limit() {
        assert_eq!(fit("", "Mac"), "Mac");
        assert_eq!(fit("iris", "Mac"), "iris");
        let long = "é".repeat(40); // 80 bytes
        let cut = fit(&long, "Mac");
        assert!(cut.len() <= 64 && cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn computer_name_is_not_the_windows_placeholder() {
        let name = computer_name();
        assert!(!name.is_empty() && name.len() <= 64);
        assert_ne!(name, "Windows PC");
        eprintln!("computer name: {name}");
    }
}
