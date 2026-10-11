//! "About tapbridge": icon, name, version and copyright, laid out like the
//! standard macOS About panel, with the diagnostic lines (host, reader,
//! relay, card...) behind a "Details" disclosure. Opened from the app menu
//! and the tray menu.

use crate::glass;
use gpui::{
    App, Context, Image, ImageFormat, Window, WindowBounds, WindowHandle, WindowOptions, div, img,
    prelude::*, px, rgb, size,
};
use std::sync::Arc;

const TEXT: u32 = 0xF4F1FF;
const SOFT: u32 = 0xA79FBA;
const WIDTH: f32 = 300.0;
const HEIGHT: f32 = 300.0;
/// Height added while Details is open.
const DETAILS_HEIGHT: f32 = 200.0;
const TITLEBAR_INSET: f32 = if cfg!(target_os = "macos") { 28.0 } else { 0.0 };

pub struct AboutView {
    details: Vec<String>,
    expanded: bool,
    icon: Arc<Image>,
}

impl AboutView {
    pub fn set_details(&mut self, details: Vec<String>, cx: &mut Context<Self>) {
        if self.details != details {
            self.details = details;
            cx.notify();
        }
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }
}

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = self.expanded;
        let mut column = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.0))
            .pt(px(TITLEBAR_INSET + 8.0))
            .px(px(20.0))
            .pb(px(16.0))
            .child(img(self.icon.clone()).size(px(96.0)))
            .child(div().h(px(8.0)))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("tapbridge"),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(SOFT))
                    .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
            )
            .child(div().h(px(10.0)))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(SOFT))
                    .child("Your phone is this computer's card reader."),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(SOFT))
                    .child("\u{a9} 2026 ethernet. MIT licensed."),
            )
            .child(div().h(px(10.0)))
            .child(
                div()
                    .id("about-details-toggle")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT))
                    .child(if expanded { "\u{25BE}" } else { "\u{25B8}" })
                    .child("Details")
                    .on_click(cx.listener(|view, _: &gpui::ClickEvent, window, cx| {
                        view.expanded = !view.expanded;
                        let height = HEIGHT + if view.expanded { DETAILS_HEIGHT } else { 0.0 };
                        window.resize(size(px(WIDTH), px(height)));
                        cx.notify();
                    })),
            );
        if expanded {
            let mut lines = div()
                .id("about-details")
                .w_full()
                .h(px(DETAILS_HEIGHT - 16.0))
                .mt(px(8.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(4.0));
            for line in &self.details {
                lines = lines.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(SOFT))
                        .child(line.clone()),
                );
            }
            column = column.child(
                glass::panel("about-details", 10.0, cx)
                    .w_full()
                    .p(px(10.0))
                    .child(lines),
            );
        }
        div()
            .relative()
            .size_full()
            .text_color(rgb(TEXT))
            .child(glass::backdrop())
            .child(div().absolute().inset_0().child(column))
    }
}

/// Opens About, or raises it (with fresh details) while it is open. A handle
/// to a window the user closed fails to update, so a new one opens.
pub fn open_about_window(
    details: Vec<String>,
    existing: Option<WindowHandle<AboutView>>,
    cx: &mut App,
) -> WindowHandle<AboutView> {
    if let Some(handle) = existing {
        let fresh = details.clone();
        if handle
            .update(cx, |view, window, cx| {
                view.set_details(fresh, cx);
                window.activate();
            })
            .is_ok()
        {
            return handle;
        }
    }
    let bounds = gpui::Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let icon = Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../assets/icons/app-256.png").to_vec(),
    ));
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("About tapbridge".into()),
                    appears_transparent: cfg!(target_os = "macos"),
                    ..Default::default()
                }),
                is_resizable: false,
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|_| AboutView {
                    details,
                    expanded: false,
                    icon,
                })
            },
        )
        .expect("about window");
    window.update(cx, |_, window, _| window.activate()).ok();
    window
}
