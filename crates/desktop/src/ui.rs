//! gpui windows: the pairing QR window and the status window. Only these two
//! surfaces use gpui; the tray and all orchestration live beside them.
use gpui::{
    Animation, AnimationExt, App, Bounds, Context, Task, Window, WindowBounds, WindowHandle,
    WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
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
const PAIRED_CLOSE_DELAY: Duration = Duration::from_millis(1600);

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
    paired: bool,
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
            paired: false,
            expires: Instant::now() + INVITATION_TTL,
            _ticker: ticker,
        }
    }

    /// The invitation ran out and nobody paired: time for a fresh one.
    pub fn expired(&self) -> bool {
        !self.paired && Instant::now() >= self.expires
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
        // One canvas paints every dark module: the QR costs one element per
        // frame while the scan line animates, not one div per module.
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
        let ring_color = if self.paired { GOOD } else { ACCENT };
        let ring = div()
            .absolute()
            .inset_0()
            .rounded(px(20.0))
            .border_2()
            .border_color(rgb(ring_color));
        let ring = if self.paired {
            ring.into_any_element()
        } else {
            ring.with_animation(
                "pairing-ring",
                Animation::new(Duration::from_millis(2200)).repeat(),
                |ring, delta| {
                    let wave = 0.5 - 0.5 * (std::f32::consts::TAU * delta).cos();
                    ring.opacity(0.25 + 0.75 * wave)
                },
            )
            .into_any_element()
        };
        let card = div()
            .relative()
            .size(side + px(20.0))
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

        let (heading, heading_color, glyph) = if self.paired {
            ("Phone paired", GOOD, "✓")
        } else {
            ("Pair your phone", TEXT, "")
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap(px(18.0))
            .bg(rgb(PANEL))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .text_size(px(18.0))
                    .text_color(rgb(heading_color))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(glyph)
                    .child(heading),
            )
            .child(card)
            .child({
                let remaining = self.expires.saturating_duration_since(Instant::now());
                let color = if self.paired {
                    GOOD
                } else if remaining < EXPIRY_WARNING {
                    WARN
                } else {
                    ACCENT_SOFT
                };
                div()
                    .text_size(px(26.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(color))
                    .child(if self.paired {
                        "Closing...".to_owned()
                    } else {
                        countdown(remaining)
                    })
            })
    }
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

/// Shows the paired confirmation, then closes the window.
pub fn complete_pairing(handle: WindowHandle<PairingView>, cx: &mut App) {
    let shown = handle.update(cx, |view, _window, cx| {
        view.paired = true;
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
