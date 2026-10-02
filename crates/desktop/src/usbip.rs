//! usbip-win2 client control: locate the installed usbip.exe, attach our
//! gadget on start, detach exactly our port on quit.
//!
//! `usbip attach -r 127.0.0.1 -b 1-1` after the host listener is up; the
//! gadget persists until detached (usbip-win2 keeps the vhci device; if the
//! host drops, re-attach on the next "phone connect" cycle is handled by the
//! service, not here). Detach uses `usbip port` to find our port index.
use std::{
    io,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

/// usbip.exe is a console program; without this every call from the
/// windowed app would flash a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const USBIP_PATH: &str = "C:\\Program Files\\USBip\\usbip.exe";
const BUSID: &str = "1-1";

/// Locates the installed usbip-win2 client.
pub fn find_usbip() -> Option<PathBuf> {
    let path = Path::new(USBIP_PATH);
    (path.is_file()).then(|| path.to_path_buf())
}

fn usbip(args: &[&str]) -> io::Result<String> {
    let output = Command::new(USBIP_PATH)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

/// Attaches the gadget. Returns Ok when the port is already attached.
pub fn attach() -> io::Result<bool> {
    let Some(_) = find_usbip() else {
        return Ok(false);
    };
    if !our_port().is_empty() {
        return Ok(true);
    }
    let output = usbip(&["attach", "-r", "127.0.0.1", "-b", BUSID])?;
    if !our_port().is_empty() {
        return Ok(true);
    }
    Err(io::Error::other(format!("usbip attach failed: {output}")))
}

/// Port indexes of our exported device (vid 1209 pid 7a71, busid 1-1).
pub fn our_port() -> Vec<u8> {
    usbip(&["port"])
        .map(|output| parse_ports(&output))
        .unwrap_or_default()
}

/// Port indexes in `usbip port` output whose device is ours (1209:7a71).
fn parse_ports(output: &str) -> Vec<u8> {
    let mut ports = Vec::new();
    let mut current_port: Option<u8> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(number) = trimmed
            .strip_prefix("Port ")
            .and_then(|rest| rest.split(':').next())
            .and_then(|number| number.trim().parse::<u8>().ok())
        {
            current_port = Some(number);
        } else if let Some(number) = current_port.filter(|_| trimmed.contains("1209:7a71")) {
            ports.push(number);
        }
    }
    ports
}

/// Detaches exactly our ports. Returns how many were detached.
pub fn detach_ours() -> usize {
    let mut detached = 0;
    for port in our_port() {
        let index = port.to_string();
        let ok = Command::new(USBIP_PATH)
            .args(["detach", "-p", &index])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if ok {
            detached += 1;
        }
    }
    detached
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `usbip port` output from usbip-win2 0.9.8.1 with our reader
    /// attached, plus a foreign device on port 2.
    const PORTS: &str = "Imported USB devices\r
====================\r
Port 01: device in use at Full Speed(12Mbps)\r
         Generic : unknown product (1209:7a71)\r
           -> usbip://127.0.0.1:3240/1-1\r
           -> remote bus/dev 001/001\r
           -> serial ''\r
Port 02: device in use at High Speed(480Mbps)\r
         Generic : unknown product (046d:c52b)\r
           -> usbip://10.0.0.5:3240/2-1\r
\r
";

    #[test]
    fn finds_only_our_reader_port() {
        assert_eq!(parse_ports(PORTS), vec![1]);
        assert!(parse_ports("Imported USB devices\r\n====================\r\n").is_empty());
    }
}
