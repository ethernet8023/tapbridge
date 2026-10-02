//! Single instance. The first launch owns a named mutex and an auto-reset
//! named event; a second launch signals the event (the first instance opens
//! its pairing window) and exits.

use crate::win::wide;
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
    System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
        WaitForSingleObject,
    },
};

const MUTEX: &str = "Local\\tapbridge-single-instance";
const EVENT: &str = "Local\\tapbridge-open-pairing";

/// Proof that this process is the single instance. Its handles stay open for
/// the life of the process, which keeps the names owned.
pub struct Instance {
    event: HANDLE,
}

/// Becomes the single instance, or signals the running one and exits.
pub fn acquire() -> Instance {
    let mutex_name = wide(MUTEX);
    let event_name = wide(EVENT);
    // SAFETY: names are NUL-terminated UTF-16 that outlive each call.
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, event_name.as_ptr());
            if !event.is_null() {
                SetEvent(event);
            }
            std::process::exit(0);
        }
        // Auto-reset, initially unsignaled.
        let event = CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr());
        Instance { event }
    }
}

impl Instance {
    /// Calls `on_request` on a background thread each time another launch
    /// asks for the pairing window.
    pub fn listen(&self, on_request: impl Fn() + Send + 'static) {
        if self.event.is_null() {
            return;
        }
        // The handle is never closed (process lifetime), so the waiting
        // thread may use it; HANDLE is a raw pointer, so pass it as usize.
        let event = self.event as usize;
        std::thread::spawn(move || {
            // SAFETY: `event` is a live event handle for the whole process.
            while unsafe { WaitForSingleObject(event as HANDLE, INFINITE) } == 0 {
                on_request();
            }
        });
    }
}
