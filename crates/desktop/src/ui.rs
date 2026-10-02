//! gpui windows: the pairing QR window and the status window. Only these two
//! surfaces use gpui; the tray and all orchestration live beside them.
use gpui::{
    Animation, AnimationExt, App, Bounds, Context, Window, WindowBounds, WindowHandle,
    WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use std::{rc::Rc, time::Duration};

/// QR quiet zone in modules (ISO/IEC 18004 asks for 4).
const QUIET_ZONE: u16 = 4;
/// How long the "Phone paired" confirmation stays up before the window closes.
const PAIRED_CLOSE_DELAY: Duration = Duration::from_millis(1600);

const PANEL: u32 = 0x17121F;
const PANEL_2: u32 = 0x221A30;
const TEXT: u32 = 0xF2EFFA;
const MUTED: u32 = 0x9D93B5;
const ACCENT: u32 = 0x7C5CFF;
const GOOD: u32 = 0x3DDC97;

pub struct PairingView {
    /// Row-major dark modules, `dimension * dimension` long; empty when the
    /// invitation could not be encoded.
    dark: Rc<[bool]>,
    dimension: u16,
    paired: bool,
}

impl PairingView {
    pub fn new(invitation: &str) -> Self {
        let (dark, dimension) = build_qr(invitation).unwrap_or((Vec::new(), 0));
        Self {
            dark: dark.into(),
            dimension,
            paired: false,
        }
    }
}

impl Render for PairingView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let module = px(7.0);
        let dimension = self.dimension;
        let side = module * (dimension + 2 * QUIET_ZONE) as f32;
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
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(if self.paired { GOOD } else { MUTED }))
                    .child(if self.paired {
                        "Closing..."
                    } else {
                        "Scan with the tapbridge app (valid 5 minutes)"
                    }),
            )
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
            *view = PairingView::new(&text);
            window.activate_window();
            cx.notify();
        });
        if refreshed.is_ok() {
            return handle;
        }
    }
    let invitation = invitation.to_owned();
    let bounds = Bounds::centered(None, size(px(360.0), px(520.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Pair phone".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |_window, cx| cx.new(|_| PairingView::new(&invitation)),
    )
    .expect("pairing window")
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
    fn qr_grid_is_square_and_dense() {
        let (dark, dimension) = build_qr("tapbridge:1:abc:def").unwrap();
        assert!(dimension >= 21);
        assert_eq!(dark.len(), dimension as usize * dimension as usize);
        assert!(dark.iter().filter(|dark| **dark).count() > 40);
        // Finder pattern: the top-left corner module is dark.
        assert!(dark[0]);
    }
}
