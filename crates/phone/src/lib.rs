//! Blocking interface for the Android-owned connection and NFC workers.
//! Kotlin owns tag lifecycle; this module owns only encrypted network I/O.
use iroh::{
    Endpoint, SecretKey,
    endpoint::{Connection, RecvStream, SendStream},
};
use std::{
    io,
    os::fd::RawFd,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tapbridge_core::{
    pairing::Invitation,
    transport,
    wake::Hint,
    wire::{self, Frame},
};
use tokio::io::{AsyncRead, AsyncWrite};

mod usb;

#[cfg(test)]
mod tests;

#[cfg(target_os = "android")]
mod jni;

pub struct Session {
    runtime: tokio::runtime::Runtime,
    transport: Transport,
    /// The host's `Accepted` reply; set once the Hello exchange succeeded.
    /// A USB handle exists before that (pairing confirmation), and frame
    /// I/O is refused until it is set.
    accepted: OnceLock<Accepted>,
    /// The hint text that was dialed, for the phone to cache for reconnects.
    /// Over USB: the bare host id, so the network path can find it later.
    used_hint: String,
    /// Set by [`Session::close`]; ends blocked reads even when the
    /// transport itself can't be interrupted.
    closed: tokio::sync::watch::Sender<bool>,
}

enum Transport {
    Iroh {
        endpoint: Endpoint,
        connection: Connection,
        reader: Mutex<RecvStream>,
        writer: Mutex<SendStream>,
    },
    Usb(usb::UsbLink),
}

struct Accepted {
    /// Host display name from the `Accepted` handshake reply.
    host_name: String,
    /// Host OS from the `Accepted` reply ([`wire::platform`]).
    host_platform: u8,
    /// The host can pair a card with its OS login.
    host_piv_login: bool,
}

/// How long the USB handshake and the Hello exchange may take.
const USB_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

impl Session {
    /// Called on a worker thread. Keys are loaded and protected by Android.
    ///
    /// `hint` is a [`Hint`] text form: `HOSTID[;r=RELAY][;d=IP:PORT]*`. A bare
    /// host id is a valid hint. An empty hint (paired reconnect without a
    /// cached address) dials the host id alone, letting iroh discovery and
    /// relays locate the PC.
    pub fn connect(
        key: [u8; 32],
        hint: &str,
        invitation: &str,
        name: &str,
        push: Option<String>,
    ) -> io::Result<Self> {
        let parsed: Hint = if hint.is_empty() {
            return Err(io::Error::other("no cached host hint"));
        } else {
            hint.parse::<Hint>()
                .map_err(|_| io::Error::other("invalid wake hint"))?
        };
        let capability = if invitation.is_empty() {
            None
        } else {
            let invitation = Invitation::parse(invitation).map_err(io::Error::other)?;
            if invitation.host_id() != parsed.host {
                return Err(io::Error::other("invitation host mismatch"));
            }
            Some(invitation.capability())
        };
        let (session, _) = Self::connect_to(
            transport::production_builder(&SecretKey::from_bytes(&key)),
            parsed.endpoint_addr(),
            transport::ClientHello {
                capability,
                name: name.into(),
                push,
            },
        )?;
        Ok(session)
    }

    fn connect_to(
        builder: iroh::endpoint::Builder,
        host: iroh::EndpointAddr,
        hello: transport::ClientHello,
    ) -> io::Result<(Self, String)> {
        let used_hint = hint_text(&host);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let endpoint = runtime.block_on(builder.bind()).map_err(io::Error::other)?;
        let connected = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(15), async {
                let connection = endpoint
                    .connect(host, tapbridge_core::ALPN)
                    .await
                    .map_err(io::Error::other)?;
                let (mut writer, mut reader) =
                    connection.open_bi().await.map_err(io::Error::other)?;
                let accepted =
                    match transport::client_hello(&mut writer, &mut reader, hello).await? {
                        transport::HelloReply::Accepted {
                            host_name,
                            platform,
                            piv_login,
                        } => (host_name, platform, piv_login),
                        transport::HelloReply::Rejected => {
                            transport::close(&connection);
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "host rejected connection",
                            ));
                        }
                    };
                Ok((connection, reader, writer, accepted))
            })
            .await
        });
        match connected {
            Ok(Ok((connection, reader, writer, (host_name, host_platform, host_piv_login)))) => {
                let accepted = OnceLock::new();
                let _ = accepted.set(Accepted {
                    host_name,
                    host_platform,
                    host_piv_login,
                });
                Ok((
                    Self {
                        runtime,
                        transport: Transport::Iroh {
                            endpoint,
                            connection,
                            reader: Mutex::new(reader),
                            writer: Mutex::new(writer),
                        },
                        accepted,
                        used_hint: used_hint.clone(),
                        closed: tokio::sync::watch::channel(false).0,
                    },
                    used_hint,
                ))
            }
            error => {
                runtime.block_on(endpoint.close());
                match error {
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "host connection timed out",
                    )),
                    _ => unreachable!(),
                }
            }
        }
    }

    /// USB step 1, on a worker thread: takes the accessory fd (Kotlin keeps
    /// owning it; we use a dup), runs the signed handshake and returns a
    /// session whose host is verified but not yet greeted. The caller
    /// checks [`Session::host_id`] / [`Session::pairing_open`], asks the
    /// user if needed, then calls [`Session::usb_hello`].
    pub fn usb_open(fd: RawFd, key: [u8; 32]) -> io::Result<Self> {
        Self::usb_open_link(Arc::new(usb::FdLink::dup(fd)?), key)
    }

    fn usb_open_link(link: Arc<usb::FdLink>, key: [u8; 32]) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (stream, guard) = tapbridge_core::usb::pump(
            Arc::clone(&link) as Arc<dyn tapbridge_core::usb::Link>,
            runtime.handle().clone(),
        );
        let (mut reader, mut writer) = tokio::io::split(stream);
        let secret = SecretKey::from_bytes(&key);
        let shaken = runtime.block_on(async {
            tokio::time::timeout(
                USB_HANDSHAKE_TIMEOUT,
                tapbridge_core::usb::phone_handshake(&mut reader, &mut writer, &secret),
            )
            .await
        });
        let info = match shaken {
            Ok(Ok(info)) => info,
            failure => {
                guard.stop();
                link.interrupt();
                return Err(match failure {
                    Ok(Err(error)) => error,
                    _ => io::Error::new(io::ErrorKind::TimedOut, "usb handshake timed out"),
                });
            }
        };
        Ok(Self {
            runtime,
            used_hint: info.host.to_string(),
            transport: Transport::Usb(usb::UsbLink {
                info,
                reader: Mutex::new(reader),
                writer: Mutex::new(writer),
                link,
                guard,
            }),
            accepted: OnceLock::new(),
            closed: tokio::sync::watch::channel(false).0,
        })
    }

    /// USB step 2: sends `Hello` (with the host's pairing capability when
    /// `use_pairing`, i.e. the user confirmed pairing) and waits for
    /// `Accepted`. Afterwards the session works exactly like a network one.
    pub fn usb_hello(&self, use_pairing: bool, name: &str, push: Option<String>) -> io::Result<()> {
        let Transport::Usb(link) = &self.transport else {
            return Err(io::Error::other("not a usb session"));
        };
        if self.accepted.get().is_some() {
            return Err(io::Error::other("hello already sent"));
        }
        let capability = if use_pairing {
            Some(link.info.capability.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the PC's pairing window is not open",
                )
            })?)
        } else {
            None
        };
        let hello = Frame::Hello {
            capability,
            name: name.into(),
            push,
        };
        let result = {
            let mut reader = lock(&link.reader, "reader")?;
            let mut writer = lock(&link.writer, "writer")?;
            self.runtime.block_on(async {
                tokio::time::timeout(
                    USB_HANDSHAKE_TIMEOUT,
                    hello_exchange(&mut *reader, &mut *writer, &hello),
                )
                .await
            })
        };
        match result {
            Ok(Ok(accepted)) => {
                let _ = self.accepted.set(accepted);
                Ok(())
            }
            failure => {
                self.close();
                Err(match failure {
                    Ok(Err(error)) => error,
                    _ => io::Error::new(io::ErrorKind::TimedOut, "host did not answer hello"),
                })
            }
        }
    }

    /// Verified host id (64 lowercase hex) of a USB session; empty for a
    /// network session (its host id is the dialed hint's).
    pub fn host_id(&self) -> String {
        match &self.transport {
            Transport::Usb(link) => link.info.host.to_string(),
            Transport::Iroh { connection, .. } => connection.remote_id().to_string(),
        }
    }

    /// The PC's pairing window is open (USB handshake carried a capability).
    pub fn pairing_open(&self) -> bool {
        matches!(&self.transport, Transport::Usb(link) if link.info.capability.is_some())
    }

    /// Whether this session runs over the USB cable.
    pub fn is_usb(&self) -> bool {
        matches!(self.transport, Transport::Usb(_))
    }

    /// Host display name from the `Accepted` reply (1..=64 UTF-8 bytes);
    /// before that (USB pairing prompt) the name from the USB handshake.
    pub fn host_name(&self) -> &str {
        match (self.accepted.get(), &self.transport) {
            (Some(accepted), _) => &accepted.host_name,
            (None, Transport::Usb(link)) => &link.info.name,
            (None, Transport::Iroh { .. }) => "",
        }
    }

    /// Host OS ([`wire::platform`]), for wording only.
    pub fn host_platform(&self) -> u8 {
        self.accepted
            .get()
            .map_or(wire::platform::OTHER, |a| a.host_platform)
    }

    /// Whether the host can pair a card's PIV identity with its OS login.
    pub fn host_piv_login(&self) -> bool {
        self.accepted.get().is_some_and(|a| a.host_piv_login)
    }

    /// Hint text actually used for the dial, so the phone can cache it and
    /// skip discovery on later reconnects.
    pub fn used_hint(&self) -> &str {
        &self.used_hint
    }

    fn ready(&self) -> io::Result<()> {
        if *self.closed.borrow() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "connection closed",
            ));
        }
        if self.accepted.get().is_none() {
            return Err(io::Error::other("host has not accepted yet"));
        }
        Ok(())
    }

    pub fn read(&self) -> io::Result<Frame> {
        self.ready()?;
        let frame = match &self.transport {
            Transport::Iroh { reader, .. } => {
                let mut reader = lock(reader, "reader")?;
                self.runtime.block_on(wire::read_frame(&mut *reader))?
            }
            Transport::Usb(link) => {
                let mut reader = lock(&link.reader, "reader")?;
                let mut closed = self.closed.subscribe();
                self.runtime.block_on(async {
                    tokio::select! {
                        frame = wire::read_frame(&mut *reader) => frame,
                        _ = closed.wait_for(|closed| *closed) => Err(io::Error::new(
                            io::ErrorKind::NotConnected,
                            "connection closed",
                        )),
                    }
                })?
            }
        };
        if !matches!(
            frame,
            Frame::Apdu { .. }
                | Frame::Cancel { .. }
                | Frame::ReadCard
                | Frame::Auth { .. }
                | Frame::CardInfo(_)
                | Frame::CardAction { .. }
                | Frame::CardActionDone { .. }
        ) {
            self.close();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid host frame",
            ));
        }
        Ok(frame)
    }

    pub fn send(&self, frame: &Frame) -> io::Result<()> {
        self.ready()?;
        let result = match &self.transport {
            Transport::Iroh { writer, .. } => {
                let mut writer = lock(writer, "writer")?;
                self.runtime.block_on(write_timed(&mut *writer, frame))
            }
            Transport::Usb(link) => {
                let mut writer = lock(&link.writer, "writer")?;
                self.runtime.block_on(write_timed(&mut *writer, frame))
            }
        };
        if result.is_err() {
            self.close();
        }
        result
    }

    /// Safe concurrently with a blocked read. Release only after callers exit.
    pub fn close(&self) {
        self.closed.send_replace(true);
        match &self.transport {
            Transport::Iroh { connection, .. } => transport::close(connection),
            Transport::Usb(link) => link.close(),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
        if let Transport::Iroh { endpoint, .. } = &self.transport {
            self.runtime.block_on(endpoint.close());
        }
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>, what: &str) -> io::Result<std::sync::MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other(format!("{what} unavailable")))
}

async fn write_timed<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    match tokio::time::timeout(Duration::from_secs(5), wire::write_frame(writer, frame)).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "phone write timed out",
        )),
    }
}

/// `Hello` then `Accepted`/`Rejected`, as [`transport::client_hello`] does
/// over iroh.
async fn hello_exchange<R, W>(reader: &mut R, writer: &mut W, hello: &Frame) -> io::Result<Accepted>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    wire::write_frame(writer, hello).await?;
    match wire::read_frame(reader).await? {
        Frame::Accepted {
            name,
            platform,
            piv_login,
        } => Ok(Accepted {
            host_name: name,
            host_platform: platform,
            host_piv_login: piv_login,
        }),
        Frame::Rejected => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "host rejected connection",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected frame in handshake",
        )),
    }
}

/// Encode the dialed address as a hint text so the phone can cache it.
fn hint_text(addr: &iroh::EndpointAddr) -> String {
    Hint::from_endpoint_addr(addr).to_string()
}
