//! Authenticated remote-reader sessions. Card bytes stay opaque.

pub mod cards;
pub mod pairing;
pub mod transport;
pub mod usb;
pub mod wake;
pub mod wire;

pub const ALPN: &[u8] = b"tapbridge/2";
