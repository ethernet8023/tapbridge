//! Wakes the phone when Windows starts a WebAuthn ceremony.
//!
//! Signal: the `Microsoft-Windows-WebAuthN/Operational` event log, which
//! interactive users may read. Measured on Windows 11 build 28120:
//! event 1000 = "WebAuthN Ctap MakeCredential started", event 1003 =
//! "WebAuthN Ctap GetAssertion started". Event 2100 is NOT used: it also
//! fires for internal steps such as GetPluginAuthenticatorList.
//!
//! The dialog window itself is a poor signal: it is hosted by
//! `CredentialUIBroker.exe` (class `Credential Dialog Xaml Host`, title
//! "Windows Security"), which also hosts ordinary password prompts, and its
//! content is not readable through UI Automation from another process.
//! `scripts/probe-webauthn-dialog.ps1` reproduces a real ceremony.

use crate::win::wide;
use std::{
    ffi::c_void,
    io,
    sync::Mutex,
    time::{Duration, Instant},
};
use windows_sys::Win32::System::EventLog::{
    EVT_HANDLE, EVT_SUBSCRIBE_NOTIFY_ACTION, EvtClose, EvtSubscribe, EvtSubscribeActionDeliver,
    EvtSubscribeToFutureEvents,
};

const CHANNEL: &str = "Microsoft-Windows-WebAuthN/Operational";
/// MakeCredential started (1000) or GetAssertion started (1003).
const QUERY: &str = "*[System[(EventID=1000 or EventID=1003)]]";
/// One ceremony can log its start more than once; one wake is enough.
const DEBOUNCE: Duration = Duration::from_secs(3);

struct Context {
    on_ceremony: Box<dyn Fn() + Send + Sync>,
    last: Mutex<Option<Instant>>,
}

impl Context {
    fn deliver(&self) {
        let now = Instant::now();
        let due = match self.last.lock() {
            Ok(mut last) => {
                let due = last.is_none_or(|at| now.duration_since(at) >= DEBOUNCE);
                if due {
                    *last = Some(now);
                }
                due
            }
            Err(_) => false,
        };
        if due {
            (self.on_ceremony)();
        }
    }
}

/// A live subscription. Dropping it unsubscribes.
pub struct Watcher {
    subscription: EVT_HANDLE,
    // Owned here and borrowed by the callback through a raw pointer; freed
    // only after EvtClose has returned.
    context: *mut Context,
}

impl Watcher {
    /// Calls `on_ceremony` (on a system thread-pool thread) once per WebAuthn
    /// ceremony start, debounced.
    pub fn start(on_ceremony: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let context = Box::into_raw(Box::new(Context {
            on_ceremony: Box::new(on_ceremony),
            last: Mutex::new(None),
        }));
        let channel = wide(CHANNEL);
        let query = wide(QUERY);
        // SAFETY: strings are NUL-terminated and outlive the call; `context`
        // stays valid until Drop, which closes the subscription first.
        let subscription = unsafe {
            EvtSubscribe(
                0,
                std::ptr::null_mut(),
                channel.as_ptr(),
                query.as_ptr(),
                0,
                context.cast_const().cast(),
                Some(on_event),
                EvtSubscribeToFutureEvents,
            )
        };
        if subscription == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: the subscription failed, so no callback holds `context`.
            drop(unsafe { Box::from_raw(context) });
            return Err(error);
        }
        Ok(Self {
            subscription,
            context,
        })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: EvtClose on a subscription waits for running callbacks, so
        // the context is unused once it returns.
        unsafe {
            EvtClose(self.subscription);
            drop(Box::from_raw(self.context));
        }
    }
}

unsafe extern "system" fn on_event(
    action: EVT_SUBSCRIBE_NOTIFY_ACTION,
    context: *const c_void,
    _event: EVT_HANDLE,
) -> u32 {
    if action == EvtSubscribeActionDeliver && !context.is_null() {
        // SAFETY: `context` is the Box owned by the live Watcher.
        unsafe { &*context.cast::<Context>() }.deliver();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn debounces_repeated_starts_of_one_ceremony() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let context = Context {
            on_ceremony: Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            last: Mutex::new(None),
        };
        context.deliver();
        context.deliver();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        *context.last.lock().unwrap() = Some(Instant::now() - DEBOUNCE);
        context.deliver();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn subscribes_to_the_real_webauthn_channel() {
        // Real subscription against this machine's event log: proves the
        // channel exists, is readable without elevation and the query parses.
        let watcher = Watcher::start(|| {}).expect("subscribe to WebAuthN/Operational");
        drop(watcher);
    }
}
