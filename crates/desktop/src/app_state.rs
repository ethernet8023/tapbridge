//! App orchestration. The gpui main thread owns everything in [`Shell`] (tray,
//! service handle, windows, prompt watcher) as a gpui global; background
//! threads (host runtime, usbip, wake requests, single-instance signal,
//! prompt watcher) only send [`UiEvent`]s into one channel drained by `tick`.

#[cfg(target_os = "macos")]
use crate::macos;
use crate::{
    autostart,
    card_tasks::{CardTask, Effect},
    hostname, logging, login,
    tray::{MenuAction, Tray, TrayState},
    ui, wake,
};
#[cfg(windows)]
use crate::{prompt_watch, single, usbip};
use gpui::{App, AsyncApp, BorrowAppContext, Global, KeyBinding, WindowHandle};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use tapbridge_core::wire;
use tapbridge_host::{
    cards::{KnownCard, caps},
    service::{self, ServiceHandle, TapOutcome},
};

const USBIP_ADDR: &str = "127.0.0.1:3240";

// App-wide commands: bound to ⌘, / ⌘Q (Ctrl on Windows) and, on macOS, the
// app menu that owns those shortcuts while a tapbridge window is focused.
gpui::actions!(tapbridge, [OpenSettings, OpenAbout, Quit]);
const TICK: Duration = Duration::from_millis(100);
/// How long the tray shows "waiting for a tap" after a wake: the phone keeps
/// its tap gate open this long (DESIGN.md, wake step 4).
const TAP_WAIT: Duration = Duration::from_secs(120);
/// One wave of the waiting-for-tap animation is lit this long.
const TAP_FRAME: Duration = Duration::from_millis(400);

/// macOS placeholder for the Windows single-instance guard: the LaunchAgent
/// RunAtLoad path plus manual launches may still race; host.lock is the real
/// single-instance guarantee (service::start fails if another owns it).
#[cfg(not(windows))]
#[derive(Default)]
pub struct NoopInstance;

/// Everything the shell's event loop reacts to.
pub enum UiEvent {
    ServiceStarted(ServiceHandle),
    ServiceFailed(String),
    /// Windows started a WebAuthn ceremony.
    #[cfg(windows)]
    Ceremony,
    /// Another launch asked for a window.
    #[cfg(windows)]
    Open(single::Request),
    #[cfg(windows)]
    UsbipAttached(Result<(), String>),
    /// PC/SC (via the IFD shim) wants the card and no phone session is live.
    #[cfg(not(windows))]
    WakeRequested,
    Woke(wake::WakeResult),
    /// Setting up the card's login (see `login`) finished: (ok, message).
    LoginDone(bool, String),
    /// Whether this card's identity is paired with the login, read after a
    /// tap (`login::present_card_paired`).
    LoginChecked(Vec<u8>, bool),
    /// macOS: the smart card settings as read from the system.
    #[cfg(target_os = "macos")]
    SecurityRead(ui::SecurityOptions),
    /// macOS: an Apply finished; the text says how.
    #[cfg(target_os = "macos")]
    SecurityApplied(String),
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Usbip {
    Missing,
    Attaching,
    Attached,
    Failed,
}

struct Shell {
    events: mpsc::Sender<UiEvent>,
    state_dir: PathBuf,
    trace_apdu: bool,
    tray: Option<Tray>,
    service: Option<ServiceHandle>,
    #[cfg(windows)]
    usbip: Usbip,
    pairing: Option<WindowHandle<ui::PairingView>>,
    settings: Option<WindowHandle<ui::SettingsView>>,
    about: Option<WindowHandle<crate::about::AboutView>>,
    /// Start-at-login state, cached: Settings re-renders from it every tick.
    autostart: bool,
    /// macOS: the login card always shows as present (see
    /// [`crate::macos::AlwaysPresent`]).
    always_present: bool,
    /// macOS: the smart card settings, once read.
    security: Option<ui::Security>,
    /// Installed through nix-darwin: settings are read-only.
    managed: bool,
    /// Shown in the tray until the next state change (e.g. relay problems).
    notice: Option<String>,
    /// Why the host service failed to start, shown in Settings → Details.
    service_error: Option<String>,
    /// Set when a tap was requested (wake sent or phone reminded); cleared by
    /// a card, an error or [`TAP_WAIT`]. Drives the animated tray icon.
    waiting_since: Option<Instant>,
    /// Known cards, re-read from the service when they change.
    cards: Vec<KnownCard>,
    /// The card action running in Preferences, if any.
    card_task: Option<CardTask>,
    /// Cards the phone set up as smart cards this run: offer Mac login.
    setup_ok: HashSet<Vec<u8>>,
    /// A card tapped here for the first time, just added: name it once it
    /// shows up in the list. (Cards synced from the phone are named there.)
    new_card: Option<Vec<u8>>,
    /// The phone asked to set up the login of this card; answer it when the
    /// pairing ends.
    phone_asked_login: Option<Vec<u8>>,
    // Held for their Drop / process-lifetime handles.
    _anchor: WindowHandle<ui::Anchor>,
    #[cfg(windows)]
    watcher: Option<prompt_watch::Watcher>,
    #[cfg(windows)]
    _instance: single::Instance,
    #[cfg(not(windows))]
    _instance: NoopInstance,
}

impl Global for Shell {}

impl Shell {
    /// Builds the shell's instance guard storage (cfg-typed field).
    fn instance_guard(instance: impl InstanceGuard) -> ShellInstance {
        #[cfg(windows)]
        return instance.into_shell_instance();
        #[cfg(not(windows))]
        {
            let _ = instance;
            NoopInstance
        }
    }
}

/// The concrete instance-guard type stored in the shell (per platform).
#[cfg(windows)]
type ShellInstance = single::Instance;
#[cfg(not(windows))]
type ShellInstance = NoopInstance;

pub fn run(state_dir: PathBuf, trace_apdu: bool, instance: impl InstanceGuard) {
    let (events, inbox) = mpsc::channel();
    #[cfg(windows)]
    let watcher = {
        let ceremony = events.clone();
        prompt_watch::Watcher::start(move || {
            let _ = ceremony.send(UiEvent::Ceremony);
        })
        .inspect_err(|error| logging::log(&format!("prompt watcher unavailable: {error}")))
        .ok()
    };
    gpui_platform::application().run(move |cx: &mut App| {
        spawn_service(state_dir.clone(), trace_apdu, events.clone());
        #[cfg(windows)]
        shell_setup(cx, events, inbox, state_dir, trace_apdu, watcher, instance);
        #[cfg(not(windows))]
        shell_setup(cx, events, inbox, state_dir, trace_apdu, instance);
    });
}

/// `--preview-ui`: Settings and pairing windows with sample data, for design work.
/// Touches no state directory and starts no service.
pub fn preview_ui() {
    gpui_platform::application().run(|cx: &mut App| {
        let snapshot = crate::ui::SettingsSnapshot {
            summary: "Phone connected".into(),
            health: crate::ui::Health::Good,
            phone: Some("Pixel 9 Pro XL".into()),
            phone_connected: true,
            usb: true,
            use_usb: true,
            autostart: true,
            always_present: Some(false),
            security: cfg!(target_os = "macos").then(|| crate::ui::Security {
                live: crate::ui::SecurityOptions {
                    allow_smart_card: true,
                    user_pairing: true,
                    sudo: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            managed: std::env::var_os("TAPBRIDGE_PREVIEW_MANAGED").is_some(),
            cards: vec![
                {
                    let flex = [
                        0x3B, 0x8D, 0x80, 0x01, 0x00, 0x31, 0xC1, 0x73, 0xC8, 0x40, 0x00, 0x52,
                        0xA5, 0x10, 0x00, 0x90, 0x00, 0x70,
                    ];
                    let card = tapbridge_host::cards::CardInfo {
                        caps: caps::FIDO | caps::PIV | caps::CERTS,
                        ..Default::default()
                    };
                    crate::ui::CardRow {
                        uid: vec![4, 1, 2, 3, 4, 5, 6],
                        label: "Implant".into(),
                        last_seen: "now".into(),
                        passkey: true,
                        smart_card: true,
                        offer_login: login::SUPPORTED,
                        checklist: Some(crate::card_tasks::Checklist::new(&card)),
                        install_steps: crate::card_tasks::install_steps(&flex, false),
                        flexsecure: true,
                        ..Default::default()
                    }
                },
                crate::ui::CardRow {
                    uid: vec![4, 9, 9, 9, 9, 9, 9],
                    label: "Card 2".into(),
                    last_seen: "3 days ago".into(),
                    passkey: true,
                    checklist: Some(crate::card_tasks::Checklist::new(
                        &tapbridge_host::cards::CardInfo {
                            caps: caps::FIDO,
                            ..Default::default()
                        },
                    )),
                    install_steps: crate::card_tasks::install_steps(&[], false),
                    task: Some("Finish the setup on your phone.".into()),
                    task_dismissable: true,
                    ..Default::default()
                },
            ],
            card_busy: true,
        };
        let sample_card = snapshot.cards[0].clone();
        // Preview: links open; every other action is only logged (no service).
        let settings = crate::ui::open_settings_window(
            snapshot,
            None,
            |action, cx| match action {
                crate::ui::SettingsAction::OpenUrl(url) => cx.open_url(&url),
                other => eprintln!("preview: {}", other.name()),
            },
            cx,
        );
        let preview_action = |action: crate::ui::SettingsAction, cx: &mut App| match action {
            crate::ui::SettingsAction::OpenUrl(url) => cx.open_url(&url),
            other => eprintln!("preview: {}", other.name()),
        };
        // TAPBRIDGE_PREVIEW_DIALOG=new shows the "New card" dialog;
        // =paired the pairing window after its test card came through;
        // =about the About window.
        match std::env::var("TAPBRIDGE_PREVIEW_DIALOG").as_deref() {
            Ok("about") => {
                crate::about::open_about_window(
                    vec![
                        "Host: iris".into(),
                        "Reader: tapbridge virtual reader".into(),
                        "Relay: use1-1.relay.iroh.network".into(),
                    ],
                    None,
                    cx,
                );
            }
            Ok("new") => crate::ui::settings_new_card(settings, vec![4, 9, 9, 9, 9, 9, 9], cx),
            Ok("paired") => {
                let window = crate::ui::open_pairing_window(
                    "tapbridge-preview-invitation-0123456789abcdef",
                    true,
                    None,
                    preview_action,
                    cx,
                );
                crate::ui::complete_pairing(window, "Pixel 9 Pro XL".into(), cx);
                let row = sample_card.clone();
                crate::ui::pairing_test_card(window, row.uid.clone(), row.label.clone(), cx);
                crate::ui::pairing_set_card(window, Some(row), false, cx);
            }
            _ => {
                crate::ui::open_pairing_window(
                    "tapbridge-preview-invitation-0123456789abcdef",
                    true,
                    None,
                    preview_action,
                    cx,
                );
            }
        }
        cx.activate(true);
    });
}

/// Shared shell bootstrap (both platforms): tray, anchor window, global.
#[allow(clippy::too_many_arguments)]
fn shell_setup(
    cx: &mut App,
    events: mpsc::Sender<UiEvent>,
    inbox: mpsc::Receiver<UiEvent>,
    state_dir: PathBuf,
    trace_apdu: bool,
    #[cfg(windows)] watcher: Option<prompt_watch::Watcher>,
    instance: impl InstanceGuard,
) {
    // A later launch asks for a window: forward it into the tick loop.
    #[cfg(windows)]
    instance.listen_requests(events.clone());
    #[cfg(not(windows))]
    let _ = &instance;
    // Created on the gpui main thread, whose message loop pumps it.
    let tray = Tray::new(crate::assets::Icons::load())
        .inspect_err(|error| logging::log(&format!("tray unavailable: {error}")))
        .ok();
    let anchor = ui::open_anchor_window(cx);
    // gpui forces a Dock icon at launch; drop it now rather than on the first
    // tick so it never flashes up.
    #[cfg(target_os = "macos")]
    macos::dock::sync(0);
    cx.set_global(Shell {
        _anchor: anchor,
        events: events.clone(),
        state_dir: state_dir.clone(),
        trace_apdu,
        tray,
        service: None,
        #[cfg(windows)]
        usbip: if usbip::find_usbip().is_some() {
            Usbip::Attaching
        } else {
            Usbip::Missing
        },
        pairing: None,
        settings: None,
        about: None,
        managed: managed_apply(),
        autostart: autostart_load(),
        always_present: always_present_load(&state_dir),
        security: None,
        notice: None,
        service_error: None,
        waiting_since: None,
        cards: Vec::new(),
        card_task: None,
        setup_ok: HashSet::new(),
        new_card: None,
        phone_asked_login: None,
        #[cfg(windows)]
        watcher,
        _instance: Shell::instance_guard(instance),
    });
    cx.update_global::<Shell, _>(|shell, _| shell.refresh_tray());
    install_commands(cx);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(TICK).await;
            cx.update(|cx| tick(&inbox, cx));
        }
    })
    .detach();
}

/// The single-instance guard both platforms satisfy.
pub trait InstanceGuard: 'static {
    /// Windows: forward `--pair`/`--passkey-options` launches into the loop.
    #[cfg(windows)]
    fn listen_requests(&self, events: mpsc::Sender<UiEvent>);
    /// Windows: the guard the shell keeps alive for the app's lifetime.
    #[cfg(windows)]
    fn into_shell_instance(self) -> single::Instance;
}

#[cfg(windows)]
impl InstanceGuard for single::Instance {
    fn listen_requests(&self, events: mpsc::Sender<UiEvent>) {
        single::Instance::listen(self, move |request| {
            let _ = events.send(UiEvent::Open(request));
        })
    }

    fn into_shell_instance(self) -> single::Instance {
        self
    }
}

#[cfg(not(windows))]
impl InstanceGuard for NoopInstance {
    // macOS: no second-instance signaling; host.lock guards single runs.
}

/// nix-darwin: applies the app options from `/etc/tapbridge/config`.
/// True when that file exists (the app is managed).
fn managed_apply() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some((_, app)) = crate::mac_security::managed() else {
            return false;
        };
        // nix-darwin starts the app itself (launchd.user.agents); the
        // app's own LaunchAgent would start a second copy.
        if autostart::is_enabled() {
            let result = autostart::set_enabled(false);
            logging::log(&format!("nix-darwin: removing our LaunchAgent: {result:?}"));
        }
        crate::macos::ALWAYS_PRESENT.set_enabled(app.always_present);
        true
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Is "Smart card only" (`enforceSmartCard`) on? Read from the system, so
/// it holds even before Preferences has read the settings.
fn smart_card_only() -> bool {
    #[cfg(target_os = "macos")]
    return crate::mac_security::read_live().enforce_smart_card;
    #[cfg(not(target_os = "macos"))]
    false
}

/// Start at login: nix-darwin's setting when managed, else our LaunchAgent.
fn autostart_load() -> bool {
    #[cfg(target_os = "macos")]
    if let Some((_, app)) = crate::mac_security::managed() {
        return app.start_at_login;
    }
    autostart::is_enabled()
}

/// Reads the saved always-present setting and applies it to the IFD server.
fn always_present_load(state_dir: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Some((_, app)) = crate::mac_security::managed() {
            crate::macos::ALWAYS_PRESENT.load_cache(state_dir);
            return app.always_present;
        }
        let enabled = crate::macos::always_present_saved(state_dir);
        crate::macos::ALWAYS_PRESENT.load_cache(state_dir);
        crate::macos::ALWAYS_PRESENT.set_enabled(enabled);
        enabled
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state_dir;
        false
    }
}

fn spawn_service(state_dir: PathBuf, trace: bool, events: mpsc::Sender<UiEvent>) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = events.send(UiEvent::ServiceFailed(format!("tokio runtime: {error}")));
                return;
            }
        };
        let started = runtime.block_on(service::start(service::Settings {
            state_dir,
            usbip_addr: USBIP_ADDR.parse().expect("constant loopback address"),
            host_name: hostname::computer_name(),
            trace,
        }));
        match started {
            Ok(handle) => {
                crate::usb_link::spawn(handle.clone(), runtime.handle().clone());
                #[cfg(target_os = "macos")]
                spawn_ifd_server(&handle, &events);
                #[cfg(target_os = "macos")]
                spawn_security_key(&handle, &events);
                let _ = events.send(UiEvent::ServiceStarted(handle));
                // The service tasks live on this runtime until the process exits.
                runtime.block_on(std::future::pending::<()>());
            }
            Err(error) => {
                let _ = events.send(UiEvent::ServiceFailed(format!("host service: {error}")));
            }
        }
    });
}

/// macOS: serve the PC/SC IFD shim socket on a dedicated thread. The wake
/// callback routes into the UI tick loop, reusing the exact tray
/// "Request tap" path (ReadCard on a live session, else relay POST).
#[cfg(target_os = "macos")]
fn spawn_ifd_server(handle: &ServiceHandle, events: &mpsc::Sender<UiEvent>) {
    match macos::listen() {
        Ok(listener) => {
            let service = handle.clone();
            let events = events.clone();
            std::thread::spawn(move || {
                macos::serve(
                    listener,
                    service,
                    move || {
                        let _ = events.send(UiEvent::WakeRequested);
                    },
                    &macos::ALWAYS_PRESENT,
                );
            });
            logging::log("ifd: PC/SC reader socket listening");
        }
        Err(error) => logging::log(&format!("ifd socket unavailable: {error}")),
    }
}

/// macOS: the virtual FIDO security key browsers talk to. A request with
/// no card on the phone asks for a tap the same way PC/SC does.
#[cfg(target_os = "macos")]
fn spawn_security_key(handle: &ServiceHandle, events: &mpsc::Sender<UiEvent>) {
    let events = events.clone();
    let started = crate::security_key::spawn(
        handle.clone(),
        move || {
            let _ = events.send(UiEvent::WakeRequested);
        },
        logging::log,
    );
    match started {
        Ok(()) => logging::log("security key: virtual FIDO device active"),
        Err(error) => logging::log(&format!("security key unavailable: {error}")),
    }
}

fn tick(inbox: &mpsc::Receiver<UiEvent>, cx: &mut App) {
    while let Ok(event) = inbox.try_recv() {
        handle_event(event, cx);
    }
    let service_events = cx
        .global::<Shell>()
        .service
        .as_ref()
        .map(ServiceHandle::drain_events)
        .unwrap_or_default();
    for event in service_events {
        if let service::Event::Trace(line) = &event {
            logging::log(&format!("apdu: {line}"));
            continue;
        }
        // Never log card UIDs.
        match &event {
            service::Event::CardPresent { present, .. } => {
                logging::log(&format!("service: CardPresent {{ present: {present} }}"))
            }
            service::Event::UnknownCard { .. } => logging::log("service: UnknownCard"),
            service::Event::CardActionRequested { action, .. } => logging::log(&format!(
                "service: CardActionRequested {{ action: {action} }}"
            )),
            service::Event::CardActionDone { action, ok, .. } => logging::log(&format!(
                "service: CardActionDone {{ action: {action}, ok: {ok} }}"
            )),
            other => logging::log(&format!("service: {other:?}")),
        }
        if let service::Event::PairingCompleted { name } = &event
            && let Some(window) = cx.global::<Shell>().pairing
        {
            ui::complete_pairing(window, name.clone(), cx);
        }
        match &event {
            service::Event::CardsChanged => {
                let fresh = cx.update_global::<Shell, _>(|shell, _| {
                    shell.reload_cards();
                    let listed =
                        |uid: &Vec<u8>| shell.cards.iter().any(|card| &card.info.uid == uid);
                    shell.new_card.take_if(|uid| listed(uid))
                });
                if let Some(uid) = fresh {
                    logging::log("cards: new card, asking for its name");
                    open_settings(cx);
                    if let Some(window) = cx.global::<Shell>().settings {
                        ui::settings_new_card(window, uid, cx);
                    }
                }
            }
            service::Event::CardPresent { present: true, uid }
                if login::SUPPORTED && !uid.is_empty() =>
            {
                check_login(uid.clone(), cx);
            }
            // A tap of a card this PC doesn't know adds it under the
            // suggested name; the CardsChanged that follows opens the New
            // card dialog to rename it.
            service::Event::UnknownCard { uid, label } => {
                let added = cx
                    .global::<Shell>()
                    .service
                    .as_ref()
                    .is_some_and(|service| {
                        service.edit_card(uid.clone(), service::CardEdit::Add(label.clone()))
                    });
                if added {
                    cx.update_global::<Shell, _>(|shell, _| shell.new_card = Some(uid.clone()));
                }
            }
            service::Event::CardActionDone { uid, action, ok }
                if *action == wire::action::SETUP_CERTS =>
            {
                let ok = *ok;
                cx.update_global::<Shell, _>(|shell, _| {
                    if let Some(task) = shell.card_task.as_mut()
                        && task.uid == *uid
                        && task.setup_done(ok)
                        && ok
                    {
                        shell.setup_ok.insert(task.uid.clone());
                        if let Some(service) = shell.service.as_ref() {
                            service.edit_card(
                                task.uid.clone(),
                                service::CardEdit::NoteCaps(caps::CERTS),
                            );
                        }
                    }
                });
            }
            service::Event::CardActionRequested { uid, action } => {
                card_action_requested(uid.clone(), *action, cx);
            }
            _ => {}
        }
        cx.update_global::<Shell, _>(|shell, _| {
            shell.notice = None;
            shell.refresh_tray();
        });
    }
    card_tick(cx);
    // The open invitation expired and the user pressed "Try again".
    if let Some(window) = cx.global::<Shell>().pairing {
        match ui::pairing_wants_new_code(window, cx) {
            Some(true) => open_pairing(cx),
            Some(false) => {}
            None => cx.update_global::<Shell, _>(|shell, _| shell.pairing = None),
        }
    }
    // Animate (and expire) the waiting-for-tap icon.
    if cx.global::<Shell>().waiting_since.is_some() {
        cx.update_global::<Shell, _>(|shell, _| shell.refresh_tray());
    }
    // Keep an open Settings window live; prune it once closed (a failed
    // update means the window is gone).
    if let Some(window) = cx.global::<Shell>().settings {
        let snapshot = cx.global::<Shell>().settings_snapshot();
        if window
            .update(cx, |view, _, cx| view.set_snapshot(snapshot, cx))
            .is_err()
        {
            cx.update_global::<Shell, _>(|shell, _| shell.settings = None);
        }
    }
    // Keep About's details live while they're showing; prune it once closed.
    if let Some(window) = cx.global::<Shell>().about {
        let lines = cx.global::<Shell>().status_lines();
        let alive = window
            .update(cx, |view, _, cx| {
                if view.expanded() {
                    view.set_details(lines, cx);
                }
            })
            .is_ok();
        if !alive {
            cx.update_global::<Shell, _>(|shell, _| shell.about = None);
        }
    }
    // macOS: show the Dock icon only while a visible window is open.
    #[cfg(target_os = "macos")]
    {
        let shell = cx.global::<Shell>();
        let visible = shell.pairing.is_some() as usize
            + shell.settings.is_some() as usize
            + shell.about.is_some() as usize;
        macos::dock::sync(visible);
    }
    let actions = cx
        .global::<Shell>()
        .tray
        .as_ref()
        .map(Tray::actions)
        .unwrap_or_default();
    for action in actions {
        handle_action(action, cx);
    }
}

fn handle_event(event: UiEvent, cx: &mut App) {
    match event {
        UiEvent::ServiceStarted(handle) => {
            cx.update_global::<Shell, _>(|shell, _| {
                #[cfg(target_os = "macos")]
                if let Some((security, app)) = crate::mac_security::managed() {
                    // nix-darwin's module asserts this pairing; a hand-edited
                    // file still never turns USB off under smart card only.
                    let use_usb = app.use_usb || security.enforce_smart_card;
                    if handle.status().use_usb != use_usb {
                        handle.set_use_usb(use_usb);
                    }
                }
                shell.service = Some(handle);
                shell.reload_cards();
                #[cfg(windows)]
                if shell.usbip != Usbip::Missing {
                    let events = shell.events.clone();
                    std::thread::spawn(move || {
                        let result = usbip::attach().map_err(|error| error.to_string()).and_then(
                            |attached| attached.then_some(()).ok_or("usbip-win2 not found".into()),
                        );
                        let _ = events.send(UiEvent::UsbipAttached(result));
                    });
                }
                shell.refresh_tray();
            });
        }
        UiEvent::ServiceFailed(error) => {
            logging::log(&format!("service failed: {error}"));
            cx.update_global::<Shell, _>(|shell, _| {
                shell.service_error = Some(error);
                shell.notice = Some("tapbridge failed to start".into());
                shell.refresh_tray();
            });
        }
        #[cfg(windows)]
        UiEvent::UsbipAttached(result) => {
            match &result {
                Ok(()) => logging::log("usbip: reader attached (busid 1-1)"),
                Err(error) => logging::log(&format!("usbip: attach failed: {error}")),
            }
            cx.update_global::<Shell, _>(|shell, _| {
                shell.usbip = if result.is_ok() {
                    Usbip::Attached
                } else {
                    Usbip::Failed
                };
                shell.refresh_tray();
            });
        }
        #[cfg(windows)]
        UiEvent::Ceremony => {
            logging::log("wake: WebAuthn ceremony started");
            request_tap(cx);
        }
        #[cfg(not(windows))]
        UiEvent::WakeRequested => {
            logging::log("wake: PC/SC requested the card");
            request_tap(cx);
        }
        #[cfg(windows)]
        UiEvent::Open(single::Request::Pairing) => open_pairing(cx),
        #[cfg(windows)]
        UiEvent::Open(single::Request::PasskeyOptions) => open_settings(cx),
        UiEvent::Woke(result) => {
            let notice = match result {
                wake::WakeResult::Sent => {
                    logging::log("wake: relay accepted");
                    None
                }
                wake::WakeResult::Unregistered => {
                    logging::log("wake: phone token unregistered");
                    Some("Open tapbridge on your phone once to re-enable wake".to_owned())
                }
                wake::WakeResult::Rejected(reason) | wake::WakeResult::Network(reason) => {
                    logging::log(&format!("wake: {reason}"));
                    Some(format!("Wake failed: {reason}"))
                }
            };
            cx.update_global::<Shell, _>(|shell, _| {
                if notice.is_some() {
                    shell.waiting_since = None;
                    if let Some(task) = shell.card_task.as_mut() {
                        task.unreachable();
                    }
                }
                shell.notice = notice;
                shell.refresh_tray();
            });
        }
        UiEvent::LoginDone(ok, text) => {
            logging::log(&format!("login: pairing finished, ok={ok}"));
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(task) = shell.card_task.as_mut() {
                    task.paired(ok, text);
                    if let Some(service) = shell.service.as_ref() {
                        if ok {
                            service.edit_card(
                                task.uid.clone(),
                                service::CardEdit::SetLogin(!task.unpair),
                            );
                        }
                        if shell.phone_asked_login.as_ref() == Some(&task.uid) {
                            service.card_action_done(
                                task.uid.clone(),
                                wire::action::SETUP_LOGIN,
                                ok,
                            );
                        }
                    }
                    if shell.phone_asked_login.as_ref() == Some(&task.uid) {
                        shell.phone_asked_login = None;
                    }
                }
            });
        }
        #[cfg(target_os = "macos")]
        UiEvent::SecurityRead(live) => {
            cx.update_global::<Shell, _>(|shell, _| {
                shell.security.get_or_insert_with(Default::default).live = live;
            });
        }
        #[cfg(target_os = "macos")]
        UiEvent::SecurityApplied(text) => {
            cx.update_global::<Shell, _>(|shell, _| {
                let security = shell.security.get_or_insert_with(Default::default);
                security.busy = false;
                security.status = Some(text);
            });
            read_security(cx);
        }
        UiEvent::LoginChecked(uid, paired) => {
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(service) = shell.service.as_ref() {
                    service.edit_card(uid, service::CardEdit::SetLogin(paired));
                }
            });
        }
    }
}

/// Drives the card action in Preferences and the pairing window's
/// test-tap step from the current service status.
fn card_tick(cx: &mut App) {
    let shell = cx.global::<Shell>();
    let pairing = shell.pairing;
    if shell.card_task.is_none() && pairing.is_none() {
        return;
    }
    let Some(status) = shell.service.as_ref().map(ServiceHandle::status) else {
        return;
    };
    let effect = cx.update_global::<Shell, _>(|shell, _| {
        shell
            .card_task
            .as_mut()
            .and_then(|task| task.tick(Instant::now(), status.phone_connected, &status.card_uid))
    });
    if let Some(effect) = effect {
        run_card_effect(effect, cx);
    }
    if let Some(window) = pairing {
        if ui::pairing_test_tick(window, status.phone_connected, cx) == Some(true) {
            logging::log("pairing: asking the phone for a test card");
            request_tap(cx);
        }
        if status.card_present && ui::pairing_test_waiting(window, cx) {
            let label = cx
                .global::<Shell>()
                .service
                .as_ref()
                .map(ServiceHandle::known_cards)
                .unwrap_or_default()
                .into_iter()
                .find(|card| !status.card_uid.is_empty() && card.info.uid == status.card_uid)
                .map_or_else(|| "your card".to_owned(), |card| card.info.label);
            ui::pairing_test_card(window, status.card_uid.clone(), label, cx);
            cx.update_global::<Shell, _>(|shell, _| shell.reload_cards());
        }
        // Keep the embedded card (and its setup progress) live.
        if let Some(uid) = ui::pairing_test_uid(window, cx) {
            let shell = cx.global::<Shell>();
            let row = shell
                .card_rows(&status)
                .into_iter()
                .find(|row| row.uid == uid);
            let busy = shell.card_task.as_ref().is_some_and(CardTask::busy);
            ui::pairing_set_card(window, row, busy, cx);
        }
    }
}

/// Carries out what the card action asks for next.
fn run_card_effect(effect: Effect, cx: &mut App) {
    match effect {
        Effect::SendSetup => {
            let next = cx.update_global::<Shell, _>(|shell, _| {
                let service = shell.service.as_ref()?;
                let task = shell.card_task.as_mut()?;
                let sent = service.request_card_action(task.uid.clone(), wire::action::SETUP_CERTS);
                logging::log(&format!(
                    "cards: smart-card setup sent to the phone: {sent}"
                ));
                task.setup_sent(sent, Instant::now())
            });
            if let Some(next) = next {
                run_card_effect(next, cx);
            }
        }
        Effect::RequestTap => {
            if !request_tap(cx) {
                cx.update_global::<Shell, _>(|shell, _| {
                    if let Some(task) = shell.card_task.as_mut() {
                        task.unreachable();
                    }
                });
            }
        }
        Effect::RunPairing => {
            let events = cx.global::<Shell>().events.clone();
            logging::log("login: pairing the card on the phone");
            std::thread::spawn(move || {
                let (ok, message) = login::pair_present_card();
                let _ = events.send(UiEvent::LoginDone(ok, message));
            });
        }
        Effect::RunUnpairing => {
            let events = cx.global::<Shell>().events.clone();
            logging::log("login: unpairing the card on the phone");
            std::thread::spawn(move || {
                let (ok, message) = login::unpair_present_card();
                let _ = events.send(UiEvent::LoginDone(ok, message));
            });
        }
    }
}

fn handle_action(action: MenuAction, cx: &mut App) {
    match action {
        MenuAction::Pair => open_pairing(cx),
        MenuAction::OpenSettings => open_settings(cx),
        MenuAction::About => open_about(cx),
        MenuAction::Quit => quit(cx),
    }
}

/// Registers the app-wide commands, their shortcuts and (macOS) the app menu.
fn install_commands(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| open_settings(cx));
    cx.on_action(|_: &Quit, cx| quit(cx));
    cx.on_action(|_: &OpenAbout, cx| open_about(cx));
    crate::text_input::bind_keys(cx);
    // "secondary" is ⌘ on macOS and Ctrl elsewhere.
    cx.bind_keys([
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
    #[cfg(target_os = "macos")]
    cx.set_menus(vec![gpui::Menu {
        name: "tapbridge".into(),
        items: vec![
            gpui::MenuItem::action("About tapbridge", OpenAbout),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Preferences\u{2026}", OpenSettings),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Quit tapbridge", Quit),
        ],
        disabled: false,
    }]);
}

fn open_about(cx: &mut App) {
    let (details, existing) = {
        let shell = cx.global::<Shell>();
        (shell.status_lines(), shell.about)
    };
    #[cfg(target_os = "macos")]
    macos::dock::bring_to_front();
    let window = crate::about::open_about_window(details, existing, cx);
    cx.update_global::<Shell, _>(|shell, _| shell.about = Some(window));
}

/// macOS: reads the smart card settings in the background (it runs
/// `defaults`); the answer arrives as [`UiEvent::SecurityRead`].
fn read_security(cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let events = cx.global::<Shell>().events.clone();
        std::thread::spawn(move || {
            let _ = events.send(UiEvent::SecurityRead(crate::mac_security::read_live()));
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = cx;
}

fn open_settings(cx: &mut App) {
    read_security(cx);
    let (snapshot, existing) = {
        let shell = cx.global::<Shell>();
        (shell.settings_snapshot(), shell.settings)
    };
    #[cfg(target_os = "macos")]
    macos::dock::bring_to_front();
    let window = ui::open_settings_window(snapshot, existing, settings_action, cx);
    cx.update_global::<Shell, _>(|shell, _| shell.settings = Some(window));
}

fn settings_action(action: ui::SettingsAction, cx: &mut App) {
    match action {
        ui::SettingsAction::Pair => open_pairing(cx),
        ui::SettingsAction::Forget => {
            logging::log("forget: revoking paired phone");
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(service) = shell.service.as_ref() {
                    service.forget_phone();
                }
                shell.refresh_tray();
            });
        }
        ui::SettingsAction::SetUseUsb(enabled) => {
            if cx.global::<Shell>().managed {
                return;
            }
            if !enabled && smart_card_only() {
                logging::log("use usb cable: kept on, smart card only is on");
                return;
            }
            logging::log(&format!("use usb cable -> {enabled}"));
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(service) = shell.service.as_ref() {
                    service.set_use_usb(enabled);
                }
                shell.refresh_tray();
            });
        }
        ui::SettingsAction::SetAutostart(enabled) => {
            if cx.global::<Shell>().managed {
                return;
            }
            let result = autostart::set_enabled(enabled);
            logging::log(&format!("start at login -> {enabled}: {result:?}"));
            cx.update_global::<Shell, _>(|shell, _| shell.autostart = autostart_load());
        }
        ui::SettingsAction::SetAlwaysPresent(enabled) => {
            #[cfg(target_os = "macos")]
            cx.update_global::<Shell, _>(|shell, _| {
                if shell.managed {
                    return;
                }
                let result = crate::macos::always_present_save(&shell.state_dir, enabled);
                logging::log(&format!("always-present card -> {enabled}: {result:?}"));
                if result.is_ok() {
                    crate::macos::ALWAYS_PRESENT.set_enabled(enabled);
                    shell.always_present = enabled;
                }
            });
            #[cfg(not(target_os = "macos"))]
            let _ = enabled;
        }
        ui::SettingsAction::ApplySecurity(want) => {
            #[cfg(target_os = "macos")]
            cx.update_global::<Shell, _>(|shell, _| {
                if shell.managed {
                    return;
                }
                let use_usb = shell
                    .service
                    .as_ref()
                    .is_some_and(|service| service.status().use_usb);
                let security = shell.security.get_or_insert_with(Default::default);
                if security.busy {
                    return;
                }
                if want.enforce_smart_card && !use_usb {
                    security.status = Some(
                        "Not applied: \"Smart card only\" needs \"Connect to phone via USB\"."
                            .to_owned(),
                    );
                    return;
                }
                security.busy = true;
                security.status = None;
                let events = shell.events.clone();
                let dir = shell.state_dir.clone();
                std::thread::spawn(move || {
                    let text = match crate::mac_security::apply(&want, &dir) {
                        crate::mac_security::Applied::Done => "Applied.".to_owned(),
                        crate::mac_security::Applied::ApproveProfile => {
                            "Almost done: approve \"tapbridge smart card settings\" in System \
                             Settings → General → Device Management."
                                .to_owned()
                        }
                        crate::mac_security::Applied::Failed(why) => format!("Not applied: {why}"),
                    };
                    logging::log(&format!("mac security: {text}"));
                    let _ = events.send(UiEvent::SecurityApplied(text));
                });
            });
            #[cfg(not(target_os = "macos"))]
            let _ = want;
        }
        ui::SettingsAction::RenameCard(uid, label) => {
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(service) = shell.service.as_ref() {
                    service.edit_card(uid, service::CardEdit::Rename(label));
                }
                shell.reload_cards();
            });
        }
        ui::SettingsAction::ForgetCard(uid) => {
            logging::log("cards: forgetting a card");
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(service) = shell.service.as_ref() {
                    service.edit_card(uid.clone(), service::CardEdit::Forget);
                }
                if shell
                    .card_task
                    .as_ref()
                    .is_some_and(|task| task.uid == uid && task.dismissable())
                {
                    shell.card_task = None;
                }
                shell.setup_ok.remove(&uid);
                shell.reload_cards();
            });
        }
        ui::SettingsAction::SetUpCard(uid) => {
            if cx
                .global::<Shell>()
                .card_task
                .as_ref()
                .is_some_and(CardTask::busy)
            {
                return;
            }
            logging::log("cards: smart-card setup requested");
            let (task, effect) = CardTask::setup(uid);
            cx.update_global::<Shell, _>(|shell, _| shell.card_task = Some(task));
            run_card_effect(effect, cx);
        }
        ui::SettingsAction::SetUpLogin(uid) => {
            if login::SUPPORTED {
                start_login(uid, cx);
            }
        }
        ui::SettingsAction::RemoveLogin(uid) => {
            if login::SUPPORTED {
                start_unpair(uid, cx);
            }
        }
        ui::SettingsAction::CheckCard => {
            logging::log("cards: asking for a tap to check the card");
            request_tap(cx);
        }
        ui::SettingsAction::OpenUrl(url) => cx.open_url(&url),
        ui::SettingsAction::DismissCardTask => {
            cx.update_global::<Shell, _>(|shell, _| {
                if shell.card_task.as_ref().is_some_and(CardTask::dismissable) {
                    shell.card_task = None;
                }
            });
        }
    }
}

/// Starts pairing the card with this PC's login unless another card task
/// is running. Returns whether it started.
fn start_login(uid: Vec<u8>, cx: &mut App) -> bool {
    if cx
        .global::<Shell>()
        .card_task
        .as_ref()
        .is_some_and(CardTask::busy)
    {
        return false;
    }
    let status = cx
        .global::<Shell>()
        .service
        .as_ref()
        .map(ServiceHandle::status)
        .unwrap_or_default();
    let present = status.card_present && status.card_uid == uid;
    let (task, effect) = CardTask::login(uid, present, Instant::now());
    cx.update_global::<Shell, _>(|shell, _| shell.card_task = Some(task));
    run_card_effect(effect, cx);
    true
}

/// Starts removing the card from this PC's login unless another card
/// task is running.
fn start_unpair(uid: Vec<u8>, cx: &mut App) {
    if cx
        .global::<Shell>()
        .card_task
        .as_ref()
        .is_some_and(CardTask::busy)
    {
        return;
    }
    let status = cx
        .global::<Shell>()
        .service
        .as_ref()
        .map(ServiceHandle::status)
        .unwrap_or_default();
    let present = status.card_present && status.card_uid == uid;
    logging::log("login: removing a card from the login");
    let (task, effect) = CardTask::unpair(uid, present, Instant::now());
    cx.update_global::<Shell, _>(|shell, _| shell.card_task = Some(task));
    run_card_effect(effect, cx);
}

/// The phone asks this PC to do something for a card. Only the login
/// setup runs here (where supported); anything else is answered as failed.
fn card_action_requested(uid: Vec<u8>, action: u8, cx: &mut App) {
    if login::SUPPORTED
        && action == wire::action::SETUP_LOGIN
        && cx
            .global::<Shell>()
            .cards
            .iter()
            .any(|card| card.info.uid == uid)
    {
        logging::log("login: the phone asked to pair a card");
        open_settings(cx);
        if start_login(uid.clone(), cx) {
            cx.update_global::<Shell, _>(|shell, _| shell.phone_asked_login = Some(uid.clone()));
            if let Some(window) = cx.global::<Shell>().settings {
                ui::settings_open_login(window, uid, cx);
            }
            return;
        }
    }
    if let Some(service) = cx.global::<Shell>().service.as_ref() {
        service.card_action_done(uid, action, false);
    }
}

/// After a known card arrives, reads (never pairs) whether its login
/// identity is already paired, so a pairing made outside tapbridge shows
/// up too. Only for cards that hold certificates.
fn check_login(uid: Vec<u8>, cx: &mut App) {
    let shell = cx.global::<Shell>();
    if !shell
        .cards
        .iter()
        .any(|card| card.info.uid == uid && card.info.has(caps::CERTS))
        || shell.card_task.as_ref().is_some_and(CardTask::busy)
    {
        return;
    }
    let events = shell.events.clone();
    std::thread::spawn(move || {
        if let Some(paired) = login::present_card_paired() {
            let _ = events.send(UiEvent::LoginChecked(uid, paired));
        }
    });
}

/// Asks the phone for a tap: Windows runs it when a WebAuthn ceremony
/// starts, macOS when PC/SC wants the card and no phone is connected, and
/// the card actions and pairing test when they need the phone. Returns
/// whether the request went out.
fn request_tap(cx: &mut App) -> bool {
    cx.update_global::<Shell, _>(|shell, _| {
        let Some(service) = shell.service.as_ref() else {
            logging::log("wake: service not running");
            return false;
        };
        let notice = match service.request_tap() {
            TapOutcome::ReadCardSent => {
                logging::log("wake: tap reminder sent to the connected phone");
                shell.waiting_since = Some(Instant::now());
                None
            }
            TapOutcome::Wake { token, hint } => match wake::relay_url(&shell.state_dir) {
                Some(relay) => {
                    let events = shell.events.clone();
                    std::thread::spawn(move || {
                        let host = hint.host.to_string();
                        let result = wake::send_wake(&relay, &token, &host, &hint.to_string());
                        let _ = events.send(UiEvent::Woke(result));
                    });
                    shell.waiting_since = Some(Instant::now());
                    None
                }
                None => {
                    logging::log("wake: relay not configured");
                    Some("Relay not configured".to_owned())
                }
            },
            TapOutcome::NotPaired => {
                logging::log("wake: no phone paired");
                Some("No phone paired".to_owned())
            }
            TapOutcome::NoPush => {
                logging::log("wake: the phone has not sent a push token");
                Some("Open tapbridge on your phone once to enable wake".to_owned())
            }
        };
        let sent = notice.is_none();
        shell.notice = notice;
        shell.refresh_tray();
        sent
    })
}

fn open_pairing(cx: &mut App) {
    let (invitation, usb_hint, existing) = {
        let shell = cx.global::<Shell>();
        (
            shell
                .service
                .as_ref()
                .and_then(ServiceHandle::mint_invitation),
            shell.service.as_ref().is_some_and(|s| s.status().use_usb),
            shell.pairing,
        )
    };
    let Some(invitation) = invitation else {
        logging::log("pairing: service not running");
        return;
    };
    logging::log("pairing: invitation minted");
    #[cfg(target_os = "macos")]
    macos::dock::bring_to_front();
    let window = ui::open_pairing_window(
        &invitation.to_string(),
        usb_hint,
        existing,
        settings_action,
        cx,
    );
    cx.update_global::<Shell, _>(|shell, _| shell.pairing = Some(window));
}

fn quit(cx: &mut App) {
    logging::log("quitting");
    // Detach while the reader still answers, so Windows sees a clean unplug.
    #[cfg(windows)]
    usbip::detach_ours();
    cx.update_global::<Shell, _>(|shell, _| {
        #[cfg(windows)]
        shell.watcher.take();
        if let Some(service) = shell.service.take() {
            service.shutdown();
        }
    });
    cx.quit();
}

impl Shell {
    fn reload_cards(&mut self) {
        if let Some(service) = self.service.as_ref() {
            self.cards = service.known_cards();
        }
        // The always-present card is the one paired with this Mac's login.
        #[cfg(target_os = "macos")]
        crate::macos::ALWAYS_PRESENT.set_atr(
            self.cards
                .iter()
                .rev()
                .find(|known| known.info.login && !known.info.deleted() && !known.atr.is_empty())
                .map(|known| known.atr.clone())
                .unwrap_or_default(),
        );
    }

    fn card_rows(&self, status: &service::Status) -> Vec<ui::CardRow> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        self.cards
            .iter()
            .map(|known| {
                let card = &known.info;
                let present = status.card_present && status.card_uid == card.uid;
                let task = self.card_task.as_ref().filter(|task| task.uid == card.uid);
                ui::CardRow {
                    uid: card.uid.clone(),
                    label: card.label.clone(),
                    last_seen: crate::card_tasks::last_seen_text(
                        now,
                        known.last_seen / 1000,
                        present,
                    ),
                    passkey: card.has(caps::FIDO),
                    smart_card: card.has(caps::PIV),
                    task: task.map(CardTask::text),
                    task_failed: task.is_some_and(CardTask::failed),
                    task_finished: task.is_some_and(|task| !task.busy()),
                    task_dismissable: task.is_some_and(CardTask::dismissable),
                    offer_login: login::SUPPORTED
                        && (card.has(caps::CERTS) || self.setup_ok.contains(&card.uid)),
                    checklist: Some(crate::card_tasks::Checklist::new(card)),
                    install_steps: crate::card_tasks::install_steps(
                        &known.atr,
                        card.has(caps::BLOCKED),
                    ),
                    flexsecure: crate::card_tasks::is_flexsecure(&known.atr),
                }
            })
            .collect()
    }

    fn refresh_tray(&mut self) {
        let status = self.service.as_ref().map(ServiceHandle::status);
        if let Some(since) = self.waiting_since
            && (since.elapsed() >= TAP_WAIT || status.as_ref().is_some_and(|s| s.card_present))
        {
            self.waiting_since = None;
        }
        let (text, state) = self.tray_status(status.as_ref());
        let frame = self.waiting_since.map_or(0, |since| {
            (since.elapsed().as_millis() / TAP_FRAME.as_millis()) as usize
        });
        // Nothing paired and nothing wrong: the status line ("Pair a phone")
        // doubles as the way to start pairing.
        let pair_action = self.notice.is_none()
            && state == TrayState::Disconnected
            && status.as_ref().is_some_and(|status| !status.paired);
        if let Some(tray) = self.tray.as_mut() {
            tray.set_status(&text, state, frame, pair_action);
        }
    }

    /// The one-line state and icon state, shared by the tray and Settings.
    fn tray_status(&self, status: Option<&service::Status>) -> (String, TrayState) {
        #[cfg(windows)]
        let (text, state) = if self.usbip == Usbip::Missing {
            ("Install USB/IP driver".to_owned(), TrayState::MissingUsbip)
        } else if let Some(notice) = &self.notice {
            (notice.clone(), TrayState::Error)
        } else if self.usbip == Usbip::Failed {
            ("USB/IP attach failed".to_owned(), TrayState::Error)
        } else if let Some(status) = status {
            self.status_text(status)
        } else {
            ("tapbridge starting".to_owned(), TrayState::Disconnected)
        };
        #[cfg(not(windows))]
        let (text, state) = if let Some(notice) = &self.notice {
            (notice.clone(), TrayState::Error)
        } else if let Some(status) = status {
            self.status_text(status)
        } else {
            ("tapbridge starting".to_owned(), TrayState::Disconnected)
        };
        (text, state)
    }

    fn status_text(&self, status: &service::Status) -> (String, TrayState) {
        let phone = status.phone_name.as_deref().unwrap_or("phone");
        if status.phone_connected && status.card_present {
            (format!("Card present ({phone})"), TrayState::Card)
        } else if self.waiting_since.is_some() {
            (format!("Tap your key on {phone}"), TrayState::Waiting)
        } else if status.phone_connected && status.usb {
            (format!("{phone} connected (USB)"), TrayState::Connected)
        } else if status.phone_connected {
            (format!("{phone} connected"), TrayState::Connected)
        } else if status.paired {
            (format!("Paired with {phone}"), TrayState::Disconnected)
        } else {
            let text = if cfg!(target_os = "macos") {
                "Pair a phone"
            } else {
                "Not paired"
            };
            (text.to_owned(), TrayState::Disconnected)
        }
    }

    fn settings_snapshot(&self) -> ui::SettingsSnapshot {
        let status = self.service.as_ref().map(ServiceHandle::status);
        let (summary, state) = self.tray_status(status.as_ref());
        let health = match state {
            TrayState::Card | TrayState::Waiting | TrayState::Connected => ui::Health::Good,
            TrayState::Disconnected => ui::Health::Idle,
            TrayState::Error | TrayState::MissingUsbip => ui::Health::Problem,
        };
        let status = status.unwrap_or_default();
        ui::SettingsSnapshot {
            summary,
            health,
            phone: status.paired.then(|| {
                status
                    .phone_name
                    .clone()
                    .unwrap_or_else(|| "Phone".to_owned())
            }),
            phone_connected: status.phone_connected,
            usb: status.usb,
            use_usb: status.use_usb,
            autostart: self.autostart,
            always_present: cfg!(target_os = "macos").then_some(self.always_present),
            security: self.security.clone(),
            managed: self.managed,
            cards: self.card_rows(&status),
            card_busy: self.card_task.as_ref().is_some_and(CardTask::busy),
        }
    }

    fn status_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("Host: {}", hostname::computer_name())];
        #[cfg(windows)]
        lines.push(format!(
            "USB/IP: {USBIP_ADDR} busid 1-1, {}",
            match self.usbip {
                Usbip::Missing => "driver missing",
                Usbip::Attaching => "attaching",
                Usbip::Attached => "attached",
                Usbip::Failed => "attach failed",
            }
        ));
        #[cfg(not(windows))]
        lines.push(format!(
            "PC/SC reader: {} (virtual PIV reader)",
            crate::macos::IFD_SOCKET_PATH
        ));
        match self.service.as_ref().map(ServiceHandle::status) {
            Some(status) => {
                lines.push(format!(
                    "Card present: {}",
                    if status.card_present { "yes" } else { "no" }
                ));
                lines.push(format!(
                    "Phone can be woken: {}",
                    if status.push_stored { "yes" } else { "no" }
                ));
                lines.push(format!(
                    "Connection: {}",
                    match (status.phone_connected, status.usb) {
                        (false, _) => "none",
                        (true, true) => "USB cable",
                        (true, false) => "network",
                    }
                ));
                lines.push(format!(
                    "USB cable: {}",
                    if status.use_usb { "on" } else { "off" }
                ));
            }
            None => lines.push(match &self.service_error {
                Some(error) => format!("Host service failed: {error}"),
                None => "Host service: starting".to_owned(),
            }),
        }
        lines.push(format!(
            "Relay: {}",
            wake::relay_url(&self.state_dir).unwrap_or_else(|| {
                "not configured (build with TAPBRIDGE_RELAY_URL or add relay.txt)".into()
            })
        ));
        #[cfg(windows)]
        lines.push(format!(
            "Prompt watcher: {}",
            if self.watcher.is_some() {
                "on"
            } else {
                "unavailable"
            }
        ));
        lines.push(format!(
            "APDU trace: {}",
            if self.trace_apdu {
                "on"
            } else {
                "off (--trace-apdu)"
            }
        ));
        lines
    }
}
