//! tapbridge-host: USB/IP CCID reader plus the phone session service.

pub mod cards;
pub mod ccid;
pub mod ctap_nfc;
pub mod ctap_pin;
pub mod ctaphid;
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

/// Isolates the macOS keychain item for one test; on other platforms state
/// lives only in the test's temp dir, so there is nothing to isolate.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn test_session() -> keychain::TestSession {
    keychain::TestSession::begin()
}
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) struct TestSession;
#[cfg(all(test, not(target_os = "macos")))]
pub(crate) fn test_session() -> TestSession {
    TestSession
}
