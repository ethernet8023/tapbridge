//! Blocking interface for the Android-owned connection and NFC workers.
//! Kotlin owns tag lifecycle; this module owns only encrypted network I/O.
use iroh::{
    Endpoint, SecretKey,
    endpoint::{Connection, RecvStream, SendStream},
};
use std::{io, sync::Mutex, time::Duration};
use tapbridge_core::{
    pairing::Invitation,
    transport,
    wake::Hint,
    wire::{self, Frame},
};

#[cfg(test)]
mod tests;

#[cfg(target_os = "android")]
mod jni;

pub struct Session {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    connection: Connection,
    reader: Mutex<RecvStream>,
    writer: Mutex<SendStream>,
    /// Host display name from the `Accepted` handshake reply.
    host_name: String,
    /// The hint text that was dialed, for the phone to cache for reconnects.
    used_hint: String,
}

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
                        transport::HelloReply::Accepted { host_name } => host_name,
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
            Ok(Ok((connection, reader, writer, host_name))) => Ok((
                Self {
                    runtime,
                    endpoint,
                    connection,
                    reader: Mutex::new(reader),
                    writer: Mutex::new(writer),
                    host_name,
                    used_hint: used_hint.clone(),
                },
                used_hint,
            )),
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

    /// Host display name from the `Accepted` reply (1..=64 UTF-8 bytes).
    pub fn host_name(&self) -> &str {
        &self.host_name
    }

    /// Hint text actually used for the dial, so the phone can cache it and
    /// skip discovery on later reconnects.
    pub fn used_hint(&self) -> &str {
        &self.used_hint
    }

    pub fn read(&self) -> io::Result<Frame> {
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| io::Error::other("reader unavailable"))?;
        let frame = self.runtime.block_on(wire::read_frame(&mut *reader))?;
        if !matches!(
            frame,
            Frame::Apdu { .. }
                | Frame::Cancel { .. }
                | Frame::ReadCard
                | Frame::Auth { .. }
                | Frame::SetupPiv { .. }
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
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("writer unavailable"))?;
        let result = self.runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                wire::write_frame(&mut *writer, frame),
            )
            .await
        });
        match result {
            Ok(Ok(())) => Ok(()),
            failure => {
                self.close();
                match failure {
                    Ok(Err(error)) => Err(error),
                    _ => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "phone write timed out",
                    )),
                }
            }
        }
    }

    /// Safe concurrently with a blocked read. Release only after callers exit.
    pub fn close(&self) {
        transport::close(&self.connection);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
        self.runtime.block_on(self.endpoint.close());
    }
}

/// Encode the dialed address as a hint text so the phone can cache it.
fn hint_text(addr: &iroh::EndpointAddr) -> String {
    Hint::from_endpoint_addr(addr).to_string()
}
