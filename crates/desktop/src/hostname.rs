//! The Windows computer name: the host display name in `Accepted` and wake.
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

#[cfg(not(windows))]
pub fn computer_name() -> String {
    "Windows PC".into()
}
