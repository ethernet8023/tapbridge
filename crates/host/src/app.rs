//! Joins authenticated phone sessions to the local reader.
use crate::{state::Store, trace::Tracer};
use iroh::{
    Endpoint,
    endpoint::{Connection, SendStream},
};
use std::{io, time::Duration};
use tapbridge_core::{
    pairing::PairingGate,
    transport,
    wire::{self, Frame},
};
use tokio::sync::{mpsc, watch};

pub async fn serve_phone(
    endpoint: Endpoint,
    mut store: Store,
    mut gate: PairingGate,
    mut outgoing: mpsc::Receiver<Frame>,
    incoming: mpsc::Sender<Frame>,
    mut shutdown: watch::Receiver<bool>,
    trace: bool,
) -> io::Result<()> {
    let mut tracer = trace.then(Tracer::default);
    let mut handshakes = tokio::task::JoinSet::new();
    let (events, mut received) = mpsc::channel::<io::Result<Frame>>(8);
    let mut active: Option<(Connection, SendStream, tokio::task::JoinHandle<()>)> = None;
    let result = async {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                connection = endpoint.accept() => {
                    let Some(connection) = connection else { break; };
                    if handshakes.len() >= 4 { connection.refuse(); continue; }
                    handshakes.spawn(async move {
                        tokio::time::timeout(Duration::from_secs(10), async {
                            let conn = connection.await.map_err(io::Error::other)?;
                            let (send, recv, hello) = transport::host_receive_hello(&conn).await?;
                            Ok::<_, io::Error>((conn, send, recv, hello))
                        }).await.map_err(io::Error::other)?
                    });
                },
                completed = handshakes.join_next(), if !handshakes.is_empty() => {
                    let Some(Ok(Ok((conn, mut send, recv, Frame::Hello { capability })))) = completed else { continue; };
                    let peer = conn.remote_id();
                    let authorized = if active.is_some() { false }
                    else if store.peer == Some(peer) { true }
                    else if store.peer.is_some() { false }
                    else {
                        match gate.authorize(peer, capability.as_ref()) {
                            Ok(pending) => {
                                store.set_peer(Some(peer))?;
                                gate.commit(pending).map_err(io::Error::other)?;
                                true
                            },
                            Err(_) => false,
                        }
                    };
                    if !authorized {
                        // Rejection is terminal; no arbitrary sleep before close.
                        let _ = tokio::time::timeout(Duration::from_secs(1), transport::reply_rejected(&mut send)).await;
                        let _ = send.finish();
                        let _ = tokio::time::timeout(Duration::from_secs(1), send.stopped()).await;
                        transport::close(&conn);
                        continue;
                    }
                    while outgoing.try_recv().is_ok() {}
                    if !matches!(tokio::time::timeout(Duration::from_secs(2), transport::reply_accepted(&mut send)).await, Ok(Ok(()))) {
                        transport::close(&conn);
                        continue;
                    }
                    let tx = events.clone();
                    let reader = tokio::spawn(async move {
                        let mut recv = recv;
                        loop {
                            let frame = wire::read_frame(&mut recv).await;
                            let failed = frame.is_err();
                            if tx.send(frame).await.is_err() || failed { break; }
                        }
                    });
                    active = Some((conn, send, reader));
                    eprintln!("phone connected");
                },
                event = received.recv(), if active.is_some() => {
                    match event {
                        Some(Ok(frame @ (Frame::Card { .. } | Frame::Response { .. } | Frame::Failed { .. }))) => {
                            if let Some(line) = tracer.as_mut().and_then(|t| t.observe(&frame)) { eprintln!("{line}"); }
                            tokio::select! {
                                result = incoming.send(frame) => result.map_err(|_| io::Error::other("reader stopped"))?,
                                _ = shutdown.changed() => break,
                            }
                        },
                        _ => disconnect(&mut active, &mut received, &incoming).await?,
                    }
                },
                frame = outgoing.recv() => {
                    let Some(frame) = frame else { break; };
                    if let Some(line) = tracer.as_mut().and_then(|t| t.observe(&frame)) { eprintln!("{line}"); }
                    if let Some((_, send, _)) = active.as_mut() {
                        let sent = tokio::time::timeout(Duration::from_secs(5), wire::write_frame(send, &frame)).await;
                        if !matches!(sent, Ok(Ok(()))) {
                            disconnect(&mut active, &mut received, &incoming).await?;
                        }
                    }
                },
            }
        }
        Ok(())
    }.await;
    if let Some((conn, _, reader)) = active {
        transport::close(&conn);
        reader.abort();
        let _ = reader.await;
    }
    handshakes.abort_all();
    while handshakes.join_next().await.is_some() {}
    endpoint.close().await;
    result
}

async fn disconnect(
    active: &mut Option<(Connection, SendStream, tokio::task::JoinHandle<()>)>,
    received: &mut mpsc::Receiver<io::Result<Frame>>,
    incoming: &mpsc::Sender<Frame>,
) -> io::Result<()> {
    if let Some((conn, _, reader)) = active.take() {
        transport::close(&conn);
        reader.abort();
        let _ = reader.await;
    }
    while received.try_recv().is_ok() {}
    tokio::time::timeout(
        Duration::from_secs(2),
        incoming.send(Frame::Card {
            generation: 0,
            atr: vec![],
            max_apdu: 0,
        }),
    )
    .await
    .map_err(|_| io::Error::other("reader stalled"))?
    .map_err(|_| io::Error::other("reader stopped"))?;
    eprintln!("phone disconnected; card removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{SecretKey, endpoint::presets};
    use tapbridge_core::ALPN;

    async fn endpoint(key: SecretKey) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(key)
            .alpns(vec![ALPN.to_vec()])
            .bind_addr(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pairs_persists_and_relays_then_clears_card_on_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let host = endpoint(store.key.clone()).await;
        let address = host.addr();
        let phone = endpoint(SecretKey::generate()).await;
        let phone_id = phone.id();
        let mut gate = PairingGate::new();
        let invitation = gate.mint(host.id());
        let (tx, rx) = mpsc::channel(8);
        let (events, mut received) = mpsc::channel(8);
        let (stop, shutdown) = watch::channel(false);
        let task = tokio::spawn(serve_phone(host, store, gate, rx, events, shutdown, false));
        let conn = phone.connect(address, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                transport::client_hello(&mut send, &mut recv, Some(invitation.capability()))
            )
            .await
            .unwrap()
            .unwrap(),
            transport::HelloReply::Accepted
        );
        assert_eq!(Store::open(dir.path()).unwrap().peer, Some(phone_id));
        let card = Frame::Card {
            generation: 1,
            atr: vec![0x3b, 0x80, 1, 0x81],
            max_apdu: 261,
        };
        wire::write_frame(&mut send, &card).await.unwrap();
        loop {
            if received.recv().await.unwrap() == card {
                break;
            }
        }
        let request = Frame::Apdu {
            id: 1,
            generation: 1,
            data: vec![0, 0xa4, 4, 0],
        };
        tx.send(request.clone()).await.unwrap();
        assert_eq!(wire::read_frame(&mut recv).await.unwrap(), request);
        let reply = Frame::Response {
            id: 1,
            generation: 1,
            data: vec![0x90, 0],
        };
        wire::write_frame(&mut send, &reply).await.unwrap();
        assert_eq!(received.recv().await.unwrap(), reply);
        transport::close(&conn);
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(5), received.recv()).await.unwrap(), Some(Frame::Card { atr, .. }) if atr.is_empty())
        );
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        phone.close().await;
    }
}
