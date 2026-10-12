//! The USB accessory transport behind [`crate::Session`] (DESIGN "USB link").
//!
//! Kotlin opens the accessory (`UsbManager.openAccessory`) and passes the
//! raw fd. We `dup` it, so Rust owns its own descriptor and Kotlin keeps
//! (and eventually closes) its `ParcelFileDescriptor` independently.
//!
//! Interrupting a blocked read: closing Kotlin's `ParcelFileDescriptor`
//! only interrupts reads made through Java (libcore's
//! `AsynchronousCloseMonitor` signals the threads IT registered for that
//! fd), and our dup keeps the file open anyway. So [`FdLink::interrupt`]
//! does what libcore does for its own threads: it sends libcore's
//! blocked-thread signal (`__SIGRTMIN + 2`, a no-op handler installed
//! without `SA_RESTART` in every app process) to the threads currently in
//! `read`/`write` on this fd. The accessory driver's waits are
//! interruptible, so the syscall returns `EINTR` and the thread sees the
//! closed flag. The signal is sent only when a handler is installed and
//! does not restart syscalls (a default disposition would kill the app).
//! For sockets (tests) `shutdown` wakes the reader too. Unplugging the
//! cable fails pending reads in the kernel regardless.
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tapbridge_core::usb::{HostInfo, Link, PumpGuard};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

#[cfg(target_os = "android")]
/// bionic `__SIGRTMIN + 2`: reserved for libcore's AsynchronousCloseMonitor
/// (bionic `reserved_signals.h`).
const BLOCKED_THREAD_SIGNAL: libc::c_int = 32 + 2;

const READER: usize = 0;
const WRITER: usize = 1;

/// A dup of the accessory fd as a blocking [`Link`].
pub(crate) struct FdLink {
    fd: OwnedFd,
    closed: AtomicBool,
    /// Threads currently inside `read` / `write`, to signal on interrupt.
    #[cfg(target_os = "android")]
    busy: Mutex<[Option<libc::pthread_t>; 2]>,
    #[cfg(not(target_os = "android"))]
    busy: Mutex<[bool; 2]>,
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "usb link closed")
}

impl FdLink {
    /// Duplicates `fd` (close-on-exec). The caller keeps ownership of `fd`.
    pub(crate) fn dup(fd: RawFd) -> io::Result<Self> {
        if fd < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid usb accessory fd",
            ));
        }
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(copy) },
            closed: AtomicBool::new(false),
            busy: Mutex::new(Default::default()),
        })
    }

    fn enter(&self, slot: usize) {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(target_os = "android")]
        {
            busy[slot] = Some(unsafe { libc::pthread_self() });
        }
        #[cfg(not(target_os = "android"))]
        {
            busy[slot] = true;
        }
    }

    fn leave(&self, slot: usize) {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        busy[slot] = Default::default();
    }

    /// Ends the link: later calls fail and blocked ones return. Idempotent,
    /// safe from any thread.
    pub(crate) fn interrupt(&self) {
        self.closed.store(true, Ordering::SeqCst);
        // Wakes a blocked read on a socket; ENOTSOCK on the accessory.
        unsafe { libc::shutdown(self.fd.as_raw_fd(), libc::SHUT_RDWR) };
        #[cfg(target_os = "android")]
        self.signal_busy_threads();
    }

    #[cfg(target_os = "android")]
    fn signal_busy_threads(&self) {
        if !interrupt_signal_usable() {
            log::warn!("usb: no interrupt signal handler; a blocked read ends on unplug");
            return;
        }
        // Resend while a thread is still inside a syscall: a signal that
        // lands just before the thread enters `read` is lost.
        for _ in 0..40 {
            {
                let busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
                if busy.iter().all(Option::is_none) {
                    return;
                }
                // Under the lock: a listed thread can't leave (and exit)
                // before we signal it, so the pthread_t is live.
                for thread in busy.iter().flatten() {
                    unsafe { libc::pthread_kill(*thread, BLOCKED_THREAD_SIGNAL) };
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(target_os = "android")]
fn interrupt_signal_usable() -> bool {
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(BLOCKED_THREAD_SIGNAL, std::ptr::null(), &mut old) } != 0 {
        return false;
    }
    old.sa_sigaction != libc::SIG_DFL
        && old.sa_sigaction != libc::SIG_IGN
        && old.sa_flags & libc::SA_RESTART == 0
}

impl Link for FdLink {
    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            // Register before checking: `interrupt` sets the flag and then
            // signals registered threads, so one of the two always lands.
            self.enter(READER);
            if self.closed.load(Ordering::SeqCst) {
                self.leave(READER);
                return Err(closed());
            }
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            let error = io::Error::last_os_error();
            self.leave(READER);
            match n {
                // A file EOF is the end of the link, never "nothing yet".
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "usb accessory closed",
                    ));
                }
                n if n > 0 => return Ok(n as usize),
                _ if error.kind() == io::ErrorKind::Interrupted => continue,
                _ => return Err(error),
            }
        }
    }

    fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            // Register before checking: `interrupt` sets the flag and then
            // signals registered threads, so one of the two always lands.
            self.enter(WRITER);
            if self.closed.load(Ordering::SeqCst) {
                self.leave(WRITER);
                return Err(closed());
            }
            let n = unsafe { libc::write(self.fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
            let error = io::Error::last_os_error();
            self.leave(WRITER);
            match n {
                0 => return Err(io::ErrorKind::WriteZero.into()),
                n if n > 0 => data = &data[n as usize..],
                _ if error.kind() == io::ErrorKind::Interrupted => continue,
                _ => return Err(error),
            }
        }
        Ok(())
    }
}

/// A handshaken USB link: the pumped stream halves plus the verified host.
pub(crate) struct UsbLink {
    pub(crate) info: HostInfo,
    pub(crate) reader: Mutex<ReadHalf<DuplexStream>>,
    pub(crate) writer: Mutex<WriteHalf<DuplexStream>>,
    pub(crate) link: Arc<FdLink>,
    pub(crate) guard: PumpGuard,
}

impl UsbLink {
    pub(crate) fn close(&self) {
        self.guard.stop();
        self.link.interrupt();
    }
}

impl Drop for UsbLink {
    fn drop(&mut self) {
        self.close();
    }
}
