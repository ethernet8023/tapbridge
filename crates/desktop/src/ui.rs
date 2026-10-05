//! gpui windows: the pairing QR window and the status window. Only these two
//! surfaces use gpui; the tray and all orchestration live beside them.
use crate::passkeys;
use gpui::{
    Animation, AnimationExt, App, Bounds, Context, Pixels, Task, Window, WindowBounds,
    WindowHandle, WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
use tapbridge_core::pairing::INVITATION_TTL;

/// The pairing window is square; the QR is sized to fit it, not the reverse.
const PAIRING_WINDOW: f32 = 480.0;
/// Target QR side including its quiet zone, in logical pixels.
const QR_TARGET: f32 = 340.0;

/// QR quiet zone in modules (ISO/IEC 18004 asks for 4).
const QUIET_ZONE: u16 = 4;
/// How long the "Phone paired" confirmation stays up before the window closes.
const PAIRED_CLOSE_DELAY: Duration = Duration::from_millis(2800);

const PANEL: u32 = 0x17121F;
const PANEL_2: u32 = 0x221A30;
const TEXT: u32 = 0xF2EFFA;
const ACCENT: u32 = 0x7C5CFF;
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
    /// When the invitation stops working (the host gate uses the same TTL).
    expires: Instant,
    /// Re-renders once a second for the countdown; dropped with the view.
    _ticker: Task<()>,
}

impl PairingView {
    pub fn new(invitation: &str, cx: &mut Context<Self>) -> Self {
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
            expires: Instant::now() + INVITATION_TTL,
            _ticker: ticker,
        }
    }

    /// The invitation ran out and nobody paired: time for a fresh one.
    pub fn expired(&self) -> bool {
        self.paired.is_none() && Instant::now() >= self.expires
    }
}

fn countdown(remaining: Duration) -> String {
    let seconds = remaining.as_secs();
    format!("Expires in {}:{:02}", seconds / 60, seconds % 60)
}

impl Render for PairingView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let dimension = self.dimension;
        // Whole device pixels per module: fractional modules leave seams.
        let scale = window.scale_factor();
        let cells = (dimension + 2 * QUIET_ZONE) as f32;
        let module = px((QR_TARGET * scale / cells).floor().max(1.0) / scale);
        let side = module * cells;
        let stage = side + px(20.0);

        let root = div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap(px(18.0))
            .bg(rgb(PANEL))
            .text_color(rgb(TEXT));

        if let Some(phone) = &self.paired {
            return root
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(GOOD))
                        .child("Pairing successful"),
                )
                .child(
                    div()
                        .size(stage)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(success_badge(150.0)),
                )
                .child(
                    div()
                        .text_size(px(22.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(format!("{phone} is ready")),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(ACCENT_SOFT))
                        .child("You can sign in with your key from this PC now."),
                );
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
        let card = div()
            .relative()
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
        root.child(
            div()
                .text_size(px(18.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child("Pair your phone"),
        )
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
    existing: Option<WindowHandle<PairingView>>,
    cx: &mut App,
) -> WindowHandle<PairingView> {
    if let Some(handle) = existing {
        let text = invitation.to_owned();
        let refreshed = handle.update(cx, |view, window, cx| {
            *view = PairingView::new(&text, cx);
            window.activate_window();
            cx.notify();
        });
        if refreshed.is_ok() {
            return handle;
        }
    }
    let invitation = invitation.to_owned();
    let bounds = Bounds::centered(None, size(px(PAIRING_WINDOW), px(PAIRING_WINDOW)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Pair phone".into()),
                ..Default::default()
            }),
            is_resizable: false,
            ..Default::default()
        },
        move |_window, cx| cx.new(|cx| PairingView::new(&invitation, cx)),
    )
    .expect("pairing window")
}

/// `Some(true)` when the open pairing window's invitation ran out; `None`
/// when the window is closed.
pub fn pairing_expired(handle: WindowHandle<PairingView>, cx: &mut App) -> Option<bool> {
    handle.update(cx, |view, _, _| view.expired()).ok()
}

/// Shows the pairing-successful screen, then closes the window.
pub fn complete_pairing(handle: WindowHandle<PairingView>, phone: String, cx: &mut App) {
    let shown = handle.update(cx, |view, _window, cx| {
        view.paired = Some(phone);
        cx.notify();
    });
    if shown.is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(PAIRED_CLOSE_DELAY).await;
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    })
    .detach();
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
pub struct PasskeyView {
    options: passkeys::Options,
    /// In-window dialog over the options while a change runs or reports.
    dialog: Option<PasskeyDialog>,
}

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
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(14.0))
        .p(px(14.0))
        .rounded(px(12.0))
        .bg(rgb(PANEL_2))
        .when(enabled, |row| {
            row.cursor_pointer().hover(|style| style.bg(rgb(0x2C2340)))
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
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(ACCENT_SOFT))
                        .child(detail),
                ),
        )
}

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
            .p(px(22.0))
            .size_full()
            .child(
                div()
                    .text_size(px(20.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Passkey options"),
            )
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
            ))
            .child(
                option_row(
                    "opt-hello",
                    options.windows_hello,
                    idle,
                    "This Windows device",
                    "Passkeys saved with Windows Hello on this PC.".to_owned(),
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
                )
                .when(!options.providers.is_empty(), |row| {
                    row.on_click(|_, _, _| passkeys::open_windows_passkey_settings())
                }),
            );
        div()
            .relative()
            .size_full()
            .bg(rgb(PANEL))
            .text_color(rgb(TEXT))
            .child(column)
            .when_some(self.dialog.as_ref(), |root, dialog| {
                root.child(self.render_dialog(dialog, cx))
            })
    }
}

pub fn open_passkey_window(
    existing: Option<WindowHandle<PasskeyView>>,
    cx: &mut App,
) -> WindowHandle<PasskeyView> {
    if let Some(handle) = existing {
        let refreshed = handle.update(cx, |view, window, cx| {
            if view.dialog.is_none() {
                view.options = passkeys::read();
            }
            window.activate_window();
            cx.notify();
        });
        if refreshed.is_ok() {
            return handle;
        }
    }
    let bounds = Bounds::centered(None, size(px(520.0), px(540.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Passkey options".into()),
                ..Default::default()
            }),
            is_resizable: false,
            ..Default::default()
        },
        |_, cx| cx.new(|_| PasskeyView::new()),
    )
    .expect("passkey options window")
}

pub struct StatusView {
    lines: Vec<String>,
}

impl StatusView {
    pub fn new(lines: Vec<String>) -> Self {
        Self { lines }
    }
}

impl Render for StatusView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .size_full()
            .gap(px(6.0))
            .p(px(18.0))
            .bg(rgb(PANEL_2))
            .text_color(rgb(TEXT))
            .text_size(px(13.0));
        for line in &self.lines {
            column = column.child(div().child(line.clone()));
        }
        div().size_full().bg(rgb(PANEL_2)).child(column)
    }
}

pub fn open_status_window(
    lines: Vec<String>,
    existing: Option<WindowHandle<StatusView>>,
    cx: &mut App,
) -> WindowHandle<StatusView> {
    if let Some(handle) = existing {
        let fresh = lines.clone();
        let refreshed = handle.update(cx, |view, window, cx| {
            view.lines = fresh;
            window.activate_window();
            cx.notify();
        });
        if refreshed.is_ok() {
            return handle;
        }
    }
    let bounds = Bounds::centered(None, size(px(420.0), px(260.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("tapbridge".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |_window, cx| cx.new(|_| StatusView::new(lines)),
    )
    .expect("status window")
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
