//! Joins authenticated phone sessions to the local reader, behind a service
//! API the desktop drives. No CLI lives here.
//!
//! Threading model: [`start`] is async and awaited inside the caller's
//! runtime (the desktop drives it from one dedicated runtime thread). The
//! returned [`ServiceHandle`] is fully synchronous — plain `std` channels —
//! so the gpui UI thread calls it directly without crossing into tokio.
use crate::{
    cards::{CapabilityProbe, CardInfo, KnownCard, KnownCards, uid_stable},
    state::Store,
    trace::Tracer,
};
use iroh::{
    Endpoint,
    endpoint::{Connection, SendStream},
};
use std::{io, net::SocketAddr, path::PathBuf, sync::mpsc, thread, time::Duration};

/// PIV APDU deadline on the macOS IFD path, matching the usbip reader's
/// `APDU_DEADLINE` (2 s) plus its allowed extensions bound.
#[cfg(target_os = "macos")]
const PIV_DEADLINE: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const CTAP_DEADLINE: Duration = Duration::from_secs(15);
use tapbridge_core::{
    pairing::{Invitation, PairingGate},
    transport,
    wake::Hint,
    wire::{self, Frame},
};
use tokio::sync::{mpsc as tokio_mpsc, watch};

/// Settings for [`start`].
pub struct Settings {
    /// `%LOCALAPPDATA%\tapbridge` in production; tests use a temp dir.
    pub state_dir: PathBuf,
    /// USB/IP listener address; loopback only (`127.0.0.1:3240`).
    pub usbip_addr: SocketAddr,
    /// Host display name sent in `Accepted` (the Windows computer name).
    pub host_name: String,
    /// Payload-free APDU summaries (`--trace-apdu`).
    pub trace: bool,
}

/// Status transitions the desktop reflects in tray and windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    PhoneConnected {
        name: String,
    },
    PhoneDisconnected,
    /// `uid` is the card's stable NFC UID; empty when absent, unknown or
    /// random.
    CardPresent {
        present: bool,
        uid: Vec<u8>,
    },
    /// The known-cards list changed (here or synced from the phone).
    /// Re-read it with [`ServiceHandle::known_cards`].
    CardsChanged,
    /// A card with a stable UID that isn't known was tapped: offer to add
    /// it. `label` is a suggested name.
    UnknownCard {
        uid: Vec<u8>,
        label: String,
    },
    /// The phone asks this PC to do a [`wire::action`] for a card.
    CardActionRequested {
        uid: Vec<u8>,
        action: u8,
    },
    /// The phone finished an action this PC asked for
    /// ([`ServiceHandle::request_card_action`]).
    CardActionDone {
        uid: Vec<u8>,
        action: u8,
        ok: bool,
    },
    /// A phone completed pairing with the outstanding invitation: a new phone,
    /// or the already paired phone scanning a fresh code.
    PairingCompleted {
        name: String,
    },
    /// One payload-free APDU trace line (`Settings::trace` only): command
    /// name and status, never payload bytes.
    Trace(String),
}

/// Everything the desktop needs to draw the status line in one query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub paired: bool,
    pub phone_connected: bool,
    pub phone_name: Option<String>,
    pub card_present: bool,
    pub push_stored: bool,
    /// Current card ATR when present (empty otherwise).
    pub atr: Vec<u8>,
    /// Generation of the current card tap (0 before the first). Every tap
    /// powers the card up afresh, which regenerates its CTAP PIN key
    /// agreement key: the security key uses this to notice a re-tap.
    pub card_generation: u64,
    /// Stable NFC UID of the current card; empty when absent, unknown or
    /// random.
    pub card_uid: Vec<u8>,
}

/// A change the user (or this PC) makes to one card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardEdit {
    /// Add the card under this name (it was tapped; see
    /// [`Event::UnknownCard`]), or revive a forgotten one.
    Add(String),
    Rename(String),
    /// The phone's "open tapbridge when tapped".
    SetWake(bool),
    /// Whether the card is paired with this PC's OS login.
    SetLogin(bool),
    /// What this PC found the card supports ([`crate::cards::caps`]).
    NoteCaps(u8),
    Forget,
}

/// What happened after a [`ServiceHandle::request_tap`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TapOutcome {
    /// A session is live: `ReadCard` was sent; the phone shows "tap now".
    ReadCardSent,
    /// No live session: the desktop must POST this token and hint to the relay.
    Wake { token: String, hint: Hint },
    /// Not paired yet.
    #[default]
    NotPaired,
    /// Paired but no push token and no live session.
    NoPush,
}

enum Command {
    MintInvitation(std::sync::mpsc::SyncSender<Invitation>),
    ForgetPhone(std::sync::mpsc::SyncSender<()>),
    RequestTap(std::sync::mpsc::SyncSender<TapOutcome>),
    /// Tell the connected phone where the current sign-in stands
    /// ([`wire::auth`]); dropped silently when no session is live.
    AuthState(u8),
    Status(std::sync::mpsc::SyncSender<Status>),
    KnownCards(std::sync::mpsc::SyncSender<Vec<KnownCard>>),
    /// A local edit to the card list; synced to the phone when it changes
    /// anything.
    EditCard {
        uid: Vec<u8>,
        edit: CardEdit,
        reply: std::sync::mpsc::SyncSender<bool>,
    },
    /// Send a [`Frame::CardAction`] or [`Frame::CardActionDone`].
    CardFrame {
        frame: Frame,
        reply: std::sync::mpsc::SyncSender<bool>,
    },
    EndpointAddr(std::sync::mpsc::SyncSender<iroh::EndpointAddr>),
    /// macOS PIV path: relay one APDU to the phone card and reply with the
    /// R-APDU. Mirrors the usbip reader's single-in-flight model.
    #[cfg(target_os = "macos")]
    PivTransmit {
        apdu: Vec<u8>,
        deadline: Duration,
        reply: std::sync::mpsc::SyncSender<PivResult>,
    },
    /// macOS PIV path: deadline watchdog fired for `apdu_id`.
    #[cfg(target_os = "macos")]
    PivTimeout {
        apdu_id: u64,
    },
}

/// macOS PIV transmit outcome; `TransmitError` maps onto the IFD socket
/// protocol statuses used by the shim.
#[cfg(target_os = "macos")]
pub type PivResult = Result<Vec<u8>, crate::ifd::TransmitError>;

/// Watchdog notifications for stalled PIV transits travel as
/// [`Command::PivTimeout`] from the per-transmit timer task.
/// One PIV APDU in flight (size-1 like the reader's `InFlight`).
#[cfg(target_os = "macos")]
struct PivInFlight {
    apdu_id: u64,
    generation: u64,
    reply: std::sync::mpsc::SyncSender<PivResult>,
}

/// The running service. Call [`shutdown`](Self::shutdown) to stop the
/// workers cleanly; detach usbip after it returns. Cloning shares the same
/// underlying service (used by the macOS IFD server thread).
#[derive(Clone)]
pub struct ServiceHandle {
    commands: std::sync::Arc<std::sync::Mutex<Option<mpsc::Sender<Command>>>>,
    events: std::sync::Arc<std::sync::Mutex<mpsc::Receiver<Event>>>,
    shutdown: watch::Sender<bool>,
    done: std::sync::Arc<std::sync::Mutex<mpsc::Receiver<io::Result<()>>>>,
}

impl ServiceHandle {
    /// Draining queue of transitions; call [`drain_events`](Self::drain_events).
    pub fn drain_events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        if let Ok(receiver) = self.events.lock() {
            while let Ok(event) = receiver.try_recv() {
                events.push(event);
            }
        }
        events
    }

    pub fn status(&self) -> Status {
        self.reply(Command::Status)
    }

    /// The host's own iroh address: the wake hint source and the dial target
    /// for a pairing phone.
    pub fn endpoint_addr(&self) -> Option<iroh::EndpointAddr> {
        self.reply_option(Command::EndpointAddr)
    }

    /// Mints a fresh invitation; minting again revokes the previous one.
    /// `None` when the service has already shut down.
    pub fn mint_invitation(&self) -> Option<Invitation> {
        self.reply_option(Command::MintInvitation)
    }

    /// Revokes the paired phone and drops a live session.
    pub fn forget_phone(&self) {
        self.ack(Command::ForgetPhone);
    }

    /// The wake trigger: `ReadCard` on a live session, otherwise the stored
    /// push token plus the current hint for a relay POST.
    pub fn request_tap(&self) -> TapOutcome {
        self.reply(Command::RequestTap)
    }

    /// Shows a sign-in state on the phone's tap animation (a
    /// [`tapbridge_core::wire::auth`] constant). Fire-and-forget: ignored
    /// when no phone is connected.
    pub fn send_auth_state(&self, state: u8) {
        if let Ok(commands) = self.commands.lock()
            && let Some(sender) = commands.as_ref()
        {
            let _ = sender.send(Command::AuthState(state));
        }
    }

    /// The user's cards, oldest first.
    pub fn known_cards(&self) -> Vec<KnownCard> {
        self.reply(Command::KnownCards)
    }

    /// Changes one card and syncs it to the phone. `false` when nothing
    /// changed (unknown card, same value, unusable name).
    pub fn edit_card(&self, uid: Vec<u8>, edit: CardEdit) -> bool {
        self.reply_option(|reply| Command::EditCard { uid, edit, reply })
            .unwrap_or(false)
    }

    /// Asks the connected phone to do a [`wire::action`] for the card `uid`.
    /// `false` when no session is live (wake the phone first). The answer
    /// arrives as [`Event::CardActionDone`].
    pub fn request_card_action(&self, uid: Vec<u8>, action: u8) -> bool {
        let frame = Frame::CardAction { uid, action };
        self.reply_option(|reply| Command::CardFrame { frame, reply })
            .unwrap_or(false)
    }

    /// Tells the phone how an action it asked for ended.
    pub fn card_action_done(&self, uid: Vec<u8>, action: u8, ok: bool) -> bool {
        let frame = Frame::CardActionDone { uid, action, ok };
        self.reply_option(|reply| Command::CardFrame { frame, reply })
            .unwrap_or(false)
    }

    /// macOS PIV path: relays one APDU to the phone card, blocking until the
    /// R-APDU (or error) arrives; the deadline matches the usbip reader.
    #[cfg(target_os = "macos")]
    pub fn piv_transmit(&self, apdu: Vec<u8>) -> PivResult {
        self.transmit(apdu, PIV_DEADLINE)
    }

    /// macOS virtual security key path: like [`Self::piv_transmit`] with a
    /// longer deadline, since CTAP operations (key generation, PIN
    /// protocol) can take seconds on a card. Matches the phone's 15 s
    /// IsoDep timeout.
    #[cfg(target_os = "macos")]
    pub fn ctap_transmit(&self, apdu: Vec<u8>) -> PivResult {
        self.transmit(apdu, CTAP_DEADLINE)
    }

    #[cfg(target_os = "macos")]
    fn transmit(&self, apdu: Vec<u8>, deadline: Duration) -> PivResult {
        self.reply_option(|reply| Command::PivTransmit {
            apdu,
            deadline,
            reply,
        })
        .unwrap_or(Err(crate::ifd::TransmitError::Other))
    }

    /// Requests graceful shutdown and waits for the workers to finish.
    /// Detach usbip after this returns.
    pub fn shutdown(&self) {
        drop(self.commands.lock().unwrap().take());
        let mut watch = self.shutdown.subscribe();
        // Sync wait: the caller is a UI thread, not inside a runtime.
        while *watch.borrow_and_update() {
            thread::sleep(Duration::from_millis(50));
        }
        if let Ok(done) = self.done.lock() {
            let _ = done.recv();
        }
    }

    fn reply<T: Default>(&self, command: fn(std::sync::mpsc::SyncSender<T>) -> Command) -> T {
        self.reply_option(command).unwrap_or_default()
    }

    fn reply_option<T>(
        &self,
        command: impl FnOnce(std::sync::mpsc::SyncSender<T>) -> Command,
    ) -> Option<T> {
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let commands = self.commands.lock().unwrap();
            let sender = commands.as_ref()?;
            sender.send(command(tx)).ok()?;
        }
        rx.recv().ok()
    }

    fn ack(&self, command: fn(std::sync::mpsc::SyncSender<()>) -> Command) {
        let _ = self.reply_option(command);
    }
}

/// Starts the host service: USB/IP reader plus phone session owner.
///
/// Locks `state_dir/host.lock`, binds `usbip_addr` (loopback only) and binds
/// the iroh endpoint. The returned handle drives everything synchronously.
pub async fn start(settings: Settings) -> io::Result<ServiceHandle> {
    std::fs::create_dir_all(&settings.state_dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(settings.state_dir.join("host.lock"))?;
    lock.try_lock()
        .map_err(|_| io::Error::other("another host owns this state directory; stop it first"))?;
    let store = Store::open(&settings.state_dir)?;
    let listener = tokio::net::TcpListener::bind(settings.usbip_addr).await?;
    let endpoint = transport::production_builder(&store.key)
        .bind()
        .await
        .map_err(io::Error::other)?;
    let (commands_tx, commands_ui) = mpsc::channel::<Command>();
    let (commands_tx_tokio, commands_rx) = tokio_mpsc::channel::<Command>(16);
    // Dedicated PIV-timeout channel: the loop holds no clone of the commands
    // channel, so shutdown (which waits for the loop to end once all command
    // senders drop) is never blocked by an idle timeout sender.
    let (events_tokio_tx, mut events_tokio_rx) = tokio_mpsc::unbounded_channel::<Event>();
    let (events_tx, events_rx) = mpsc::channel::<Event>();
    let (stop, shutdown) = watch::channel(false);
    let (done_tx, done_rx) = mpsc::channel::<io::Result<()>>();
    // Detached deliberately; completion is reported through `done_tx`.
    let run_job = tokio::spawn(run(
        store,
        endpoint,
        listener,
        settings,
        shutdown,
        commands_rx,
        events_tokio_tx,
        done_tx,
    ));
    drop(run_job);
    // Forward UI commands into the tokio loop on a plain OS thread.
    let handle = tokio::runtime::Handle::current();
    thread::spawn(move || {
        for command in commands_ui {
            if handle.block_on(commands_tx_tokio.send(command)).is_err() {
                break;
            }
        }
    });
    // Forward service events out to the UI thread.
    thread::spawn(move || {
        while let Some(event) = events_tokio_rx.blocking_recv() {
            if events_tx.send(event).is_err() {
                break;
            }
        }
    });
    Ok(ServiceHandle {
        commands: std::sync::Arc::new(std::sync::Mutex::new(Some(commands_tx))),
        events: std::sync::Arc::new(std::sync::Mutex::new(events_rx)),
        shutdown: stop,
        done: std::sync::Arc::new(std::sync::Mutex::new(done_rx)),
    })
}

#[allow(clippy::too_many_arguments)]
async fn run(
    store: Store,
    endpoint: Endpoint,
    listener: tokio::net::TcpListener,
    settings: Settings,
    shutdown: watch::Receiver<bool>,
    commands: tokio_mpsc::Receiver<Command>,
    events: tokio_mpsc::UnboundedSender<Event>,
    done: mpsc::Sender<io::Result<()>>,
) {
    let result = run_inner(
        store, endpoint, listener, settings, shutdown, commands, events,
    )
    .await;
    let _ = done.send(result);
}

async fn run_inner(
    mut store: Store,
    endpoint: Endpoint,
    listener: tokio::net::TcpListener,
    settings: Settings,
    mut shutdown: watch::Receiver<bool>,
    mut commands: tokio_mpsc::Receiver<Command>,
    events: tokio_mpsc::UnboundedSender<Event>,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let (piv_timeout_tx, mut piv_timeout_rx) = tokio_mpsc::channel::<Command>(8);
    let mut tracer = settings.trace.then(Tracer::default);
    let mut gate = PairingGate::new();
    let mut handshakes = tokio::task::JoinSet::new();
    // reader -> service (frames the reader wants on the phone stream)
    let (to_service, mut from_reader) = tokio_mpsc::channel::<Frame>(8);
    // service -> reader (phone frames: Card/Response/Failed)
    let (to_reader, from_phone) = tokio_mpsc::channel::<Frame>(8);
    let reader_task = tokio::spawn(crate::serve(
        listener,
        from_phone,
        to_service,
        shutdown.clone(),
    ));
    // Frames received from the phone over iroh.
    let (received_tx, mut received) = tokio_mpsc::channel::<io::Result<Frame>>(8);
    let mut active: Option<(Connection, SendStream, tokio::task::JoinHandle<()>)> = None;
    let mut card_present = false;
    // The current card's stable UID (empty: none), the remembered cards and
    // the passive capability probe over the PC's own APDUs.
    let mut card_uid: Vec<u8> = Vec::new();
    let mut known = KnownCards::open(&settings.state_dir);
    let mut probe = CapabilityProbe::default();
    let mut pending_sync: Vec<CardInfo> = Vec::new();
    // Latest card state: generation + ATR (latest-wins, like the reader's
    // `Shared::latest_card`). Reported in Status; the macOS IFD path also
    // tags PIV APDUs with the generation.
    let mut card_generation: u64 = 0;
    let mut latest_atr: Vec<u8> = Vec::new();
    // One PIV APDU in flight (size-1, like the reader's `InFlight`).
    #[cfg(target_os = "macos")]
    let mut piv_in_flight: Option<PivInFlight> = None;
    // PIV timeouts re-enter the loop as commands (tokio::select! cannot hold
    // cfg'd arms). `Option` so shutdown is not blocked by an idle sender.
    #[cfg(target_os = "macos")]
    let piv_timeout_tx: tokio_mpsc::Sender<Command> = piv_timeout_tx;
    #[cfg(target_os = "macos")]
    let mut piv_ids: u64 = 0;
    // The watchdog timer's senders live in the spawned tasks; the receiver
    // is drained after each loop iteration (deadline logic tolerates the
    // small delay, and no cfg'd select arm is needed).
    #[cfg(target_os = "macos")]
    let drain_piv_timeouts =
        |piv_in_flight: &mut Option<PivInFlight>,
         piv_timeout_rx: &mut tokio_mpsc::Receiver<Command>| {
            while let Ok(command) = piv_timeout_rx.try_recv() {
                if let Command::PivTimeout { apdu_id } = command
                    && piv_in_flight
                        .as_ref()
                        .is_some_and(|inf| inf.apdu_id == apdu_id)
                    && let Some(inf) = piv_in_flight.take()
                {
                    let _ = inf.reply.send(Err(crate::ifd::TransmitError::Other));
                }
            }
        };
    let result = async {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        Command::MintInvitation(reply) => {
                            let _ = reply.send(gate.mint(endpoint.id()));
                        },
                        Command::ForgetPhone(reply) => {
                            store.set_peer(None)?;
                            if active.is_some() {
                                disconnect(&mut active, &mut received, &to_reader, &events, &mut card_present).await?;
                            }
                            let _ = reply.send(());
                        },
                        Command::AuthState(state) => {
                            if active.is_some() {
                                let _ = send_frame(
                                    &mut active, &mut received, &to_reader, &events,
                                    &mut card_present, Frame::Auth { state }, tracer.as_mut(),
                                ).await?;
                            }
                        },
                        Command::RequestTap(reply) => {
                            let outcome = if active.is_some() {
                                let sent = send_frame(
                                    &mut active, &mut received, &to_reader, &events,
                                    &mut card_present, Frame::ReadCard, tracer.as_mut(),
                                ).await?;
                                if sent { TapOutcome::ReadCardSent } else { session_outcome(&store, &endpoint) }
                            } else {
                                session_outcome(&store, &endpoint)
                            };
                            let _ = reply.send(outcome);
                        },
                        Command::EndpointAddr(reply) => {
                            let _ = reply.send(endpoint.addr());
                        },
                        Command::Status(reply) => {
                            let _ = reply.send(Status {
                                paired: store.peer.is_some(),
                                phone_connected: active.is_some(),
                                phone_name: store.peer_name.clone(),
                                card_present,
                                push_stored: store.push_token.is_some(),
                                atr: if card_present { latest_atr.clone() } else { Vec::new() },
                                card_generation,
                                card_uid: if card_present { card_uid.clone() } else { Vec::new() },
                            });
                        },
                        Command::KnownCards(reply) => {
                            let _ = reply.send(known.list());
                        },
                        Command::EditCard { uid, edit, reply } => {
                            let now = tapbridge_core::cards::now_ms();
                            let atr = if card_present && card_uid == uid { latest_atr.clone() } else { Vec::new() };
                            let changed = match edit {
                                CardEdit::Add(label) => known.add(&uid, &label, &atr, now),
                                CardEdit::Rename(label) => known.rename(&uid, &label, now),
                                CardEdit::SetWake(wake) => known.set_wake(&uid, wake, now),
                                CardEdit::SetLogin(paired) => known.set_login(&uid, paired, now),
                                CardEdit::NoteCaps(seen) => known.note_caps(&uid, seen, now),
                                CardEdit::Forget => known.forget(&uid, now),
                            }
                            .cloned();
                            if let Some(info) = &changed {
                                let _ = known.save();
                                let _ = events.send(Event::CardsChanged);
                                if active.is_some() {
                                    send_frame(
                                        &mut active, &mut received, &to_reader, &events,
                                        &mut card_present, Frame::CardInfo(info.clone()), tracer.as_mut(),
                                    ).await?;
                                }
                            }
                            let _ = reply.send(changed.is_some());
                        },
                        Command::CardFrame { frame, reply } => {
                            // Checked here: an invalid frame would fail the
                            // write and drop the session.
                            let valid = match &frame {
                                Frame::CardAction { uid, action } | Frame::CardActionDone { uid, action, .. } => {
                                    uid_stable(uid) && uid.len() <= wire::MAX_UID && (1..=wire::action::MAX).contains(action)
                                }
                                _ => false,
                            };
                            let sent = valid
                                && active.is_some()
                                && send_frame(
                                    &mut active, &mut received, &to_reader, &events,
                                    &mut card_present, frame, tracer.as_mut(),
                                ).await?;
                            let _ = reply.send(sent);
                        },
                        #[cfg(target_os = "macos")]
                        Command::PivTransmit { apdu, deadline, reply } => {
                            // Mirrors the usbip reader's XfrBlock guards: one
                            // in flight, card present + valid ATR, session
                            // live, APDU within wire bounds.
                            let early: Option<PivResult> = if piv_in_flight.is_some() {
                                Some(Err(crate::ifd::TransmitError::Busy))
                            } else if !card_present {
                                Some(Err(crate::ifd::TransmitError::NoCard))
                            } else if active.is_none() {
                                Some(Err(crate::ifd::TransmitError::NoSession))
                            } else if !(4..=tapbridge_core::wire::MAX_APDU).contains(&apdu.len()) {
                                Some(Err(crate::ifd::TransmitError::InvalidApdu))
                            } else {
                                None
                            };
                            match early {
                                Some(result) => { let _ = reply.send(result); },
                                None => {
                                    let apdu_id = piv_ids;
                                    piv_ids = piv_ids.wrapping_add(1);
                                    probe.request(apdu_id, &apdu);
                                    let frame = Frame::Apdu { id: apdu_id, generation: card_generation, data: apdu };
                                    let sent = send_frame(
                                        &mut active, &mut received, &to_reader, &events,
                                        &mut card_present, frame, tracer.as_mut(),
                                    ).await?;
                                    if sent {
                                        piv_in_flight = Some(PivInFlight {
                                            apdu_id,
                                            generation: card_generation,
                                            reply,
                                        });
                                        // Deadline watchdog: re-enters the
                                        // loop through the dedicated timeout
                                        // channel (drained after each event);
                                        // harmless when the APDU completed
                                        // first. The task drops its sender
                                        // after firing.
                                        let watchdog = piv_timeout_tx.clone();
                                        tokio::spawn(async move {
                                            tokio::time::sleep(deadline + Duration::from_millis(100)).await;
                                            let _ = watchdog.send(Command::PivTimeout { apdu_id }).await;
                                        });
                                    } else {
                                        let _ = reply.send(Err(crate::ifd::TransmitError::NoSession));
                                    }
                                },
                            }
                        },
                        #[cfg(target_os = "macos")]
                        Command::PivTimeout { apdu_id: _ } => {
                            // Handled by `drain_piv_timeouts` after each loop
                            // iteration; reaching the commands channel is a
                            // misroute, so it is ignored here.
                        },
                    }
                },
                // Reader asked to put a frame on the phone stream (Apdu, ReadCard).
                frame = from_reader.recv() => {
                    let Some(frame) = frame else { break };
                    if let Frame::Apdu { id, data, .. } = &frame {
                        probe.request(*id, data);
                    }
                    let _ = send_frame(
                        &mut active, &mut received, &to_reader, &events,
                        &mut card_present, frame, tracer.as_mut(),
                    ).await?;
                },
                connection = endpoint.accept() => {
                    let Some(connection) = connection else { break };
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
                    let Some(Ok(Ok((conn, mut send, recv, Frame::Hello { capability, name, push })))) = completed else { continue; };
                    let peer = conn.remote_id();
                    // The paired phone runs one session at a time, so a new
                    // authenticated connection from it means the old one is
                    // dead (app restarted, network switched) even if QUIC has
                    // not noticed yet. Replace it instead of making the phone
                    // wait out the idle timeout. Any other peer is still
                    // refused while a session is live.
                    if active.is_some() && store.peer == Some(peer) {
                        disconnect(&mut active, &mut received, &to_reader, &events, &mut card_present).await?;
                    }
                    let (authorized, paired_now) = if active.is_some() { (false, false) }
                    else if store.peer == Some(peer) {
                        // The paired phone scanned a fresh code: consume it so
                        // the pairing window completes. A stale or missing
                        // capability is still a normal pinned reconnect.
                        match capability.as_ref().map(|capability| gate.authorize(peer, Some(capability))) {
                            Some(Ok(pending)) => {
                                gate.commit(pending).map_err(io::Error::other)?;
                                (true, true)
                            }
                            _ => (true, false),
                        }
                    }
                    else if store.peer.is_some() { (false, false) }
                    else {
                        match gate.authorize(peer, capability.as_ref()) {
                            Ok(pending) => {
                                store.set_peer(Some(peer))?;
                                store.set_phone_info(None, Some(&name), push.as_deref())?;
                                gate.commit(pending).map_err(io::Error::other)?;
                                (true, true)
                            },
                            Err(_) => (false, false),
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
                    if paired_now {
                        let _ = events.send(Event::PairingCompleted { name: name.clone() });
                    }
                    if let Err(error) = store.set_phone_info(None, Some(&name), push.as_deref()) {
                        transport::close(&conn);
                        return Err(error);
                    }
                    if !matches!(tokio::time::timeout(Duration::from_secs(2), transport::reply_accepted(&mut send, settings.host_name.clone())).await, Ok(Ok(()))) {
                        transport::close(&conn);
                        continue;
                    }
                    let tx = received_tx.clone();
                    let reader = tokio::spawn(async move {
                        let mut recv = recv;
                        loop {
                            let frame = wire::read_frame(&mut recv).await;
                            let failed = frame.is_err();
                            if tx.send(frame).await.is_err() || failed { break; }
                        }
                    });
                    active = Some((conn, send, reader));
                    let _ = events.send(Event::PhoneConnected { name: name.clone() });
                    let _ = events.send(Event::CardPresent { present: false, uid: Vec::new() });
                    // Sync: every record this PC has, forgotten ones too.
                    let records: Vec<CardInfo> = known.records().cloned().collect();
                    for info in records {
                        if !send_frame(
                            &mut active, &mut received, &to_reader, &events,
                            &mut card_present, Frame::CardInfo(info), tracer.as_mut(),
                        ).await? {
                            break;
                        }
                    }
                },
                event = received.recv(), if active.is_some() => {
                    match event {
                        Some(Ok(frame @ (Frame::Card { .. } | Frame::Response { .. } | Frame::Failed { .. }))) => {
                            if let Some(line) = tracer.as_mut().and_then(|t| t.observe(&frame)) { let _ = events.send(Event::Trace(line)); }
                            if let Frame::Card { generation, atr, uid, .. } = &frame {
                                card_present = !atr.is_empty();
                                probe.reset();
                                card_uid = if card_present && uid_stable(uid) { uid.clone() } else { Vec::new() };
                                if card_present && !card_uid.is_empty() {
                                    if known.see(&card_uid, atr, tapbridge_core::cards::now_ms()) {
                                        let _ = known.save();
                                        let _ = events.send(Event::CardsChanged);
                                    } else {
                                        let _ = events.send(Event::UnknownCard {
                                            uid: card_uid.clone(),
                                            label: known.default_label(atr),
                                        });
                                    }
                                }
                                card_generation = *generation;
                                latest_atr.clone_from(atr);
                                let _ = events.send(Event::CardPresent { present: card_present, uid: card_uid.clone() });
                                // Card absence/replacement retires a pending
                                // PIV APDU (never carried to the next tag),
                                // mirroring the reader.
                                #[cfg(target_os = "macos")]
                                if let Some(inf) = piv_in_flight.take() {
                                    if inf.generation != *generation || atr.is_empty() {
                                        let _ = inf.reply.send(Err(crate::ifd::TransmitError::NoCard));
                                    } else {
                                        piv_in_flight = Some(inf);
                                    }
                                }
                            }
                            match &frame {
                                Frame::Response { id, data, .. } => {
                                    let seen = probe.response(*id, data);
                                    if seen != 0
                                        && let Some(info) = known.note_caps(&card_uid, seen, tapbridge_core::cards::now_ms()).cloned()
                                    {
                                        let _ = known.save();
                                        let _ = events.send(Event::CardsChanged);
                                        // Queued for the phone after this frame is handled.
                                        pending_sync.push(info);
                                    }
                                }
                                Frame::Failed { id, .. } => probe.failed(*id),
                                _ => {}
                            }
                            #[cfg(target_os = "macos")]
                            if let Frame::Response { id, generation, data } = &frame
                                && piv_in_flight.as_ref().is_some_and(|inf| inf.apdu_id == *id && inf.generation == *generation)
                                    && let Some(inf) = piv_in_flight.take()
                                {
                                    let _ = inf.reply.send(Ok(data.clone()));
                                }
                            #[cfg(target_os = "macos")]
                            if let Frame::Failed { id, generation } = &frame
                                && piv_in_flight.as_ref().is_some_and(|inf| inf.apdu_id == *id && inf.generation == *generation)
                                    && let Some(inf) = piv_in_flight.take()
                                {
                                    let _ = inf.reply.send(Err(crate::ifd::TransmitError::Other));
                                }
                            tokio::select! {
                                result = to_reader.send(frame) => result.map_err(|_| io::Error::other("reader stopped"))?,
                                _ = shutdown.changed() => break,
                            }
                        },
                        Some(Ok(Frame::CardInfo(theirs))) => {
                            let (reply, changed) = known.merge(&theirs);
                            if changed {
                                let _ = known.save();
                                let _ = events.send(Event::CardsChanged);
                            }
                            if let Some(merged) = reply {
                                send_frame(
                                    &mut active, &mut received, &to_reader, &events,
                                    &mut card_present, Frame::CardInfo(merged), tracer.as_mut(),
                                ).await?;
                            }
                        },
                        Some(Ok(Frame::CardAction { uid, action })) => {
                            let _ = events.send(Event::CardActionRequested { uid, action });
                        },
                        Some(Ok(Frame::CardActionDone { uid, action, ok })) => {
                            let _ = events.send(Event::CardActionDone { uid, action, ok });
                        },
                        _ => disconnect(&mut active, &mut received, &to_reader, &events, &mut card_present).await?,
                    }
                },
            }
            // Caps learned from the PC's own traffic go to the phone too.
            for info in std::mem::take(&mut pending_sync) {
                if active.is_some() {
                    send_frame(
                        &mut active, &mut received, &to_reader, &events,
                        &mut card_present, Frame::CardInfo(info), tracer.as_mut(),
                    ).await?;
                }
            }
            // PIV deadline watchdog: drain fired timers after each event.
            #[cfg(target_os = "macos")]
            drain_piv_timeouts(&mut piv_in_flight, &mut piv_timeout_rx);
        }
        Ok(())
    }.await;
    if let Some((conn, _, reader)) = active {
        transport::close(&conn);
        reader.abort();
        let _ = reader.await;
    }
    reader_task.abort();
    let _ = reader_task.await;
    handshakes.abort_all();
    while handshakes.join_next().await.is_some() {}
    endpoint.close().await;
    result
}

/// The tap outcome when no session is live.
fn session_outcome(store: &Store, endpoint: &Endpoint) -> TapOutcome {
    if store.peer.is_none() {
        TapOutcome::NotPaired
    } else if let Some(token) = store.push_token.clone() {
        TapOutcome::Wake {
            token,
            hint: Hint::from_endpoint_addr(&endpoint.addr()),
        }
    } else {
        TapOutcome::NoPush
    }
}

async fn disconnect(
    active: &mut Option<(Connection, SendStream, tokio::task::JoinHandle<()>)>,
    received: &mut tokio_mpsc::Receiver<io::Result<Frame>>,
    to_reader: &tokio_mpsc::Sender<Frame>,
    events: &tokio_mpsc::UnboundedSender<Event>,
    card_present: &mut bool,
) -> io::Result<()> {
    if let Some((conn, _, reader)) = active.take() {
        transport::close(&conn);
        reader.abort();
        let _ = reader.await;
    }
    while received.try_recv().is_ok() {}
    tokio::time::timeout(
        Duration::from_secs(2),
        to_reader.send(Frame::Card {
            generation: 0,
            atr: vec![],
            max_apdu: 0,
            uid: vec![],
        }),
    )
    .await
    .map_err(|_| io::Error::other("reader stalled"))?
    .map_err(|_| io::Error::other("reader stopped"))?;
    *card_present = false;
    let _ = events.send(Event::CardPresent {
        present: false,
        uid: Vec::new(),
    });
    let _ = events.send(Event::PhoneDisconnected);
    Ok(())
}

/// Writes one frame to the live session, disconnecting on failure. Returns
/// whether the frame reached the stream.
async fn send_frame(
    active: &mut Option<(Connection, SendStream, tokio::task::JoinHandle<()>)>,
    received: &mut tokio_mpsc::Receiver<io::Result<Frame>>,
    to_reader: &tokio_mpsc::Sender<Frame>,
    events: &tokio_mpsc::UnboundedSender<Event>,
    card_present: &mut bool,
    frame: Frame,
    tracer: Option<&mut Tracer>,
) -> io::Result<bool> {
    if let Some(line) = tracer.and_then(|t| t.observe(&frame)) {
        let _ = events.send(Event::Trace(line));
    }
    if let Some((_, send, _)) = active.as_mut() {
        let sent =
            tokio::time::timeout(Duration::from_secs(5), wire::write_frame(send, &frame)).await;
        if !matches!(sent, Ok(Ok(()))) {
            disconnect(active, received, to_reader, events, card_present).await?;
            return Ok(false);
        }
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{EndpointId, SecretKey};
    use tapbridge_core::{ALPN, transport::ClientHello};

    fn settings(dir: &std::path::Path, trace: bool) -> Settings {
        Settings {
            state_dir: dir.to_path_buf(),
            usbip_addr: "127.0.0.1:0".parse().unwrap(),
            host_name: "Test PC".into(),
            trace,
        }
    }

    async fn phone_endpoint() -> (Endpoint, EndpointId) {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(SecretKey::generate())
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let id = endpoint.id();
        (endpoint, id)
    }

    /// Connects a phone with the given Hello and returns the reply plus the
    /// connection streams.
    async fn connect_phone(
        host_addr: &iroh::EndpointAddr,
        hello: ClientHello,
    ) -> (
        transport::HelloReply,
        iroh::endpoint::SendStream,
        iroh::endpoint::RecvStream,
        Endpoint,
        Connection,
    ) {
        let (endpoint, _) = phone_endpoint().await;
        let conn = endpoint.connect(host_addr.clone(), ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reply = transport::client_hello(&mut send, &mut recv, hello)
            .await
            .unwrap();
        (reply, send, recv, endpoint, conn)
    }

    fn drain(handle: &ServiceHandle) -> Vec<Event> {
        handle.drain_events()
    }

    /// Buffered event waiter: keeps non-matching events for later waits.
    struct Collector {
        handle: std::sync::Arc<ServiceHandle>,
        seen: Vec<Event>,
    }

    impl Collector {
        async fn wait(&mut self, pred: impl Fn(&Event) -> bool) -> Event {
            for _ in 0..200 {
                self.seen.extend(drain(&self.handle));
                if let Some(index) = self.seen.iter().position(&pred) {
                    return self.seen.remove(index);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("event never arrived; seen so far: {:?}", self.seen);
        }
    }

    async fn paired_service(
        dir: &std::path::Path,
        name: &str,
        push: Option<&str>,
    ) -> (std::sync::Arc<ServiceHandle>, Collector, PhoneSide) {
        let handle =
            std::sync::Arc::new(crate::service::start(settings(dir, false)).await.unwrap());
        let host_addr = handle.endpoint_addr().unwrap();
        let invitation = handle.mint_invitation().unwrap();
        let (reply, send, recv, endpoint, conn) = connect_phone(
            &host_addr,
            ClientHello {
                capability: Some(invitation.capability()),
                name: name.into(),
                push: push.map(str::to_owned),
            },
        )
        .await;
        assert_eq!(
            reply,
            transport::HelloReply::Accepted {
                host_name: "Test PC".into(),
                platform: wire::platform::current(),
                piv_login: wire::platform::piv_login(),
            }
        );
        let mut collector = Collector {
            handle: handle.clone(),
            seen: Vec::new(),
        };
        collector
            .wait(|event| matches!(event, Event::PairingCompleted { .. }))
            .await;
        // The session is registered as active only after the accept handshake
        // completes; wait for PhoneConnected so tests that call request_tap
        // see a live session.
        let expected_name = name.to_owned();
        collector
            .wait(|event| matches!(event, Event::PhoneConnected { name } if *name == expected_name))
            .await;
        (
            handle,
            collector,
            PhoneSide {
                endpoint,
                conn,
                send,
                recv,
            },
        )
    }

    /// The phone half of a paired session. Keep it alive for the whole test:
    /// dropping the streams sends STOP_SENDING/RESET and kills the session.
    struct PhoneSide {
        endpoint: Endpoint,
        conn: Connection,
        #[allow(dead_code)]
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pairing_persists_name_and_token_and_accepted_carries_host_name() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, _collector, phone) =
            paired_service(dir.path(), "Pixel 9", Some("fcm-token-1")).await;
        assert!(handle.status().paired);
        assert!(handle.status().phone_connected);
        assert!(handle.status().push_stored);
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
        let store = Store::open(dir.path()).unwrap();
        assert!(store.peer.is_some());
        assert_eq!(store.peer_name.as_deref(), Some("Pixel 9"));
        assert_eq!(store.push_token.as_deref(), Some("fcm-token-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_tap_on_fresh_service_reports_not_paired() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let handle = crate::service::start(settings(dir.path(), false))
            .await
            .unwrap();
        assert_eq!(handle.request_tap(), TapOutcome::NotPaired);
        handle.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_tap_without_token_reports_no_push_once_paired() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, phone) = paired_service(dir.path(), "Pixel", None).await;
        transport::close(&phone.conn);
        collector
            .wait(|event| matches!(event, Event::PhoneDisconnected))
            .await;
        assert_eq!(handle.request_tap(), TapOutcome::NoPush);
        handle.shutdown();
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_state_reaches_the_connected_phone() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, _collector, phone) = paired_service(dir.path(), "Pixel", Some("tok")).await;
        let mut phone = phone;
        handle.send_auth_state(wire::auth::SUCCESS);
        let frame = tokio::time::timeout(Duration::from_secs(5), wire::read_frame(&mut phone.recv))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            frame,
            Frame::Auth {
                state: wire::auth::SUCCESS
            }
        );
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }

    /// Each tap is a new card generation; the security key compares them
    /// to tell a re-tap (new PIN key agreement on the card) from the same
    /// tap.
    #[tokio::test(flavor = "multi_thread")]
    async fn status_exposes_the_card_generation_of_each_tap() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, phone) = paired_service(dir.path(), "Pixel", None).await;
        let mut phone = phone;
        for generation in [7u64, 8] {
            wire::write_frame(
                &mut phone.send,
                &Frame::Card {
                    generation,
                    atr: vec![0x3B, 0x80, 0x80, 0x01, 0x01],
                    max_apdu: 255,
                    uid: vec![],
                },
            )
            .await
            .unwrap();
            collector
                .wait(|event| matches!(event, Event::CardPresent { present: true, .. }))
                .await;
            let status = handle.status();
            assert!(status.card_present);
            assert_eq!(status.card_generation, generation);
        }
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_tap_sends_read_card_on_live_session() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, _collector, phone) = paired_service(dir.path(), "Pixel", Some("tok")).await;
        let mut phone = phone;
        assert_eq!(handle.request_tap(), TapOutcome::ReadCardSent);
        let frame = tokio::time::timeout(Duration::from_secs(5), wire::read_frame(&mut phone.recv))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame, Frame::ReadCard);
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_tap_without_session_returns_stored_token_and_hint() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, phone) =
            paired_service(dir.path(), "Pixel", Some("fcm-token-1")).await;
        let expected_hint = Hint::from_endpoint_addr(&handle.endpoint_addr().unwrap());
        transport::close(&phone.conn);
        collector
            .wait(|event| matches!(event, Event::PhoneDisconnected))
            .await;
        collector
            .wait(|event| matches!(event, Event::CardPresent { present: false, .. }))
            .await;
        assert_eq!(
            handle.request_tap(),
            TapOutcome::Wake {
                token: "fcm-token-1".into(),
                hint: expected_hint
            }
        );
        handle.shutdown();
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn paired_phone_scanning_a_fresh_code_completes_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, phone) = paired_service(dir.path(), "Pixel", None).await;
        let host_addr = handle.endpoint_addr().unwrap();
        transport::close(&phone.conn);
        collector
            .wait(|event| matches!(event, Event::PhoneDisconnected))
            .await;
        // Same phone identity, fresh invitation: PairingCompleted again.
        let invitation = handle.mint_invitation().unwrap();
        let conn = phone
            .endpoint
            .connect(host_addr.clone(), ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            transport::client_hello(
                &mut send,
                &mut recv,
                ClientHello {
                    capability: Some(invitation.capability()),
                    name: "Pixel".into(),
                    push: None,
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(reply, transport::HelloReply::Accepted { .. }));
        collector
            .wait(|event| matches!(event, Event::PairingCompleted { name } if name == "Pixel"))
            .await;
        // The code was consumed: a replay is a plain reconnect, no new pairing.
        transport::close(&conn);
        collector
            .wait(|event| matches!(event, Event::PhoneDisconnected))
            .await;
        let conn = phone.endpoint.connect(host_addr, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reply = transport::client_hello(
            &mut send,
            &mut recv,
            ClientHello {
                capability: Some(invitation.capability()),
                name: "Pixel".into(),
                push: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(reply, transport::HelloReply::Accepted { .. }));
        collector
            .wait(|event| matches!(event, Event::PhoneConnected { .. }))
            .await;
        collector.seen.extend(handle.drain_events());
        assert!(
            !collector
                .seen
                .iter()
                .any(|event| matches!(event, Event::PairingCompleted { .. }))
        );
        handle.shutdown();
        transport::close(&conn);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forget_phone_revokes_pairing_and_rejects_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, _collector, phone) = paired_service(dir.path(), "Pixel", None).await;
        handle.forget_phone();
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
        assert!(Store::open(dir.path()).unwrap().peer.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn paired_phone_redialing_replaces_its_stale_session() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, phone) = paired_service(dir.path(), "Pixel", None).await;
        let host_addr = handle.endpoint_addr().unwrap();
        // The old session is still open on the host (the phone app was killed
        // without closing it). The same identity dials again with a token.
        let conn = phone.endpoint.connect(host_addr, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            transport::client_hello(
                &mut send,
                &mut recv,
                ClientHello {
                    capability: None,
                    name: "Pixel".into(),
                    push: Some("fresh-token".into()),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(reply, transport::HelloReply::Accepted { .. }));
        collector
            .wait(|event| matches!(event, Event::PhoneDisconnected))
            .await;
        collector
            .wait(|event| matches!(event, Event::PhoneConnected { .. }))
            .await;
        let status = handle.status();
        assert!(status.phone_connected && status.push_stored);
        // The new session is the live one: tap requests reach it.
        assert_eq!(handle.request_tap(), TapOutcome::ReadCardSent);
        let frame = tokio::time::timeout(Duration::from_secs(5), wire::read_frame(&mut recv))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame, Frame::ReadCard);
        handle.shutdown();
        transport::close(&conn);
        drop(phone);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn second_connection_while_session_live_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, _collector, paired) = paired_service(dir.path(), "Pixel", None).await;
        let host_addr = handle.endpoint_addr().unwrap();
        // A different identity while the paired phone's session is live:
        // refused, and the live session is not replaced.
        let (phone, _) = phone_endpoint().await;
        let second = phone.connect(host_addr, ALPN).await.unwrap();
        let (mut send, mut recv) = second.open_bi().await.unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            transport::client_hello(
                &mut send,
                &mut recv,
                ClientHello {
                    capability: None,
                    name: "Pixel".into(),
                    push: None,
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reply, transport::HelloReply::Rejected);
        handle.shutdown();
        transport::close(&paired.conn);
        drop(paired);
        drop(phone);
    }

    async fn send_card(phone: &mut PhoneSide, generation: u64, uid: &[u8]) {
        wire::write_frame(
            &mut phone.send,
            &Frame::Card {
                generation,
                atr: vec![0x3B, 0x80, 0x80, 0x01, 0x01],
                max_apdu: 255,
                uid: uid.to_vec(),
            },
        )
        .await
        .unwrap();
    }

    const IMPLANT: [u8; 7] = [0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

    async fn read_phone_frame(phone: &mut PhoneSide) -> Frame {
        tokio::time::timeout(Duration::from_secs(5), wire::read_frame(&mut phone.recv))
            .await
            .unwrap()
            .unwrap()
    }

    /// Reads frames until one matches (skipping sync records and the like).
    async fn read_until(phone: &mut PhoneSide, wanted: impl Fn(&Frame) -> bool) -> Frame {
        loop {
            let frame = read_phone_frame(phone).await;
            if wanted(&frame) {
                return frame;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_cards_are_offered_and_added_ones_sync_to_the_phone() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, mut phone) = paired_service(dir.path(), "Pixel", None).await;
        send_card(&mut phone, 1, &IMPLANT).await;
        collector
            .wait(|event| matches!(event, Event::UnknownCard { uid, label } if uid == &IMPLANT && label == "Card 1"))
            .await;
        assert!(
            handle.known_cards().is_empty(),
            "a tap alone doesn't add it"
        );
        // A random UID is reported as none and never offered.
        send_card(&mut phone, 2, &[0x08, 1, 2, 3]).await;
        collector
            .wait(|event| matches!(event, Event::CardPresent { present: true, uid } if uid.is_empty()))
            .await;
        // Adding it sends the record to the phone.
        assert!(handle.edit_card(IMPLANT.to_vec(), CardEdit::Add("Hand".into())));
        let Frame::CardInfo(info) =
            read_until(&mut phone, |f| matches!(f, Frame::CardInfo(_))).await
        else {
            unreachable!()
        };
        assert_eq!(
            (info.uid.as_slice(), info.label.as_str(), info.wake),
            (&IMPLANT[..], "Hand", true)
        );
        assert!(handle.edit_card(IMPLANT.to_vec(), CardEdit::Rename("Implant".into())));
        assert!(
            !handle.edit_card(IMPLANT.to_vec(), CardEdit::Rename("Implant".into())),
            "no change"
        );
        assert_eq!(
            crate::cards::KnownCards::open(dir.path()).list()[0]
                .info
                .label,
            "Implant"
        );
        assert!(handle.edit_card(IMPLANT.to_vec(), CardEdit::Forget));
        assert!(handle.known_cards().is_empty());
        let Frame::CardInfo(info) = read_until(
            &mut phone,
            |f| matches!(f, Frame::CardInfo(i) if i.deleted()),
        )
        .await
        else {
            unreachable!()
        };
        assert_eq!(info.uid, IMPLANT);
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn phone_records_merge_and_the_newer_copy_goes_back() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, mut phone) = paired_service(dir.path(), "Pixel", None).await;
        assert!(handle.edit_card(IMPLANT.to_vec(), CardEdit::Add("PC name".into())));
        assert!(handle.edit_card(IMPLANT.to_vec(), CardEdit::SetLogin(true)));
        // The phone renamed it (later) and read its caps, but doesn't know
        // about the login.
        let mut theirs = tapbridge_core::cards::CardInfo::new(
            IMPLANT.to_vec(),
            "Phone name".into(),
            true,
            crate::cards::caps::FIDO,
            tapbridge_core::cards::now_ms() + 60_000,
        );
        theirs.label_at += 1;
        wire::write_frame(&mut phone.send, &Frame::CardInfo(theirs.clone()))
            .await
            .unwrap();
        collector
            .wait(|event| matches!(event, Event::CardsChanged))
            .await;
        let Frame::CardInfo(back) = read_until(
            &mut phone,
            |f| matches!(f, Frame::CardInfo(i) if i.label == "Phone name"),
        )
        .await
        else {
            unreachable!()
        };
        assert!(back.login, "the PC's newer field goes back");
        let card = handle.known_cards().remove(0);
        assert_eq!(card.info.label, "Phone name");
        assert!(card.info.has(crate::cards::caps::FIDO) && card.info.login);
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn card_actions_travel_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, mut phone) = paired_service(dir.path(), "Pixel", None).await;
        assert!(handle.request_card_action(IMPLANT.to_vec(), wire::action::SETUP_CERTS));
        assert_eq!(
            read_until(&mut phone, |f| matches!(f, Frame::CardAction { .. })).await,
            Frame::CardAction {
                uid: IMPLANT.to_vec(),
                action: wire::action::SETUP_CERTS
            }
        );
        wire::write_frame(
            &mut phone.send,
            &Frame::CardActionDone {
                uid: IMPLANT.to_vec(),
                action: wire::action::SETUP_CERTS,
                ok: true,
            },
        )
        .await
        .unwrap();
        collector
            .wait(|event| matches!(event, Event::CardActionDone { ok: true, action, .. } if *action == wire::action::SETUP_CERTS))
            .await;
        // The phone asks the PC to set up its login; the PC answers.
        wire::write_frame(
            &mut phone.send,
            &Frame::CardAction {
                uid: IMPLANT.to_vec(),
                action: wire::action::SETUP_LOGIN,
            },
        )
        .await
        .unwrap();
        collector
            .wait(|event| matches!(event, Event::CardActionRequested { action, .. } if *action == wire::action::SETUP_LOGIN))
            .await;
        assert!(handle.card_action_done(IMPLANT.to_vec(), wire::action::SETUP_LOGIN, false));
        assert_eq!(
            read_until(&mut phone, |f| matches!(f, Frame::CardActionDone { .. })).await,
            Frame::CardActionDone {
                uid: IMPLANT.to_vec(),
                action: wire::action::SETUP_LOGIN,
                ok: false
            }
        );
        assert!(
            !handle.request_card_action(vec![0x08, 1, 2, 3], wire::action::SETUP_CERTS),
            "random UID"
        );
        assert!(handle.status().phone_connected);
        transport::close(&phone.conn);
        collector
            .wait(|event| matches!(event, Event::PhoneDisconnected))
            .await;
        assert!(!handle.request_card_action(IMPLANT.to_vec(), wire::action::SETUP_CERTS));
        handle.shutdown();
        drop(phone.endpoint);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_connecting_phone_receives_every_record_including_forgotten_ones() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        {
            let mut cards = crate::cards::KnownCards::open(dir.path());
            cards.add(&IMPLANT, "Kept", &[], 1);
            cards.add(&[0x04, 9, 9, 9], "Gone", &[], 2);
            cards.forget(&[0x04, 9, 9, 9], 3);
            cards.save().unwrap();
        }
        let (handle, _collector, mut phone) = paired_service(dir.path(), "Pixel", None).await;
        let mut seen = Vec::new();
        while seen.len() < 2 {
            if let Frame::CardInfo(info) = read_phone_frame(&mut phone).await {
                seen.push((info.label.clone(), info.deleted()));
            }
        }
        seen.sort();
        assert_eq!(
            seen,
            [("Gone".to_owned(), true), ("Kept".to_owned(), false)]
        );
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }

    /// macOS: the PIV token's own SELECT and certificate read mark the
    /// card, without the service sending anything of its own.
    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    async fn piv_traffic_from_the_mac_marks_the_card_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let _session = crate::test_session();
        let (handle, mut collector, mut phone) = paired_service(dir.path(), "Pixel", None).await;
        assert!(handle.edit_card(IMPLANT.to_vec(), CardEdit::Add("Implant".into())));
        let _ = read_until(&mut phone, |f| matches!(f, Frame::CardInfo(_))).await;
        send_card(&mut phone, 3, &IMPLANT).await;
        collector
            .wait(|event| matches!(event, Event::CardPresent { present: true, .. }))
            .await;
        let exchanges: [(Vec<u8>, Vec<u8>); 2] = [
            (
                vec![0x00, 0xA4, 0x04, 0x00, 5, 0xA0, 0, 0, 3, 8, 0],
                vec![0x61, 0x11],
            ),
            (
                vec![0x00, 0xCB, 0x3F, 0xFF, 5, 0x5C, 3, 0x5F, 0xC1, 0x05, 0],
                vec![0x53, 0x01, 0x00, 0x90, 0x00],
            ),
        ];
        for (apdu, answer) in exchanges {
            let worker = {
                let handle = handle.clone();
                let apdu = apdu.clone();
                std::thread::spawn(move || handle.piv_transmit(apdu))
            };
            // Caps learned from the previous answer sync first.
            let frame = read_until(&mut phone, |f| matches!(f, Frame::Apdu { .. })).await;
            let Frame::Apdu {
                id,
                generation,
                data,
            } = frame
            else {
                panic!("expected an APDU, got {frame:?}");
            };
            // Only the Mac's own command goes to the card.
            assert_eq!(data, apdu);
            wire::write_frame(
                &mut phone.send,
                &Frame::Response {
                    id,
                    generation,
                    data: answer.clone(),
                },
            )
            .await
            .unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), answer);
        }
        let card = handle.known_cards().remove(0);
        assert!(
            card.info
                .has(crate::cards::caps::PIV | crate::cards::caps::CERTS)
        );
        assert!(!card.info.has(crate::cards::caps::FIDO));
        handle.shutdown();
        transport::close(&phone.conn);
        drop(phone.endpoint);
    }
}
