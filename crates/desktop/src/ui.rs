//! gpui windows: the pairing QR window and the Settings window. Only these
//! surfaces use gpui; the tray and all orchestration live beside them.
use crate::card_tasks::{Checklist, StepState, TestTap};
use crate::glass;
#[cfg(windows)]
use crate::passkeys;
use crate::text_input::{InputEvent, TextInput};
use gpui::{
    Animation, AnimationExt, App, Bounds, Context, FocusHandle, Task, Window, WindowBounds,
    WindowHandle, WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, rgba, size,
};
#[cfg(windows)]
use gpui::{Entity, Pixels};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
use tapbridge_core::pairing::INVITATION_TTL;

/// The pairing window is square; the QR is sized to fit it, not the reverse.
const PAIRING_WINDOW: f32 = 480.0;
/// The QR step is taller than wide: room for the USB hint under the countdown.
const PAIRING_QR_HEIGHT: f32 = PAIRING_WINDOW + 30.0;
/// The pairing window grows to this height once it shows the test card's
/// checklist, so the steps fit without scrolling.
const PAIRING_CARD_HEIGHT: f32 = 660.0;
/// Target QR side including its quiet zone, in logical pixels.
const QR_TARGET: f32 = 340.0;

/// QR quiet zone in modules (ISO/IEC 18004 asks for 4).
const QUIET_ZONE: u16 = 4;

#[cfg(windows)]
const PANEL_2: u32 = 0x221A30;
/// macOS draws the traffic lights over our content (transparent titlebar).
const TITLEBAR_INSET: f32 = if cfg!(target_os = "macos") { 28.0 } else { 0.0 };
const TEXT: u32 = 0xF2EFFA;
const ACCENT: u32 = 0x7C5CFF;
/// Card settings glyph (SF Symbols gearshape), tinted by `text_color`.
const GEAR_SVG: &[u8] = include_bytes!("../assets/ui/gear.svg");
const GOOD: u32 = 0x3DDC97;
/// Light brand lavender: readable on PANEL, still clearly "tapbridge".
const ACCENT_SOFT: u32 = 0xC4B5FF;
const WARN: u32 = 0xFFB454;
/// The countdown turns WARN for the last stretch before the code refreshes.
const EXPIRY_WARNING: Duration = Duration::from_secs(30);

pub struct PairingView {
    /// Row-major dark modules, `dimension * dimension` long; empty when the
    /// invitation could not be encoded.
    dark: Rc<[bool]>,
    dimension: u16,
    /// Set when pairing completes: the phone's display name.
    paired: Option<String>,
    /// The "tap a card to test" step after pairing.
    test: Option<TestTap>,
    /// The card that came through the test, as Preferences shows it (kept
    /// fresh by the shell), and whether a card action is running.
    test_card: Option<(Vec<u8>, Option<CardRow>, bool)>,
    /// Carries out the embedded card's buttons (same as Preferences).
    on_action: OnSettingsAction,
    /// The embedded card is showing the applet install instructions.
    show_install: bool,
    /// When the invitation stops working (the host gate uses the same TTL).
    expires: Instant,
    /// "Try again" was pressed on the expired code: the shell mints a new one.
    retry: bool,
    /// "Use USB cable" is on: say that plugging the phone in pairs too.
    usb_hint: bool,
    /// Re-renders once a second for the countdown; dropped with the view.
    _ticker: Task<()>,
}

impl PairingView {
    pub fn new(
        invitation: &str,
        usb_hint: bool,
        on_action: OnSettingsAction,
        cx: &mut Context<Self>,
    ) -> Self {
        let (dark, dimension) = build_qr(invitation).unwrap_or((Vec::new(), 0));
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            dark: dark.into(),
            dimension,
            paired: None,
            test: None,
            test_card: None,
            on_action,
            show_install: false,
            expires: Instant::now() + INVITATION_TTL,
            retry: false,
            usb_hint,
            _ticker: ticker,
        }
    }

    /// The invitation ran out and nobody paired.
    pub fn expired(&self) -> bool {
        self.paired.is_none() && Instant::now() >= self.expires
    }

    /// The user asked for a fresh code after this one expired.
    pub fn wants_new_code(&self) -> bool {
        self.retry && self.expired()
    }
}

fn countdown(remaining: Duration) -> String {
    let seconds = remaining.as_secs();
    format!("Expires in {}:{:02}", seconds / 60, seconds % 60)
}

impl Render for PairingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dimension = self.dimension;
        // Whole device pixels per module: fractional modules leave seams.
        let scale = window.scale_factor();
        let cells = (dimension + 2 * QUIET_ZONE) as f32;
        let module = px((QR_TARGET * scale / cells).floor().max(1.0) / scale);
        let side = module * cells;
        let stage = side + px(20.0);

        let root = div()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap(px(18.0))
            .pt(px(TITLEBAR_INSET))
            .text_color(rgb(TEXT))
            .child(glass::backdrop());

        if let Some(phone) = self.paired.clone()
            && let Some((_, Some(card), busy)) = self.test_card.clone()
        {
            return root.child(self.render_test_card(&phone, &card, busy, cx));
        }
        if let Some(phone) = &self.paired {
            return root
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(GOOD))
                        .child("Pairing successful"),
                )
                .child(success_badge(96.0))
                .child(
                    div()
                        .text_size(px(22.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(format!("Paired with {phone}")),
                )
                .child(self.render_test(cx));
        }

        // One canvas paints every dark module: the QR costs one element per
        // frame while the ring animates, not one div per module.
        let dark = self.dark.clone();
        let grid = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let origin = bounds.origin + point(module, module) * QUIET_ZONE as f32;
                for (index, _) in dark.iter().enumerate().filter(|(_, dark)| **dark) {
                    let (col, row) = (index % dimension as usize, index / dimension as usize);
                    let at = origin + point(module * col as f32, module * row as f32);
                    window.paint_quad(fill(Bounds::new(at, size(module, module)), rgb(0x000000)));
                }
            },
        )
        .size(side);
        // A soft accent ring breathes around the card. Motion stays outside
        // the code: a coloured line over the modules can read as dark.
        let ring = div()
            .absolute()
            .inset_0()
            .rounded(px(20.0))
            .border_2()
            .border_color(rgb(ACCENT))
            .with_animation(
                "pairing-ring",
                Animation::new(Duration::from_millis(2200)).repeat(),
                |ring, delta| {
                    let wave = 0.5 - 0.5 * (std::f32::consts::TAU * delta).cos();
                    ring.opacity(0.25 + 0.75 * wave)
                },
            );
        let header = div()
            .text_size(px(18.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .child("Scan this code with the tapbridge app");
        if self.expired() {
            // The old code no longer pairs: hide it so nobody scans it.
            let blank = glass::panel("pairing-card", 20.0, cx)
                .size(stage)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(20.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(WARN))
                        .child("Code expired"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(ACCENT_SOFT))
                        .child("Codes work for a few minutes."),
                );
            return root.child(header).child(blank).child(
                button("pair-retry", "Try again", ACCENT, 0.6, cx).on_click(cx.listener(
                    |view, _: &gpui::ClickEvent, _, cx| {
                        view.retry = true;
                        cx.notify();
                    },
                )),
            );
        }
        let card = glass::panel("pairing-card", 20.0, cx)
            .size(stage)
            .flex()
            .items_center()
            .justify_center()
            .child(ring)
            .child(
                div()
                    .size(side)
                    .bg(rgb(0xFFFFFF))
                    .rounded(px(12.0))
                    .overflow_hidden()
                    .when(dimension == 0, |card| {
                        card.child("invitation too long for a QR code")
                    })
                    .child(grid),
            );
        let remaining = self.expires.saturating_duration_since(Instant::now());
        root.child(header)
            .child(card)
            .child(
                div()
                    .text_size(px(26.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(if remaining < EXPIRY_WARNING {
                        WARN
                    } else {
                        ACCENT_SOFT
                    }))
                    .child(countdown(remaining)),
            )
            // While this window is open, plugging the phone in pairs it too
            // (the USB handshake carries the outstanding capability).
            .when(self.usb_hint, |root| {
                root.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(ACCENT_SOFT))
                        .child("Or plug your phone into this computer with a USB cable"),
                )
            })
    }
}

impl ChecklistHost for PairingView {
    fn checklist_action(&mut self, action: SettingsAction, cx: &mut Context<Self>) {
        let on_action = self.on_action.clone();
        cx.defer(move |cx| on_action(action, cx));
    }

    fn install_shown(&self, _: &[u8]) -> bool {
        self.show_install
    }

    fn toggle_install(&mut self, _: Vec<u8>, cx: &mut Context<Self>) {
        self.show_install = !self.show_install;
        cx.notify();
    }
}

impl PairingView {
    /// After the test card came through: the card itself, as Preferences
    /// shows it (minus rename and forget), so its setup can start here.
    fn render_test_card(
        &self,
        phone: &str,
        card: &CardRow,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("pairing-card")
            .absolute()
            .inset_0()
            .pt(px(TITLEBAR_INSET + 20.0))
            .pb(px(20.0))
            .overflow_y_scroll()
            .child(
                div()
                    .w(px(400.0))
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(13.0))
                            .text_color(rgb(GOOD))
                            .child(success_badge(18.0))
                            .child(format!("Paired with {phone}")),
                    )
                    .child(
                        div()
                            .text_size(px(20.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(card.label.clone()),
                    )
                    .child(card_details(self, 0, card, busy, cx))
                    .child(div().flex().flex_row().justify_end().child(
                        card_button("test-done", 0, "Done", ACCENT, 0.6, cx).on_click(
                            cx.listener(|_, _: &gpui::ClickEvent, window, _| {
                                window.remove_window()
                            }),
                        ),
                    )),
            )
    }

    /// The test-tap step under the pairing confirmation.
    fn render_test(&self, cx: &mut Context<Self>) -> impl IntoElement {
        fn close(
            _: &mut PairingView,
            _: &gpui::ClickEvent,
            window: &mut Window,
            _: &mut Context<PairingView>,
        ) {
            window.remove_window();
        }
        let (title, detail, color) = match &self.test {
            Some(TestTap::Works { label }) => (
                format!("It works! {label} was tapped."),
                Some("tapbridge will notify your phone when a website requests a security key."),
                GOOD,
            ),
            Some(TestTap::TimedOut) => (
                "No card arrived".to_owned(),
                Some("Open tapbridge on your phone and hold a card to it."),
                WARN,
            ),
            _ => (
                "Try it out by tapping your card on your phone".to_owned(),
                None,
                TEXT,
            ),
        };
        // A card must come through before setup is done: no Skip. The
        // window can still be closed; the test starts again next pairing.
        let mut buttons = div().flex().flex_row().gap(px(10.0));
        buttons = match &self.test {
            Some(TestTap::Works { .. }) => buttons
                .child(button("test-done", "Done", ACCENT, 0.6, cx).on_click(cx.listener(close))),
            Some(TestTap::TimedOut) => {
                buttons.child(button("test-retry", "Try again", ACCENT, 0.6, cx).on_click(
                    cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                        view.test = Some(TestTap::start(Instant::now()));
                        cx.notify();
                    }),
                ))
            }
            Some(TestTap::Waiting { .. }) | None => buttons,
        };
        div()
            .w(px(380.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(color))
                    .child(title),
            )
            .children(detail.map(|detail| {
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(ACCENT_SOFT))
                    .text_center()
                    .child(detail)
            }))
            .child(div().h(px(4.0)))
            .child(buttons)
    }
}

/// Green disc that springs in with a white check mark drawn as a vector path.
fn success_badge(diameter: f32) -> impl IntoElement {
    div()
        .size(px(diameter))
        .rounded_full()
        .bg(rgb(GOOD))
        .child(
            canvas(
                |_, _, _| {},
                |bounds, (), window, _| {
                    let d = bounds.size.width.min(bounds.size.height);
                    let at = |x: f32, y: f32| bounds.origin + point(d * x, d * y);
                    let mut check = gpui::PathBuilder::stroke(d * 0.085);
                    check.move_to(at(0.29, 0.52));
                    check.line_to(at(0.44, 0.67));
                    check.line_to(at(0.72, 0.37));
                    if let Ok(path) = check.build() {
                        window.paint_path(path, rgb(0xFFFFFF));
                    }
                },
            )
            .size_full(),
        )
        .with_animation(
            "paired-badge",
            Animation::new(Duration::from_millis(550)).with_easing(ease_out_back),
            move |badge, delta| {
                badge
                    .size(px(diameter * (0.4 + 0.6 * delta)))
                    .opacity((delta * 2.0).min(1.0))
            },
        )
}

/// Opens the pairing window, or refreshes it in place while it is still open.
/// A handle to a window the user already closed fails to update, so a fresh
/// window opens instead.
pub fn open_pairing_window(
    invitation: &str,
    usb_hint: bool,
    existing: Option<WindowHandle<PairingView>>,
    on_action: impl Fn(SettingsAction, &mut App) + 'static,
    cx: &mut App,
) -> WindowHandle<PairingView> {
    let on_action: OnSettingsAction = Rc::new(on_action);
    if let Some(handle) = existing {
        let text = invitation.to_owned();
        let actions = on_action.clone();
        let refreshed = handle.update(cx, |view, window, cx| {
            *view = PairingView::new(&text, usb_hint, actions, cx);
            window.activate();
            cx.notify();
        });
        if refreshed.is_ok() {
            return handle;
        }
    }
    let invitation = invitation.to_owned();
    let bounds = Bounds::centered(None, size(px(PAIRING_WINDOW), px(PAIRING_QR_HEIGHT)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Pair phone".into()),
                    appears_transparent: cfg!(target_os = "macos"),
                    ..Default::default()
                }),
                is_resizable: false,
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| PairingView::new(&invitation, usb_hint, on_action, cx)),
        )
        .expect("pairing window");
    window.update(cx, |_, window, _| window.activate()).ok();
    window
}

/// `Some(true)` when the open pairing window's invitation ran out; `None`
/// when the window is closed.
pub fn pairing_wants_new_code(handle: WindowHandle<PairingView>, cx: &mut App) -> Option<bool> {
    handle.update(cx, |view, _, _| view.wants_new_code()).ok()
}

/// Shows the pairing-successful screen with its test-tap step.
pub fn complete_pairing(handle: WindowHandle<PairingView>, phone: String, cx: &mut App) {
    let _ = handle.update(cx, |view, _window, cx| {
        view.paired = Some(phone);
        view.test = Some(TestTap::start(Instant::now()));
        cx.notify();
    });
}

/// Advances the test-tap step. `Some(true)`: ask the phone for a card now.
/// `None` when the window is closed.
pub fn pairing_test_tick(
    handle: WindowHandle<PairingView>,
    connected: bool,
    cx: &mut App,
) -> Option<bool> {
    handle
        .update(cx, |view, _, cx| {
            let before = view.test.clone();
            let ask = view
                .test
                .as_mut()
                .is_some_and(|test| test.tick(Instant::now(), connected));
            if view.test != before {
                cx.notify();
            }
            ask
        })
        .ok()
}

/// Whether the pairing window is waiting for a test card.
pub fn pairing_test_waiting(handle: WindowHandle<PairingView>, cx: &mut App) -> bool {
    handle
        .update(cx, |view, _, _| {
            matches!(view.test, Some(TestTap::Waiting { .. }))
        })
        .unwrap_or(false)
}

/// A card arrived during the test-tap step. `uid` is empty when the card
/// has no stable UID (nothing to show beyond its name).
pub fn pairing_test_card(
    handle: WindowHandle<PairingView>,
    uid: Vec<u8>,
    label: String,
    cx: &mut App,
) {
    let _ = handle.update(cx, |view, _, cx| {
        if let Some(test) = view.test.as_mut() {
            test.card(label);
            if !uid.is_empty() {
                view.test_card = Some((uid, None, false));
            }
            cx.notify();
        }
    });
}

/// The UID of the card the test step showed, while the window is open.
pub fn pairing_test_uid(handle: WindowHandle<PairingView>, cx: &mut App) -> Option<Vec<u8>> {
    handle
        .update(cx, |view, _, _| {
            view.test_card.as_ref().map(|(uid, ..)| uid.clone())
        })
        .ok()
        .flatten()
}

/// Refreshes the test card shown in the pairing window.
pub fn pairing_set_card(
    handle: WindowHandle<PairingView>,
    row: Option<CardRow>,
    busy: bool,
    cx: &mut App,
) {
    let _ = handle.update(cx, |view, window, cx| {
        if let Some((_, shown, was_busy)) = view.test_card.as_mut()
            && (*shown != row || *was_busy != busy)
        {
            if shown.is_none() && row.is_some() {
                window.resize(size(px(PAIRING_WINDOW), px(PAIRING_CARD_HEIGHT)));
            }
            *shown = row;
            *was_busy = busy;
            cx.notify();
        }
    });
}

/// Invisible window that keeps the app alive. gpui on Windows quits the
/// process when its last window closes (`PostQuitMessage` in
/// `close_one_window`), which would end the tray app whenever the user
/// closed the pairing or status window, and after every successful pairing.
pub struct Anchor;

impl Render for Anchor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub fn open_anchor_window(cx: &mut App) -> WindowHandle<Anchor> {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(1.0), px(1.0)),
            ))),
            titlebar: None,
            focus: false,
            show: false,
            // Tool window: no taskbar button even if something shows it.
            kind: gpui::WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        |_, cx| cx.new(|_| Anchor),
    )
    .expect("anchor window")
}

/// Passkey options: which choices the Windows passkey dialog offers.
#[cfg(windows)]
pub struct PasskeyView {
    options: passkeys::Options,
    /// In-window dialog over the options while a change runs or reports.
    dialog: Option<PasskeyDialog>,
}

#[cfg(windows)]
enum PasskeyDialog {
    /// Waiting for the UAC prompt and the SYSTEM policy step.
    Working {
        since: Instant,
    },
    Done {
        ok: bool,
        text: String,
    },
}

#[cfg(windows)]
impl PasskeyView {
    fn new() -> Self {
        Self {
            options: passkeys::read(),
            dialog: None,
        }
    }

    fn toggle_windows_hello(&mut self, cx: &mut Context<Self>) {
        let enable = !self.options.windows_hello;
        if let Err(error) = passkeys::set_windows_hello(enable) {
            self.dialog = Some(PasskeyDialog::Done {
                ok: false,
                text: format!("Couldn't change Windows Hello passkeys: {error}"),
            });
        }
        self.options = passkeys::read();
        cx.notify();
    }

    fn toggle_phone(&mut self, cx: &mut Context<Self>) {
        if self.dialog.is_some() || self.options.phone == passkeys::Phone::ManagedElsewhere {
            return;
        }
        let allow = self.options.phone == passkeys::Phone::Blocked;
        self.dialog = Some(PasskeyDialog::Working {
            since: Instant::now(),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { passkeys::set_phone(allow) })
                .await;
            let _ =
                this.update(cx, |view, cx| {
                    view.options = passkeys::read();
                    view.dialog = Some(match result {
                    Ok(()) if allow => PasskeyDialog::Done {
                        ok: true,
                        text: "Phone sign-in is allowed again.".into(),
                    },
                    Ok(()) => PasskeyDialog::Done {
                        ok: true,
                        text: "Phone sign-in is blocked. Your other Bluetooth devices keep working."
                            .into(),
                    },
                    Err(error) => PasskeyDialog::Done { ok: false, text: error },
                });
                    cx.notify();
                });
        })
        .detach();
    }

    fn render_dialog(&self, dialog: &PasskeyDialog, cx: &mut Context<Self>) -> impl IntoElement {
        let card = div()
            .w(px(380.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.0))
            .p(px(26.0))
            .rounded(px(18.0))
            .bg(rgb(PANEL_2))
            .border_1()
            .border_color(rgb(0x3A3052));
        let card = match dialog {
            PasskeyDialog::Working { since } => card
                .child(spinner(px(56.0), *since))
                .child(
                    div()
                        .text_size(px(17.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Waiting for Windows"),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(ACCENT_SOFT))
                        .text_center()
                        .child(
                            "Approve the administrator prompt (UAC). tapbridge then updates \
                             the Bluetooth policy. This takes a few seconds.",
                        ),
                ),
            PasskeyDialog::Done { ok, text } => card
                .child(if *ok {
                    success_badge(64.0).into_any_element()
                } else {
                    div()
                        .text_size(px(40.0))
                        .text_color(rgb(WARN))
                        .child("!")
                        .into_any_element()
                })
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_center()
                        .text_color(rgb(if *ok { TEXT } else { WARN }))
                        .child(text.clone()),
                )
                .child(
                    div()
                        .id("dialog-done")
                        .px(px(22.0))
                        .py(px(8.0))
                        .rounded(px(10.0))
                        .bg(rgb(ACCENT))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0x6A4BEB)))
                        .text_size(px(14.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("Done")
                        .on_click(cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                            view.dialog = None;
                            cx.notify();
                        })),
                ),
        };
        // Dim backdrop; occlude so the options behind it can't be clicked.
        div()
            .id("dialog-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::rgba(0x0B0814C8))
            .flex()
            .items_center()
            .justify_center()
            .child(card)
    }
}

/// Twelve radial strokes, the brightest one sweeping clockwise. The phase
/// comes from `since` (owned by the dialog, so it survives re-renders); the
/// repeating animation only keeps frames coming.
#[cfg(windows)]
fn spinner(diameter: Pixels, since: Instant) -> impl IntoElement {
    div()
        .size(diameter)
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, (), window, _| {
                    const BARS: usize = 12;
                    let phase = (since.elapsed().as_secs_f32() / 0.9).fract();
                    let head = (phase * BARS as f32).floor() as usize;
                    let center = bounds.center();
                    let radius = bounds.size.width.min(bounds.size.height) / 2.0;
                    for i in 0..BARS {
                        let angle = std::f32::consts::TAU * i as f32 / BARS as f32;
                        let (sin, cos) = angle.sin_cos();
                        let inner = center + point(radius * 0.45 * sin, -(radius * 0.45 * cos));
                        let outer = center + point(radius * 0.92 * sin, -(radius * 0.92 * cos));
                        // Bars trail the head: full alpha at the head, fading
                        // over the previous eleven.
                        let behind = (head + BARS - i) % BARS;
                        let alpha = (255.0 * (1.0 - behind as f32 / BARS as f32)).max(46.0) as u32;
                        let mut bar = gpui::PathBuilder::stroke(radius * 0.15);
                        bar.move_to(inner);
                        bar.line_to(outer);
                        if let Ok(path) = bar.build() {
                            window.paint_path(path, gpui::rgba((ACCENT_SOFT << 8) | alpha));
                        }
                    }
                },
            )
            .size_full(),
        )
        .with_animation(
            "spinner",
            Animation::new(Duration::from_secs(1)).repeat(),
            |spinner, _| spinner,
        )
}

/// Overshoots a little, then settles exactly at 1 (gpui's `bounce` returns
/// to 0, which made the badge vanish).
fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
}

fn checkbox(checked: bool, enabled: bool) -> impl IntoElement {
    let color = if enabled { ACCENT } else { 0x5A5070 };
    div()
        .size(px(22.0))
        .flex_none()
        .rounded(px(6.0))
        .border_2()
        .border_color(rgb(color))
        .when(checked, |box_| {
            box_.bg(rgb(color)).child(
                canvas(
                    |_, _, _| {},
                    |bounds, (), window, _| {
                        let d = bounds.size.width;
                        let at = |x: f32, y: f32| bounds.origin + point(d * x, d * y);
                        let mut check = gpui::PathBuilder::stroke(d * 0.14);
                        check.move_to(at(0.22, 0.52));
                        check.line_to(at(0.42, 0.72));
                        check.line_to(at(0.78, 0.30));
                        if let Ok(path) = check.build() {
                            window.paint_path(path, rgb(0xFFFFFF));
                        }
                    },
                )
                .size_full(),
            )
        })
}

fn option_row(
    id: &'static str,
    checked: bool,
    enabled: bool,
    title: &str,
    detail: String,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    glass::panel(id, 14.0, cx)
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(14.0))
        .p(px(14.0))
        .when(enabled, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(rgba(0xFFFFFF0A)))
        })
        .child(checkbox(checked, enabled))
        .child(
            // flex_1 + min_w(0): take the remaining width and wrap inside it
            // instead of growing past the window edge.
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(if enabled { TEXT } else { 0x8A8199 }))
                        .child(title.to_owned()),
                )
                .when(!detail.is_empty(), |column| {
                    column.child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(ACCENT_SOFT))
                            .child(detail),
                    )
                }),
        )
}

#[cfg(windows)]
impl Render for PasskeyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = &self.options;
        let idle = self.dialog.is_none();
        let phone_enabled = idle && options.phone != passkeys::Phone::ManagedElsewhere;
        let phone_detail = match options.phone {
            passkeys::Phone::Allowed => {
                "Sign in with a phone over QR code and Bluetooth. Unchecking blocks the FIDO \
                 Bluetooth services (asks for admin)."
            }
            passkeys::Phone::Blocked => {
                "Blocked. Bluetooth allows every common service except FIDO."
            }
            passkeys::Phone::ManagedElsewhere => {
                "Your organization manages this PC's Bluetooth services."
            }
        }
        .to_owned();
        let managers_detail = if options.providers.is_empty() {
            "None installed (1Password, Bitwarden...).".to_owned()
        } else {
            format!(
                "{}. Click to switch them in Windows Settings.",
                options
                    .providers
                    .iter()
                    .map(|provider| provider.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let column = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(section_title("Passkeys"))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(ACCENT_SOFT))
                    .pb(px(6.0))
                    .child(
                        "Choose what the Windows passkey dialog offers. Unchecked options \
                         disappear or stop working.",
                    ),
            )
            .child(option_row(
                "opt-key",
                true,
                false,
                "Security key",
                "tapbridge. Always on.".to_owned(),
                cx,
            ))
            .child(
                option_row(
                    "opt-hello",
                    options.windows_hello,
                    idle,
                    "This Windows device",
                    "Passkeys saved with Windows Hello on this PC.".to_owned(),
                    cx,
                )
                .on_click(cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                    if view.dialog.is_none() {
                        view.toggle_windows_hello(cx)
                    }
                })),
            )
            .child(
                option_row(
                    "opt-phone",
                    options.phone != passkeys::Phone::Blocked,
                    phone_enabled,
                    "iPhone, iPad, or Android device",
                    phone_detail,
                    cx,
                )
                .on_click(cx.listener(|view, _: &gpui::ClickEvent, _, cx| view.toggle_phone(cx))),
            )
            .child(
                option_row(
                    "opt-managers",
                    !options.providers.is_empty(),
                    idle && !options.providers.is_empty(),
                    "Passkey managers",
                    managers_detail,
                    cx,
                )
                .when(!options.providers.is_empty(), |row| {
                    row.on_click(|_, _, _| passkeys::open_windows_passkey_settings())
                }),
            );
        // Embedded in Settings: the dialog overlay covers this section only.
        div()
            .relative()
            .child(column)
            .when_some(self.dialog.as_ref(), |root, dialog| {
                root.child(self.render_dialog(dialog, cx))
            })
    }
}

/// What the Settings window asks the shell to do. Runs deferred, after the
/// view's own update, so the shell may refresh or open windows freely.
pub enum SettingsAction {
    Pair,
    Forget,
    SetAutostart(bool),
    SetAlwaysPresent(bool),
    ApplySecurity(SecurityOptions),
    /// "Use USB cable" (persisted by the host service).
    SetUseUsb(bool),
    RenameCard(Vec<u8>, String),
    ForgetCard(Vec<u8>),
    SetUpCard(Vec<u8>),
    /// Ask for a tap so the card's applets are read again (macOS's PIV
    /// driver reads the card when it arrives; the probe watches that).
    CheckCard,
    OpenUrl(String),
    /// Pair the card's login certificate with this PC's login (where
    /// `login::SUPPORTED`).
    SetUpLogin(Vec<u8>),
    /// Unpair the card from this PC's login (the card must be on the phone).
    RemoveLogin(Vec<u8>),
    /// Close the status of the card action (or stop waiting for it).
    DismissCardTask,
}

impl SettingsAction {
    /// For logs: the action without its data.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Forget => "forget phone",
            Self::SetAutostart(_) => "set autostart",
            Self::SetAlwaysPresent(_) => "set always present",
            Self::ApplySecurity(_) => "apply mac security",
            Self::SetUseUsb(_) => "set use usb",
            Self::RenameCard(..) => "rename card",
            Self::ForgetCard(_) => "forget card",
            Self::SetUpCard(_) => "set up certificates",
            Self::CheckCard => "check card",
            Self::OpenUrl(_) => "open url",
            Self::SetUpLogin(_) => "set up login",
            Self::RemoveLogin(_) => "remove login",
            Self::DismissCardTask => "dismiss card task",
        }
    }
}

/// One known card as the Cards section shows it.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct CardRow {
    pub uid: Vec<u8>,
    pub label: String,
    /// "now", "2 days ago"...
    pub last_seen: String,
    pub passkey: bool,
    pub smart_card: bool,
    /// The "macOS login" steps for this card.
    pub checklist: Option<Checklist>,
    /// "How to install" paragraphs for the applet step.
    pub install_steps: Vec<String>,
    /// A flexSecure: link the exact PivApplet build.
    pub flexsecure: bool,
    /// Status line of the action in progress on this card.
    pub task: Option<String>,
    pub task_failed: bool,
    /// The action's result is showing: offer "OK" to clear it.
    pub task_finished: bool,
    /// The action can be closed or stopped (not while sc_auth runs).
    pub task_dismissable: bool,
    /// Offer "Set up login" (this PC supports card login).
    pub offer_login: bool,
}

/// The overall state shown at the top of Settings (mirrors the tray icon).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Health {
    Good,
    #[default]
    Idle,
    Problem,
}

/// Everything Settings displays; rebuilt by the shell every tick while the
/// window is open and pushed in only when it changed.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SettingsSnapshot {
    pub summary: String,
    pub health: Health,
    /// Display name of the paired phone.
    pub phone: Option<String>,
    pub phone_connected: bool,
    /// The live session runs over the USB cable.
    pub usb: bool,
    /// "Use USB cable".
    pub use_usb: bool,
    pub autostart: bool,
    /// The login card always shows as present; `None` where unsupported.
    pub always_present: Option<bool>,
    /// The Mac's smart card settings as they are now; `None` off macOS.
    pub security: Option<Security>,
    /// Installed through nix-darwin: settings are shown, not changed.
    pub managed: bool,
    /// Known cards, shown once a phone is paired.
    pub cards: Vec<CardRow>,
    /// A card action is running: no new one can start.
    pub card_busy: bool,
}

/// The Mac's smart card settings (Apple's support article 101438), kept
/// platform-free here so the UI builds everywhere. `Default` is all off;
/// see `mac_security::SecurityConfig::macos_default` for macOS's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SecurityOptions {
    /// `allowSmartCard`: cards may log in, unlock and authorize.
    pub allow_smart_card: bool,
    /// `UserPairing`: macOS offers to pair a new card when it's inserted.
    pub user_pairing: bool,
    /// `checkCertificateTrust`, 0 to 3.
    pub check_certificate_trust: u8,
    /// `oneCardPerUser`.
    pub one_card_per_user: bool,
    /// `tokenRemovalAction = 1`: lock the screen when the card goes away.
    pub lock_on_removal: bool,
    /// `enforceSmartCard`: passwords stop working.
    pub enforce_smart_card: bool,
    pub sudo: bool,
    pub su: bool,
    pub login: bool,
    pub ssh_client: bool,
    pub sshd_cards_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Security {
    pub live: SecurityOptions,
    /// Applying (admin prompt or profile approval) is in progress.
    pub busy: bool,
    /// How the last apply went.
    pub status: Option<String>,
}

/// One checkbox of the security section.
struct SecurityRow {
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    get: fn(&SecurityOptions) -> bool,
    set: fn(&mut SecurityOptions, bool),
}

const SECURITY_ROWS: &[SecurityRow] = &[
    SecurityRow {
        id: "sec-allow",
        title: "Allow smart card login",
        detail: "Cards can log in, unlock the screen and approve admin prompts. Needs a restart.",
        get: |o| o.allow_smart_card,
        set: |o, v| o.allow_smart_card = v,
    },
    SecurityRow {
        id: "sec-pairing",
        title: "Offer to pair new cards",
        detail: "macOS asks to link a card to your account when it's inserted.",
        get: |o| o.user_pairing,
        set: |o, v| o.user_pairing = v,
    },
    SecurityRow {
        id: "sec-one",
        title: "One card per user",
        detail: "Each account can be paired with only one card.",
        get: |o| o.one_card_per_user,
        set: |o, v| o.one_card_per_user = v,
    },
    SecurityRow {
        id: "sec-lock",
        title: "Lock when the card is removed",
        detail: "The screen locks when your card goes away.",
        get: |o| o.lock_on_removal,
        set: |o, v| o.lock_on_removal = v,
    },
    SecurityRow {
        id: "sec-sudo",
        title: "Card for sudo",
        detail: "Your card works for sudo in Terminal.",
        get: |o| o.sudo,
        set: |o, v| o.sudo = v,
    },
    SecurityRow {
        id: "sec-su",
        title: "Card for su",
        detail: "Your card works for su in Terminal.",
        get: |o| o.su,
        set: |o, v| o.su = v,
    },
    SecurityRow {
        id: "sec-login",
        title: "Card for login",
        detail: "Your card works for the login command in Terminal.",
        get: |o| o.login,
        set: |o, v| o.login = v,
    },
    SecurityRow {
        id: "sec-ssh",
        title: "SSH with your card",
        detail: "ssh offers your card's key to servers.",
        get: |o| o.ssh_client,
        set: |o, v| o.ssh_client = v,
    },
    SecurityRow {
        id: "sec-sshd",
        title: "No passwords over SSH",
        detail: "This Mac's SSH server only accepts keys and cards.",
        get: |o| o.sshd_cards_only,
        set: |o, v| o.sshd_cards_only = v,
    },
    SecurityRow {
        id: "sec-enforce",
        title: "Smart card only",
        detail: "Passwords stop working: no card, no login. tapbridge must be \
                 running and your phone reachable. Make sure card login works first.",
        get: |o| o.enforce_smart_card,
        set: |o, v| o.enforce_smart_card = v,
    },
];

const TRUST_LEVELS: [&str; 4] = [
    "Off",
    "Check",
    "Check + soft revocation",
    "Check + hard revocation",
];

type OnSettingsAction = Rc<dyn Fn(SettingsAction, &mut App)>;

pub struct SettingsView {
    snapshot: SettingsSnapshot,
    on_action: OnSettingsAction,
    /// "Forget phone" asks once more before revoking the pairing.
    confirm_forget: bool,
    /// "Forget" on a card asks once more, too.
    confirm_forget_card: Option<Vec<u8>>,
    /// The card dialog on screen (login setup, or naming a new card).
    dialog: Option<CardDialog>,
    /// Cards showing the applet install instructions.
    show_install: std::collections::HashSet<Vec<u8>>,
    /// Security settings being edited, before Apply.
    security_draft: Option<SecurityOptions>,
    /// "Smart card only" asks once more before applying.
    confirm_enforce: bool,
    /// Card being renamed, its text field, and the field's event subscription.
    editing: Option<(Vec<u8>, gpui::Entity<TextInput>, Vec<gpui::Subscription>)>,
    focus: FocusHandle,
    #[cfg(windows)]
    passkeys: Entity<PasskeyView>,
}

impl SettingsView {
    fn new(
        snapshot: SettingsSnapshot,
        on_action: OnSettingsAction,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            snapshot,
            on_action,
            confirm_forget: false,
            confirm_forget_card: None,
            dialog: None,
            show_install: Default::default(),
            security_draft: None,
            confirm_enforce: false,
            editing: None,
            focus: cx.focus_handle(),
            #[cfg(windows)]
            passkeys: cx.new(|_| PasskeyView::new()),
        }
    }

    pub fn set_snapshot(&mut self, snapshot: SettingsSnapshot, cx: &mut Context<Self>) {
        if self.snapshot != snapshot {
            if snapshot.phone.is_none() {
                self.confirm_forget = false;
            }
            let known = |uid: &Vec<u8>| snapshot.cards.iter().any(|card| &card.uid == uid);
            if self
                .dialog
                .as_ref()
                .is_some_and(|dialog| !known(&dialog.uid().to_vec()))
            {
                self.dialog = None;
            }
            if self.editing.as_ref().is_some_and(|(uid, ..)| !known(uid)) {
                self.editing = None;
            }
            if self
                .confirm_forget_card
                .as_ref()
                .is_some_and(|uid| !known(uid))
            {
                self.confirm_forget_card = None;
            }
            if let Some(security) = snapshot.security.as_ref()
                && self.security_draft == Some(security.live)
            {
                self.security_draft = None;
            }
            self.snapshot = snapshot;
            cx.notify();
        }
    }

    fn act(&mut self, action: SettingsAction, cx: &mut Context<Self>) {
        let on_action = self.on_action.clone();
        cx.defer(move |cx| on_action(action, cx));
    }

    /// Opens the rename field for a card, focused with the name selected.
    fn start_rename(
        &mut self,
        uid: Vec<u8>,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            TextInput::new(label, tapbridge_host::cards::MAX_LABEL, cx).placeholder("Card name")
        });
        // Return saves, Escape cancels.
        let subscription = cx.subscribe(&input, |view, _, event: &InputEvent, cx| match event {
            InputEvent::Submit(_) => view.finish_rename(true, cx),
            InputEvent::Cancel if view.dialog.is_some() => {
                view.finish_rename(false, cx);
                view.dialog = None;
            }
            InputEvent::Cancel => view.finish_rename(false, cx),
        });
        let handle = gpui::Focusable::focus_handle(input.read(cx), cx);
        // Clicking elsewhere keeps the typed name, like Return.
        let blur = cx.on_blur(&handle, window, |view, _, cx| view.finish_rename(true, cx));
        handle.focus(window, cx);
        self.editing = Some((uid, input, vec![subscription, blur]));
        self.confirm_forget_card = None;
        cx.notify();
    }

    fn finish_rename(&mut self, save: bool, cx: &mut Context<Self>) {
        if let Some((uid, input, _)) = self.editing.take() {
            let text = input.read(cx).text();
            if save && !text.trim().is_empty() {
                self.act(SettingsAction::RenameCard(uid, text), cx);
            }
        }
        cx.notify();
    }

    fn render_cards(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let snapshot = &self.snapshot;
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(section_title("Cards"));
        if snapshot.cards.is_empty() {
            section = section.child(
                glass::panel("cards-empty", 14.0, cx)
                    .p(px(14.0))
                    .text_size(px(12.5))
                    .text_color(rgb(ACCENT_SOFT))
                    .child("Tap a card on your phone and it shows up here."),
            );
        }
        for (index, card) in snapshot.cards.iter().enumerate() {
            section = section.child(self.render_card(index, card, cx));
        }
        section
    }

    fn render_card(&self, index: usize, card: &CardRow, cx: &mut Context<Self>) -> gpui::Div {
        // The dialog owns the field while it shows this card.
        let in_dialog = self
            .dialog
            .as_ref()
            .is_some_and(|d| d.uid() == card.uid.as_slice());
        let editing = self
            .editing
            .as_ref()
            .filter(|(uid, ..)| *uid == card.uid && !in_dialog)
            .map(|(_, input, _)| input.clone());
        let title = match &editing {
            Some(input) => div().child(input.clone()),
            None => editable_name(("card-name", index), &card.uid, &card.label, 15.0, cx),
        };
        let mut badges = div().flex().flex_row().gap(px(6.0));
        for (shown, text) in [(card.passkey, "Passkey"), (card.smart_card, "Smart card")] {
            if shown {
                badges = badges.child(
                    div()
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded_full()
                        .bg(rgba(0x7C5CFF40))
                        .text_size(px(11.0))
                        .text_color(rgb(ACCENT_SOFT))
                        .child(text),
                );
            }
        }
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(title)
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(ACCENT_SOFT))
                            .child(format!("Last seen {}", card.last_seen)),
                    )
                    .when(crate::login::SUPPORTED, |column| {
                        column.child(capability_marks(
                            card.passkey,
                            card.checklist.is_some_and(|c| c.ready()),
                        ))
                    }),
            )
            .child(badges)
            // Card settings: login steps and Forget.
            .when(editing.is_none(), |header| {
                let open_uid = card.uid.clone();
                header.child(
                    div()
                        .id(("card-settings", index))
                        .flex_none()
                        .self_start()
                        .p(px(4.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgba(0xFFFFFF14)))
                        .child(
                            gpui::svg()
                                .data(GEAR_SVG)
                                .size(px(16.0))
                                .text_color(rgb(ACCENT_SOFT)),
                        )
                        .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                            view.confirm_forget_card = None;
                            view.dialog = Some(CardDialog::Settings(open_uid.clone()));
                            cx.notify();
                        })),
                )
            });
        let mut panel = glass::panel(&format!("card-{index}"), 14.0, cx)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(14.0))
            .child(header);
        if let Some(task) = &card.task {
            let mut line = div().flex().flex_row().items_center().gap(px(10.0)).child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.5))
                    .text_color(rgb(if card.task_failed { WARN } else { TEXT }))
                    .child(task.clone()),
            );
            let dismiss = if card.task_finished { "OK" } else { "Cancel" };
            if card.task_dismissable {
                line = line.child(
                    card_button("card-dismiss", index, dismiss, 0xFFFFFF, 0.06, cx).on_click(
                        cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                            view.act(SettingsAction::DismissCardTask, cx)
                        }),
                    ),
                );
            }
            panel = panel.child(line);
        }
        panel
    }
}

/// A card's name that shows a box on hover; clicking it turns it into a
/// text field (Return or clicking away saves, Escape cancels).
fn editable_name(
    id: (&'static str, usize),
    uid: &[u8],
    label: &str,
    size: f32,
    cx: &mut Context<SettingsView>,
) -> gpui::Div {
    let uid = uid.to_vec();
    let text = label.to_owned();
    div().flex().child(
        div()
            .id(id)
            .px(px(6.0))
            .py(px(2.0))
            .ml(px(-7.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgba(0x00000000))
            .cursor_text()
            .hover(|style| style.border_color(rgba(0xFFFFFF40)).bg(rgba(0xFFFFFF0A)))
            .text_size(px(size))
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(label.to_owned())
            .on_click(cx.listener(move |view, _: &gpui::ClickEvent, window, cx| {
                view.start_rename(uid.clone(), &text, window, cx)
            })),
    )
}

/// A window that shows a card's login checklist and carries out its
/// buttons: Preferences, and the pairing window's test step.
trait ChecklistHost: Sized + 'static {
    fn checklist_action(&mut self, action: SettingsAction, cx: &mut Context<Self>);
    fn install_shown(&self, uid: &[u8]) -> bool;
    fn toggle_install(&mut self, uid: Vec<u8>, cx: &mut Context<Self>);
}

/// What a card can do and its login steps, with the running action's
/// status: the body of the card dialog, also embedded in the pairing
/// window once the test card arrives.
fn card_details<V: ChecklistHost>(
    host: &V,
    index: usize,
    card: &CardRow,
    busy: bool,
    cx: &mut Context<V>,
) -> gpui::Div {
    let mut details = div().flex().flex_col().gap(px(14.0));
    if crate::login::SUPPORTED {
        details = details.child(capability_marks(
            card.passkey,
            card.checklist.is_some_and(|c| c.ready()),
        ));
    }
    if let Some(checklist) = card.checklist
        && crate::login::SUPPORTED
    {
        details = details
            .child(div().h(px(1.0)).bg(rgba(0xFFFFFF1F)))
            .child(render_checklist(host, index, card, checklist, busy, cx));
    }
    if let Some(task) = &card.task {
        details = details.child(
            div()
                .text_size(px(12.5))
                .text_color(rgb(if card.task_failed { WARN } else { TEXT }))
                .child(task.clone()),
        );
    }
    details
}

/// The three login steps: applet, certificates, OS login. Each button
/// runs on the device that does the work: the phone creates
/// certificates, this PC pairs its login.
fn render_checklist<V: ChecklistHost>(
    host: &V,
    index: usize,
    card: &CardRow,
    checklist: Checklist,
    busy: bool,
    cx: &mut Context<V>,
) -> gpui::Div {
    {
        let uid = card.uid.clone();
        let mut list = div().flex().flex_col().gap(px(14.0)).child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child("Log into your computer with this card"),
        );

        // 1. Applet.
        let applet_detail = match checklist.applet {
            StepState::Done => "The card has a PIV smart-card applet.",
            StepState::Problem => "The applet on this card blocks its keys over NFC.",
            _ => "Your card needs a PIV smart-card applet first.",
        };
        let mut applet_actions = div().flex().flex_row().flex_wrap().gap(px(8.0));
        if checklist.applet != StepState::Done {
            let install_uid = uid.clone();
            let showing = host.install_shown(&card.uid);
            applet_actions = applet_actions.child(
                card_button(
                    "step-install",
                    index,
                    if showing {
                        "Hide instructions"
                    } else {
                        "How to install"
                    },
                    0xFFFFFF,
                    0.06,
                    cx,
                )
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    view.toggle_install(install_uid.clone(), cx);
                })),
            );
            if !busy {
                applet_actions = applet_actions.child(
                    card_button("step-check", index, "Check card", ACCENT, 0.6, cx).on_click(
                        cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                            view.checklist_action(SettingsAction::CheckCard, cx)
                        }),
                    ),
                );
            }
        }
        let mut applet_body = div().flex().flex_col().gap(px(8.0)).child(applet_actions);
        if checklist.applet != StepState::Done && host.install_shown(&card.uid) {
            let mut how = div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .text_size(px(12.0))
                .text_color(rgb(TEXT));
            for line in &card.install_steps {
                how = how.child(div().child(line.clone()));
            }
            let mut links = div().flex().flex_row().flex_wrap().gap(px(8.0));
            for (id, label, url) in [
                (
                    "link-gp",
                    "GlobalPlatform Mobile",
                    crate::card_tasks::GP_MOBILE_URL,
                ),
                (
                    "link-pivapplet",
                    if card.flexsecure {
                        "PivApplet v0.9.0 (REePAxaD)"
                    } else {
                        "PivApplet releases"
                    },
                    crate::card_tasks::PIVAPPLET_RELEASE_URL,
                ),
            ] {
                links = links.child(card_button(id, index, label, 0xFFFFFF, 0.06, cx).on_click(
                    cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                        view.checklist_action(SettingsAction::OpenUrl(url.to_owned()), cx)
                    }),
                ));
            }
            applet_body = applet_body.child(how.child(links));
        }
        list = list.child(step_row(
            1,
            "Smart-card applet installed",
            applet_detail,
            checklist.applet,
            applet_body,
        ));

        // 2. Certificates (made on the phone).
        let mut certs_body = div().flex().flex_row();
        if checklist.certificates == StepState::Todo && !busy {
            let setup_uid = uid.clone();
            certs_body = certs_body.child(
                card_button("step-certs", index, "Create on phone", ACCENT, 0.6, cx).on_click(
                    cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                        view.checklist_action(SettingsAction::SetUpCard(setup_uid.clone()), cx)
                    }),
                ),
            );
        }
        list = list.child(step_row(
            2,
            "Certificates created",
            match checklist.certificates {
                StepState::Done => "The login certificate is on the card.",
                StepState::Locked => "Needs the applet.",
                _ => "Your phone creates them. Existing certificates are never replaced.",
            },
            checklist.certificates,
            certs_body,
        ));

        // 3. OS login (paired on this PC).
        let mut login_body = div().flex().flex_row().flex_wrap().gap(px(8.0));
        let can_act = !busy && crate::login::SUPPORTED && checklist.login != StepState::Locked;
        if checklist.login == StepState::Todo && can_act {
            let login_uid = uid.clone();
            login_body = login_body.child(
                card_button("step-login", index, "Set up login", ACCENT, 0.6, cx).on_click(
                    cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                        view.checklist_action(SettingsAction::SetUpLogin(login_uid.clone()), cx)
                    }),
                ),
            );
        }
        if can_act {
            // Also offered while "Todo": a pairing made outside tapbridge
            // (or one we don't know about yet) can be removed too.
            let remove_uid = uid.clone();
            login_body = login_body.child(
                card_button("step-unlogin", index, "Remove login", 0xFFFFFF, 0.06, cx).on_click(
                    cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                        view.checklist_action(SettingsAction::RemoveLogin(remove_uid.clone()), cx)
                    }),
                ),
            );
        }
        let unlocks = format!("This card unlocks your {}.", crate::login::OS_NAME);
        list.child(step_row(
            3,
            &format!("{} login", crate::login::OS_NAME),
            match checklist.login {
                StepState::Done => &unlocks,
                StepState::Locked => "Needs certificates.",
                _ if crate::login::SUPPORTED => crate::login::PAIR_HINT,
                _ => "Not available on this computer yet.",
            },
            checklist.login,
            login_body,
        ))
    }
}

/// What the card dialog shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CardDialog {
    /// "Card settings": login steps and Forget for this card.
    Settings(Vec<u8>),
    /// A card tapped for the first time: name it, then the same steps.
    New(Vec<u8>),
}

impl CardDialog {
    fn uid(&self) -> &[u8] {
        match self {
            Self::Settings(uid) | Self::New(uid) => uid,
        }
    }
}

/// "✓ 2FA  ✓ Login" under a card's name (where card login is supported).
fn capability_marks(two_factor: bool, login: bool) -> gpui::Div {
    let mark = |on: bool, label: &'static str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .text_size(px(12.0))
            .text_color(rgb(if on { GOOD } else { 0x8A84A3 }))
            .child(if on { "\u{2713}" } else { "\u{2715}" })
            .child(
                div()
                    .text_color(rgb(if on { TEXT } else { 0x8A84A3 }))
                    .child(label),
            )
    };
    div()
        .flex()
        .flex_row()
        .gap(px(12.0))
        .pt(px(2.0))
        .child(mark(two_factor, "2FA"))
        .child(mark(login, "Login"))
}

/// One checklist row: mark, then title, detail and `body` (actions,
/// instructions) stacked beside it.
fn step_row(number: u8, title: &str, detail: &str, state: StepState, body: gpui::Div) -> gpui::Div {
    let (mark, color) = match state {
        StepState::Done => ("\u{2713}".to_owned(), GOOD),
        StepState::Problem => ("!".to_owned(), WARN),
        StepState::Todo => (number.to_string(), ACCENT_SOFT),
        StepState::Locked => (number.to_string(), 0x8A84A3),
    };
    let dim = state == StepState::Locked;
    div()
        .flex()
        .flex_row()
        .gap(px(10.0))
        .child(
            div()
                .flex_none()
                .size(px(22.0))
                .rounded_full()
                .border_1()
                .border_color(rgb(color))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(color))
                .child(mark),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(if dim { 0x8A84A3 } else { TEXT }))
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(if state == StepState::Problem {
                            WARN
                        } else {
                            ACCENT_SOFT
                        }))
                        .child(detail.to_owned()),
                )
                .child(body),
        )
}

/// A button inside a card row; `index` keeps ids and glass keys unique.
fn card_button(
    id: &'static str,
    index: usize,
    label: impl Into<gpui::SharedString>,
    tint: u32,
    opacity: f32,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    glass::capsule(&format!("{id}-{index}"), tint, opacity, cx)
        .id((id, index))
        .flex_none()
        .px(px(14.0))
        .py(px(6.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .text_size(px(12.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into())
}

/// The nix-darwin guide in the tapbridge repo.
pub const MANAGED_GUIDE: &str =
    "https://github.com/ethernet8023/tapbridge/blob/main/docs/nix-darwin.md";

/// Shown above the (grayed out) settings when nix-darwin manages them.
fn managed_notice(cx: &mut App) -> impl IntoElement {
    glass::panel("managed", 14.0, cx)
        .id("managed")
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(14.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF0A)))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child("Managed by nix-darwin"),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(rgb(ACCENT_SOFT))
                .child("Change these settings in programs.tapbridge in your nix-darwin config. Open the guide →"),
        )
        .on_click(|_, _, cx| cx.open_url(MANAGED_GUIDE))
}

fn section_title(title: &str) -> impl IntoElement {
    div()
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(ACCENT_SOFT))
        .child(title.to_uppercase())
}

fn button(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    tint: u32,
    opacity: f32,
    cx: &mut App,
) -> gpui::Stateful<gpui::Div> {
    glass::capsule(id, tint, opacity, cx)
        .id(id)
        .flex_none()
        .px(px(16.0))
        .py(px(8.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(0xFFFFFF14)))
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into())
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = &self.snapshot;
        let dot = match snapshot.health {
            Health::Good => GOOD,
            Health::Idle => ACCENT_SOFT,
            Health::Problem => WARN,
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .child(div().size(px(10.0)).flex_none().rounded_full().bg(rgb(dot)))
            .child(
                div()
                    .text_size(px(17.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(snapshot.summary.clone()),
            );

        let (phone_title, phone_detail) = match &snapshot.phone {
            Some(name) if snapshot.phone_connected && snapshot.usb => {
                (name.clone(), Some("Paired & connected by USB"))
            }
            Some(name) if snapshot.phone_connected => (name.clone(), Some("Paired & online")),
            Some(name) => (name.clone(), Some("Paired")),
            None => ("No phone paired".to_owned(), None),
        };
        let phone_button = if snapshot.phone.is_some() {
            let label = if self.confirm_forget {
                "Click again to forget"
            } else {
                "Forget phone"
            };
            button(
                "forget",
                label,
                if self.confirm_forget {
                    0xE0455A
                } else {
                    0xFFFFFF
                },
                if self.confirm_forget { 0.55 } else { 0.06 },
                cx,
            )
            .on_click(cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                if view.confirm_forget {
                    view.confirm_forget = false;
                    view.act(SettingsAction::Forget, cx);
                } else {
                    view.confirm_forget = true;
                }
                cx.notify();
            }))
        } else {
            button("pair", "Pair a phone", ACCENT, 0.6, cx).on_click(
                cx.listener(|view, _: &gpui::ClickEvent, _, cx| view.act(SettingsAction::Pair, cx)),
            )
        };
        let phone = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(section_title("Phone"))
            .child(
                glass::panel("phone-card", 14.0, cx)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(14.0))
                    .p(px(14.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(phone_title),
                            )
                            .children(phone_detail.map(|detail| {
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(ACCENT_SOFT))
                                    .child(detail)
                            })),
                    )
                    .child(phone_button),
            );

        let autostart = snapshot.autostart;
        let managed = snapshot.managed;
        let use_usb = snapshot.use_usb;
        // "Smart card only" needs the cable as a way in when the network
        // is down, so USB can't be turned off while it's on.
        let enforced = snapshot
            .security
            .as_ref()
            .is_some_and(|security| security.live.enforce_smart_card);
        let usb_locked = enforced && use_usb;
        let general = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(section_title("General"))
            .when(managed, |general| general.child(managed_notice(cx)))
            .child(
                option_row(
                    "opt-login",
                    autostart,
                    !managed,
                    "Start at login",
                    String::new(),
                    cx,
                )
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    if managed {
                        return;
                    }
                    // Optimistic; the shell's next snapshot has the real state.
                    view.snapshot.autostart = !autostart;
                    view.act(SettingsAction::SetAutostart(!autostart), cx);
                    cx.notify();
                })),
            )
            .child(
                option_row(
                    "opt-usb",
                    use_usb,
                    !managed && !usb_locked,
                    "Connect to phone via USB",
                    if usb_locked {
                        "Needed while \"Smart card only\" is on: the cable still works \
                         when the network doesn't."
                            .to_owned()
                    } else {
                        String::new()
                    },
                    cx,
                )
                .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                    if managed || usb_locked {
                        return;
                    }
                    view.snapshot.use_usb = !use_usb;
                    view.act(SettingsAction::SetUseUsb(!use_usb), cx);
                    cx.notify();
                })),
            )
            .when_some(snapshot.always_present, |general, always| {
                general.child(
                    option_row(
                        "opt-always-present",
                        always,
                        !managed,
                        "Card always ready",
                        "Your login card shows as inserted even before you tap it, so \
                         the lock screen and sudo ask for it straight away. If you \
                         don't tap within 25 seconds, you can use your password."
                            .to_owned(),
                        cx,
                    )
                    .on_click(cx.listener(
                        move |view, _: &gpui::ClickEvent, _, cx| {
                            if managed {
                                return;
                            }
                            view.snapshot.always_present = Some(!always);
                            view.act(SettingsAction::SetAlwaysPresent(!always), cx);
                            cx.notify();
                        },
                    )),
                )
            });

        let column = div()
            .flex()
            .flex_col()
            .gap(px(22.0))
            .p(px(22.0))
            .pt(px(TITLEBAR_INSET + 14.0))
            .child(header)
            .child(phone)
            .when(snapshot.phone.is_some(), |column| {
                column.child(self.render_cards(cx))
            })
            .child(general)
            .children(self.render_security(cx));
        #[cfg(windows)]
        let column = column.child(self.passkeys.clone());
        let dialog = self.render_dialog(cx);
        div()
            .relative()
            .size_full()
            .text_color(rgb(TEXT))
            .child(glass::backdrop())
            .child(
                div()
                    .id("settings")
                    .track_focus(&self.focus)
                    .absolute()
                    .inset_0()
                    .overflow_y_scroll()
                    .child(column),
            )
            .children(dialog)
    }
}

impl ChecklistHost for SettingsView {
    fn checklist_action(&mut self, action: SettingsAction, cx: &mut Context<Self>) {
        self.act(action, cx);
    }

    fn install_shown(&self, uid: &[u8]) -> bool {
        self.show_install.contains(uid)
    }

    fn toggle_install(&mut self, uid: Vec<u8>, cx: &mut Context<Self>) {
        if !self.show_install.remove(&uid) {
            self.show_install.insert(uid);
        }
        cx.notify();
    }
}

impl SettingsView {
    /// Opens the "New card" dialog for a card seen for the first time,
    /// unless another dialog is up.
    pub fn new_card(&mut self, uid: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() || !self.snapshot.cards.iter().any(|card| card.uid == uid) {
            return;
        }
        let label = self
            .snapshot
            .cards
            .iter()
            .find(|card| card.uid == uid)
            .map(|card| card.label.clone())
            .unwrap_or_default();
        self.dialog = Some(CardDialog::New(uid.clone()));
        self.start_rename(uid, &label, window, cx);
    }

    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.confirm_forget_card = None;
        if let Some(dialog) = self.dialog.take()
            && self
                .editing
                .as_ref()
                .is_some_and(|(uid, ..)| uid.as_slice() == dialog.uid())
        {
            // A name typed into the New card dialog is kept.
            self.finish_rename(matches!(dialog, CardDialog::New(_)), cx);
        }
        cx.notify();
    }

    /// "Mac security": every smart card setting from Apple's article, with
    /// one Apply for all of them.
    fn render_security(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let security = self.snapshot.security.clone()?;
        let live = security.live;
        let managed = self.snapshot.managed;
        let editable = !managed && !security.busy;
        let draft = self.security_draft.unwrap_or(live);
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(section_title("Mac security"));
        let use_usb = self.snapshot.use_usb;
        for (index, row) in SECURITY_ROWS.iter().enumerate() {
            let checked = (row.get)(&draft);
            // "Smart card only" can't be turned on without the USB link.
            let needs_usb = row.id == "sec-enforce" && !use_usb && !checked;
            let editable = editable && !needs_usb;
            let detail = if needs_usb {
                "Turn on \"Connect to phone via USB\" first: without the cable, \
                 a network problem would lock you out."
            } else {
                row.detail
            };
            section = section.child(
                option_row(row.id, checked, editable, row.title, detail.to_owned(), cx).on_click(
                    cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                        if !editable {
                            return;
                        }
                        let mut draft = view.security_draft.unwrap_or(live);
                        (SECURITY_ROWS[index].set)(&mut draft, !checked);
                        view.security_draft = Some(draft);
                        view.confirm_enforce = false;
                        cx.notify();
                    }),
                ),
            );
        }
        let level = usize::from(draft.check_certificate_trust.min(3));
        let trust_ids = ["trust-0", "trust-1", "trust-2", "trust-3"];
        let mut levels = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for (value, label) in TRUST_LEVELS.iter().enumerate() {
            let chosen = level == value;
            let (tint, opacity) = if chosen {
                (ACCENT, 0.6)
            } else {
                (0xFFFFFF, 0.08)
            };
            levels = levels.child(
                button(trust_ids[value], *label, tint, opacity, cx).on_click(cx.listener(
                    move |view, _: &gpui::ClickEvent, _, cx| {
                        if !editable {
                            return;
                        }
                        let mut draft = view.security_draft.unwrap_or(live);
                        draft.check_certificate_trust = value as u8;
                        view.security_draft = Some(draft);
                        cx.notify();
                    },
                )),
            );
        }
        section = section.child(
            glass::panel("sec-trust", 14.0, cx)
                .flex()
                .flex_col()
                .gap(px(8.0))
                .p(px(14.0))
                .child(
                    div()
                        .text_size(px(15.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(if editable { TEXT } else { 0x8A8199 }))
                        .child("Certificate check"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(ACCENT_SOFT))
                        .child(
                            "Only accept cards whose certificate a trusted authority issued. \
                     tapbridge makes self-signed certificates, so leave this off.",
                        ),
                )
                .child(levels),
        );
        let mut footer = div().flex().flex_col().gap(px(8.0));
        if let Some(status) = security.status.clone() {
            footer = footer.child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(ACCENT_SOFT))
                    .child(status),
            );
        }
        if security.busy {
            footer = footer.child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(ACCENT_SOFT))
                    .child("Applying…"),
            );
        } else if draft.enforce_smart_card && !use_usb {
            // Possible only if USB went off under a draft; Apply would refuse.
            footer = footer.child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(WARN))
                    .child("\"Smart card only\" needs \"Connect to phone via USB\"."),
            );
        } else if draft != live && !managed {
            let enforcing = draft.enforce_smart_card && !live.enforce_smart_card;
            let confirm = self.confirm_enforce;
            let label = match (enforcing, confirm) {
                (true, false) => "Apply…",
                (true, true) => "Turn off passwords",
                _ => "Apply",
            };
            let apply_tint = if enforcing { WARN } else { ACCENT };
            footer = footer
                .when(enforcing && confirm, |footer| {
                    footer.child(div().text_size(px(12.5)).text_color(rgb(WARN)).child(
                        "Passwords will stop working on this Mac. If tapbridge or your \
                         phone can't reach the card, you'll need macOS Recovery to get \
                         back in.",
                    ))
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(button("sec-revert", "Revert", 0xFFFFFF, 0.08, cx).on_click(
                            cx.listener(|view, _: &gpui::ClickEvent, _, cx| {
                                view.security_draft = None;
                                view.confirm_enforce = false;
                                cx.notify();
                            }),
                        ))
                        .child(button("sec-apply", label, apply_tint, 0.6, cx).on_click(
                            cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
                                if enforcing && !view.confirm_enforce {
                                    view.confirm_enforce = true;
                                } else {
                                    view.confirm_enforce = false;
                                    view.act(SettingsAction::ApplySecurity(draft), cx);
                                }
                                cx.notify();
                            }),
                        )),
                );
        }
        Some(section.child(footer).into_any_element())
    }

    /// The card dialog over a blurred window: frosted scrim, glass sheet.
    fn render_dialog(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let dialog = self.dialog.clone()?;
        let Some((index, card)) = self
            .snapshot
            .cards
            .iter()
            .enumerate()
            .find(|(_, card)| card.uid.as_slice() == dialog.uid())
            .map(|(index, card)| (index, card.clone()))
        else {
            self.dialog = None;
            return None;
        };
        let is_new = matches!(dialog, CardDialog::New(_));
        let name: gpui::AnyElement =
            match self.editing.as_ref().filter(|(uid, ..)| *uid == card.uid) {
                Some((_, input, _)) => div().child(input.clone()).into_any_element(),
                None => editable_name(("dialog-name", index), &card.uid, &card.label, 17.0, cx)
                    .into_any_element(),
            };
        let mut sheet = glass::dialog("card-dialog", 18.0, cx)
            .id("card-dialog")
            .occlude()
            .w(px(400.0))
            .max_h(px(560.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(20.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(ACCENT_SOFT))
                    .child(if is_new { "NEW CARD" } else { "CARD SETTINGS" }),
            )
            .child(name)
            .child(card_details(
                self,
                index,
                &card,
                self.snapshot.card_busy,
                cx,
            ));
        let done = if is_new { "Done" } else { "Close" };
        let confirming = self.confirm_forget_card.as_ref() == Some(&card.uid);
        let forget_uid = card.uid.clone();
        let forget = card_button(
            "card-forget",
            index,
            if confirming {
                "Click again to forget"
            } else {
                "Forget card"
            },
            if confirming { 0xE0455A } else { 0xFFFFFF },
            if confirming { 0.55 } else { 0.06 },
            cx,
        )
        .on_click(cx.listener(move |view, _: &gpui::ClickEvent, _, cx| {
            if view.confirm_forget_card.as_ref() == Some(&forget_uid) {
                view.confirm_forget_card = None;
                view.act(SettingsAction::ForgetCard(forget_uid.clone()), cx);
                view.close_dialog(cx);
            } else {
                view.confirm_forget_card = Some(forget_uid.clone());
            }
            cx.notify();
        }));
        sheet = sheet.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(forget)
                .child(
                    card_button("dialog-close", index, done, 0xFFFFFF, 0.08, cx).on_click(
                        cx.listener(|view, _: &gpui::ClickEvent, _, cx| view.close_dialog(cx)),
                    ),
                ),
        );
        Some(
            div()
                .id("card-dialog-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .on_click(cx.listener(|view, _: &gpui::ClickEvent, _, cx| view.close_dialog(cx)))
                .child(glass::scrim("card-dialog-scrim", cx))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(20.0))
                        .child(sheet.on_click(|_, _, cx| cx.stop_propagation())),
                )
                .into_any_element(),
        )
    }
}

/// Opens Settings, or refreshes and raises it while it is already open. A
/// handle to a window the user closed fails to update, so a new one opens.
pub fn open_settings_window(
    snapshot: SettingsSnapshot,
    existing: Option<WindowHandle<SettingsView>>,
    on_action: impl Fn(SettingsAction, &mut App) + 'static,
    cx: &mut App,
) -> WindowHandle<SettingsView> {
    if let Some(handle) = existing {
        let fresh = snapshot.clone();
        let refreshed = handle.update(cx, |view, window, cx| {
            view.set_snapshot(fresh, cx);
            #[cfg(windows)]
            view.passkeys.update(cx, |passkeys, cx| {
                if passkeys.dialog.is_none() {
                    passkeys.options = passkeys::read();
                    cx.notify();
                }
            });
            window.activate();
        });
        if refreshed.is_ok() {
            return handle;
        }
    }
    let on_action: OnSettingsAction = Rc::new(on_action);
    let height = if cfg!(windows) { 720.0 } else { 640.0 };
    let bounds = Bounds::centered(None, size(px(480.0), px(height)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(
                        if cfg!(target_os = "macos") {
                            "tapbridge Preferences"
                        } else {
                            "tapbridge Settings"
                        }
                        .into(),
                    ),
                    appears_transparent: cfg!(target_os = "macos"),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| {
                    let view = SettingsView::new(snapshot, on_action, cx);
                    view.focus.focus(window, cx);
                    view
                })
            },
        )
        .expect("settings window");
    window.update(cx, |_, window, _| window.activate()).ok();
    window
}

/// Row-major dark modules and the side length in modules.
fn build_qr(text: &str) -> Option<(Vec<bool>, u16)> {
    // QrCode::new picks the smallest version that fits.
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let dimension = code.width() as u16;
    let dark = code
        .to_colors()
        .into_iter()
        .map(|color| color == qrcode::Color::Dark)
        .collect();
    Some((dark, dimension))
}

/// Shows the "New card" dialog in an open Settings window.
pub fn settings_new_card(handle: WindowHandle<SettingsView>, uid: Vec<u8>, cx: &mut App) {
    let _ = handle.update(cx, |view, window, cx| {
        view.new_card(uid, window, cx);
        window.activate();
    });
}

/// Shows a card's "Set up login" dialog in an open Settings window (the
/// phone asked for it), unless another dialog is up.
pub fn settings_open_login(handle: WindowHandle<SettingsView>, uid: Vec<u8>, cx: &mut App) {
    let _ = handle.update(cx, |view, window, cx| {
        if view.dialog.is_none() {
            view.dialog = Some(CardDialog::Settings(uid));
            cx.notify();
        }
        window.activate();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_easing_settles_at_full_size() {
        assert!(ease_out_back(0.0).abs() < 1e-6);
        assert!((ease_out_back(1.0) - 1.0).abs() < 1e-6);
        // Overshoots on the way, never ends small.
        assert!((0..=100).any(|i| ease_out_back(i as f32 / 100.0) > 1.0));
    }

    #[test]
    fn countdown_shows_minutes_and_padded_seconds() {
        assert_eq!(countdown(Duration::from_secs(300)), "Expires in 5:00");
        assert_eq!(countdown(Duration::from_millis(64_900)), "Expires in 1:04");
        assert_eq!(countdown(Duration::ZERO), "Expires in 0:00");
    }

    #[test]
    fn qr_grid_is_square_and_dense() {
        let (dark, dimension) = build_qr("tapbridge:1:abc:def").unwrap();
        assert!(dimension >= 21);
        assert_eq!(dark.len(), dimension as usize * dimension as usize);
        assert!(dark.iter().filter(|dark| **dark).count() > 40);
        // Finder pattern: the top-left corner module is dark.
        assert!(dark[0]);
    }
}
