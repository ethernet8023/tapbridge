//! tapbridge-host: USB/IP CCID reader module.

pub mod ccid;
pub mod reader;
pub mod usb;
pub mod usbip;

pub use reader::serve;
