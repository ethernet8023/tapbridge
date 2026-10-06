//! tapbridge-host: USB/IP CCID reader plus the phone session service.

pub mod ccid;
#[cfg(target_os = "macos")]
pub mod ifd;
#[cfg(target_os = "macos")]
pub mod keychain;
pub mod reader;
pub mod service;
pub mod state;
pub mod trace;
pub mod usb;
pub mod usbip;

pub use reader::serve;
