//! Blocking interface for the Android-owned connection and NFC workers.
//! Kotlin owns tag lifecycle; this module owns only encrypted network I/O.
use iroh::{
    Endpoint, EndpointId, SecretKey,
    endpoint::{Connection, RecvStream, SendStream},
};
use std::{io, sync::Mutex, time::Duration};
use tapbridge_core::{
    pairing::Invitation,
    transport,
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
}

impl Session {
    /// Called on a worker thread. Keys are loaded and protected by Android.
    pub fn connect(key: [u8; 32], host: &str, invitation: &str) -> io::Result<Self> {
        let host: EndpointId = host
            .parse()
            .map_err(|_| io::Error::other("invalid host identity"))?;
        let capability = if invitation.is_empty() {
            None
        } else {
            let invitation = Invitation::parse(invitation).map_err(io::Error::other)?;
            if invitation.host_id() != host {
                return Err(io::Error::other("invitation host mismatch"));
            }
            Some(invitation.capability())
        };
        Self::connect_to(
            transport::production_builder(&SecretKey::from_bytes(&key)),
            host.into(),
            capability,
        )
    }

    fn connect_to(
        builder: iroh::endpoint::Builder,
        host: iroh::EndpointAddr,
        capability: Option<[u8; 32]>,
    ) -> io::Result<Self> {
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
                if transport::client_hello(&mut writer, &mut reader, capability).await?
                    != transport::HelloReply::Accepted
                {
                    transport::close(&connection);
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "host rejected connection",
                    ));
                }
                Ok((connection, reader, writer))
            })
            .await
        });
        match connected {
            Ok(Ok((connection, reader, writer))) => Ok(Self {
                runtime,
                endpoint,
                connection,
                reader: Mutex::new(reader),
                writer: Mutex::new(writer),
            }),
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

    pub fn read(&self) -> io::Result<Frame> {
        let mut reader = self
            .reader
            .lock()
            .map_err(|_| io::Error::other("reader unavailable"))?;
        let frame = self.runtime.block_on(wire::read_frame(&mut *reader))?;
        if !matches!(
            frame,
            Frame::Apdu { .. } | Frame::Cancel { .. } | Frame::ReadCard
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
