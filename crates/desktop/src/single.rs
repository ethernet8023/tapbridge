//! Single instance. The first launch owns a named mutex and one auto-reset
//! named event per window a later launch can ask for; a second launch
//! signals the matching event and exits.

use crate::win::wide;
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
    System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
        WaitForSingleObject,
    },
};

const MUTEX: &str = "Local\\tapbridge-single-instance";

/// What a later launch asks the running instance to show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    /// Plain second launch (or `--pair`): the pairing window.
    Pairing,
    /// `--passkey-options`: the passkey options window.
    PasskeyOptions,
}

impl Request {
    const ALL: [Request; 2] = [Request::Pairing, Request::PasskeyOptions];

    fn event_name(self) -> &'static str {
        match self {
            Request::Pairing => "Local\\tapbridge-open-pairing",
            Request::PasskeyOptions => "Local\\tapbridge-open-passkey-options",
        }
    }

    pub fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        if args.into_iter().any(|arg| arg == "--passkey-options") {
            Request::PasskeyOptions
        } else {
            Request::Pairing
        }
    }
}

/// Proof that this process is the single instance. Its handles stay open for
/// the life of the process, which keeps the names owned.
pub struct Instance {
    events: Vec<(Request, HANDLE)>,
}

/// Becomes the single instance, or signals the running one with `request`
/// and exits.
pub fn acquire(request: Request) -> Instance {
    let mutex_name = wide(MUTEX);
    // SAFETY: names are NUL-terminated UTF-16 that outlive each call.
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, wide(request.event_name()).as_ptr());
            if !event.is_null() {
                SetEvent(event);
            }
            std::process::exit(0);
        }
        let events = Request::ALL
            .into_iter()
            .map(|request| {
                // Auto-reset, initially unsignaled.
                let name = wide(request.event_name());
                (request, CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()))
            })
            .collect();
        Instance { events }
    }
}

impl Instance {
    /// Calls `on_request` on a background thread each time another launch
    /// asks for a window.
    pub fn listen(&self, on_request: impl Fn(Request) + Send + Sync + Clone + 'static) {
        for &(request, event) in &self.events {
            if event.is_null() {
                continue;
            }
            // The handles are never closed (process lifetime), so the waiting
            // threads may use them; HANDLE is a raw pointer, so pass usize.
            let event = event as usize;
            let on_request = on_request.clone();
            std::thread::spawn(move || {
                // SAFETY: `event` is a live event handle for the whole process.
                while unsafe { WaitForSingleObject(event as HANDLE, INFINITE) } == 0 {
                    on_request(request);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passkey_flag_selects_the_passkey_window() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert_eq!(
            Request::from_args(args(&["--passkey-options"])),
            Request::PasskeyOptions
        );
        assert_eq!(
            Request::from_args(args(&["--trace-apdu"])),
            Request::Pairing
        );
    }
}
