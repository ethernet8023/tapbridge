//! App orchestration. The gpui main thread owns everything in [`Shell`] (tray,
//! service handle, windows, prompt watcher) as a gpui global; background
//! threads (host runtime, usbip, wake requests, single-instance signal,
//! prompt watcher) only send [`UiEvent`]s into one channel drained by `tick`.

#[cfg(target_os = "macos")]
use crate::macos;
use crate::{
    autostart, hostname, logging,
    tray::{MenuAction, Tray, TrayState},
    ui, wake,
};
#[cfg(windows)]
use crate::{prompt_watch, single, usbip};
use gpui::{App, Application, AsyncApp, BorrowAppContext, Global, WindowHandle};
use std::{path::PathBuf, sync::mpsc, time::Duration};
use tapbridge_host::service::{self, ServiceHandle, TapOutcome};

const USBIP_ADDR: &str = "127.0.0.1:3240";
const TICK: Duration = Duration::from_millis(100);

/// macOS placeholder for the Windows single-instance guard: the LaunchAgent
/// RunAtLoad path plus manual launches may still race; host.lock is the real
/// single-instance guarantee (service::start fails if another owns it).
#[cfg(not(windows))]
#[derive(Default)]
pub struct NoopInstance;

enum UiEvent {
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
    status: Option<WindowHandle<ui::StatusView>>,
    #[cfg(windows)]
    passkey_options: Option<WindowHandle<ui::PasskeyView>>,
    /// Shown in the tray until the next state change (e.g. relay problems).
    notice: Option<String>,
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

/// Conversion helper for the Windows guard.
#[cfg(windows)]
trait IntoShellInstance {
    fn into_shell_instance(self) -> single::Instance;
}

#[cfg(windows)]
impl IntoShellInstance for single::Instance {
    fn into_shell_instance(self) -> single::Instance {
        self
    }
}

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
    Application::new().run(move |cx: &mut App| {
        spawn_service(state_dir.clone(), trace_apdu, events.clone());
        #[cfg(windows)]
        shell_setup(cx, events, inbox, state_dir, trace_apdu, watcher, instance);
        #[cfg(not(windows))]
        shell_setup(cx, events, inbox, state_dir, trace_apdu, instance);
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
        status: None,
        #[cfg(windows)]
        passkey_options: None,
        notice: None,
        #[cfg(windows)]
        watcher,
        _instance: Shell::instance_guard(instance),
    });
    cx.update_global::<Shell, _>(|shell, _| {
        let enabled = autostart::is_enabled();
        if let Some(tray) = shell.tray.as_mut() {
            tray.set_autostart_checked(enabled);
        }
        shell.refresh_tray();
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(TICK).await;
            if cx.update(|cx| tick(&inbox, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

/// The single-instance guard both platforms satisfy.
pub trait InstanceGuard: 'static {
    /// Windows: forward `--pair`/`--passkey-options` launches into the loop.
    #[cfg(windows)]
    fn listen_requests(&self, events: mpsc::Sender<UiEvent>);
}

#[cfg(windows)]
impl InstanceGuard for single::Instance {
    fn listen_requests(&self, events: mpsc::Sender<UiEvent>) {
        single::Instance::listen(self, move |request| {
            let _ = events.send(UiEvent::Open(request));
        })
    }
}

#[cfg(not(windows))]
impl InstanceGuard for NoopInstance {
    // macOS: no second-instance signaling; host.lock guards single runs.
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
                #[cfg(target_os = "macos")]
                spawn_ifd_server(&handle, &events);
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
                macos::serve(listener, service, move || {
                    let _ = events.send(UiEvent::WakeRequested);
                });
            });
            logging::log("ifd: PC/SC reader socket listening");
        }
        Err(error) => logging::log(&format!("ifd socket unavailable: {error}")),
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
        logging::log(&format!("service: {event:?}"));
        if let service::Event::PairingCompleted { name } = &event
            && let Some(window) = cx.global::<Shell>().pairing
        {
            ui::complete_pairing(window, name.clone(), cx);
        }
        cx.update_global::<Shell, _>(|shell, _| {
            shell.notice = None;
            shell.refresh_tray();
        });
    }
    // An unused invitation expired while its window is open: show a fresh one.
    if let Some(window) = cx.global::<Shell>().pairing {
        match ui::pairing_expired(window, cx) {
            Some(true) => open_pairing(cx),
            Some(false) => {}
            None => cx.update_global::<Shell, _>(|shell, _| shell.pairing = None),
        }
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
                shell.service = Some(handle);
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
        UiEvent::Open(single::Request::PasskeyOptions) => open_passkey_options(cx),
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
                shell.notice = notice;
                shell.refresh_tray();
            });
        }
    }
}

fn handle_action(action: MenuAction, cx: &mut App) {
    match action {
        MenuAction::RequestTap => request_tap(cx),
        MenuAction::Pair => open_pairing(cx),
        MenuAction::Forget => {
            logging::log("forget: revoking paired phone");
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(service) = shell.service.as_ref() {
                    service.forget_phone();
                }
                shell.refresh_tray();
            });
        }
        MenuAction::OpenStatus => {
            let (lines, existing) = {
                let shell = cx.global::<Shell>();
                (shell.status_lines(), shell.status)
            };
            let window = ui::open_status_window(lines, existing, cx);
            cx.update_global::<Shell, _>(|shell, _| shell.status = Some(window));
        }
        #[cfg(windows)]
        MenuAction::PasskeyOptions => open_passkey_options(cx),
        #[cfg(not(windows))]
        MenuAction::PasskeyOptions => {} // hidden in the macOS tray menu
        MenuAction::ToggleAutostart(enabled) => {
            let result = autostart::set_enabled(enabled);
            logging::log(&format!("start at login -> {enabled}: {result:?}"));
            let actual = autostart::is_enabled();
            cx.update_global::<Shell, _>(|shell, _| {
                if let Some(tray) = shell.tray.as_mut() {
                    tray.set_autostart_checked(actual);
                }
            });
        }
        MenuAction::Quit => quit(cx),
    }
}

fn request_tap(cx: &mut App) {
    cx.update_global::<Shell, _>(|shell, _| {
        let Some(service) = shell.service.as_ref() else {
            logging::log("wake: service not running");
            return;
        };
        let notice = match service.request_tap() {
            TapOutcome::ReadCardSent => {
                logging::log("wake: tap reminder sent to the connected phone");
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
        shell.notice = notice;
        shell.refresh_tray();
    });
}

#[cfg(windows)]
fn open_passkey_options(cx: &mut App) {
    let existing = cx.global::<Shell>().passkey_options;
    let window = ui::open_passkey_window(existing, cx);
    cx.update_global::<Shell, _>(|shell| shell.passkey_options = Some(window));
}

fn open_pairing(cx: &mut App) {
    let (invitation, existing) = {
        let shell = cx.global::<Shell>();
        (
            shell
                .service
                .as_ref()
                .and_then(ServiceHandle::mint_invitation),
            shell.pairing,
        )
    };
    let Some(invitation) = invitation else {
        logging::log("pairing: service not running");
        return;
    };
    logging::log("pairing: invitation minted");
    let window = ui::open_pairing_window(&invitation.to_string(), existing, cx);
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
    fn refresh_tray(&mut self) {
        let status = self.service.as_ref().map(ServiceHandle::status);
        #[cfg(windows)]
        let (text, state) = if self.usbip == Usbip::Missing {
            ("Install USB/IP driver".to_owned(), TrayState::MissingUsbip)
        } else if let Some(notice) = &self.notice {
            (notice.clone(), TrayState::Error)
        } else if self.usbip == Usbip::Failed {
            ("USB/IP attach failed".to_owned(), TrayState::Error)
        } else if let Some(status) = status {
            Self::status_text(&status)
        } else {
            ("tapbridge starting".to_owned(), TrayState::Idle)
        };
        #[cfg(not(windows))]
        let (text, state) = if let Some(notice) = &self.notice {
            (notice.clone(), TrayState::Error)
        } else if let Some(status) = status {
            Self::status_text(&status)
        } else {
            ("tapbridge starting".to_owned(), TrayState::Idle)
        };
        if let Some(tray) = self.tray.as_mut() {
            tray.set_status(&text, state);
        }
    }

    fn status_text(status: &service::Status) -> (String, TrayState) {
        let phone = status.phone_name.as_deref().unwrap_or("phone");
        if status.phone_connected && status.card_present {
            (format!("Card present ({phone})"), TrayState::Card)
        } else if status.phone_connected {
            (format!("{phone} connected"), TrayState::Phone)
        } else if status.paired {
            (format!("Paired with {phone}"), TrayState::Idle)
        } else {
            ("Not paired".to_owned(), TrayState::Idle)
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
                let phone = status.phone_name.as_deref().unwrap_or("phone");
                lines.push(if status.paired {
                    format!("Paired with {phone}")
                } else {
                    "Not paired".to_owned()
                });
                lines.push(format!(
                    "Phone session: {}",
                    if status.phone_connected {
                        "connected"
                    } else {
                        "idle"
                    }
                ));
                lines.push(format!("Card present: {}", status.card_present));
                lines.push(format!("Phone can be woken: {}", status.push_stored));
            }
            None => lines.push("Host service: not running".to_owned()),
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
