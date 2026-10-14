//! Tray icon + menu via tray-icon. The tray is created on the gpui main
//! thread so its hidden window is pumped by gpui's message loop.
//!
//! The menu stays small: a status line, About, Settings… and Quit. Everything
//! configurable lives in the Settings window.
use crate::assets;
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{
        IconMenuItem, Menu, MenuEvent, MenuItem, NativeIcon, PredefinedMenuItem,
        accelerator::{Accelerator, Code, Modifiers},
    },
};

pub enum MenuAction {
    /// The status line, clicked while it offers pairing ("Pair a phone").
    Pair,
    OpenSettings,
    About,
    Quit,
}

/// Tray visual states, in priority order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayState {
    Error,
    /// A card is on the phone.
    Card,
    /// A tap was requested and no card is present yet (animated on macOS).
    Waiting,
    /// The phone session is live.
    Connected,
    /// No phone connected (paired or not).
    Disconnected,
    MissingUsbip,
}

/// Menu titles. macOS matches the item by title to swap in an SF Symbol.
const ABOUT_TITLE: &str = "About tapbridge";

/// macOS keeps the classic "Preferences…" wording; Windows says "Settings…".
const SETTINGS_TITLE: &str = if cfg!(target_os = "macos") {
    "Preferences\u{2026}"
} else {
    "Settings\u{2026}"
};
const QUIT_TITLE: &str = "Quit tapbridge";

/// ⌘ on macOS, Ctrl elsewhere: the platform's command modifier.
const COMMAND: Modifiers = if cfg!(target_os = "macos") {
    Modifiers::META
} else {
    Modifiers::CONTROL
};

/// Linux: the tray and its menu are GTK objects, and gpui does not run a GTK
/// main loop, so the UI tick runs the pending GTK events (the menu's D-Bus
/// traffic, clicks) itself. A no-op elsewhere.
pub fn pump_gtk() {
    #[cfg(target_os = "linux")]
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

pub struct Tray {
    tray: TrayIcon,
    status_item: MenuItem,
    icons: assets::Icons,
    /// What is drawn now: icon swaps (and the template flag) only happen on
    /// a change, so the animation tick costs nothing between frames.
    shown: Option<(TrayState, usize)>,
    /// Status line and tooltip as last set.
    text: (String, String),
    /// Whether the status line is a clickable "pair" action right now.
    pair_action: bool,
}

impl Tray {
    pub fn new(icons: assets::Icons) -> Result<Self, String> {
        // tray-icon's menu is made of GTK widgets, which need GTK initialized.
        #[cfg(target_os = "linux")]
        gtk::init().map_err(|error| format!("gtk init: {error}"))?;
        let status_item = MenuItem::with_id("status", "tapbridge starting", false, None);
        // macOS gets an SF Symbol gear below; the native icon is the Windows
        // Settings stock icon (and the AppKit fallback).
        let settings_item = IconMenuItem::with_id_and_native_icon(
            "settings",
            SETTINGS_TITLE,
            true,
            Some(NativeIcon::PreferencesGeneral),
            Some(Accelerator::new(COMMAND, Code::Comma)),
        );
        let about_item = MenuItem::with_id("about", ABOUT_TITLE, true, None);
        let quit_item = MenuItem::with_id(
            "quit",
            QUIT_TITLE,
            true,
            Some(Accelerator::new(COMMAND, Code::KeyQ)),
        );
        let menu = Menu::new();
        menu.append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &about_item,
            &settings_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])
        .map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        {
            use_svg_icon(
                &menu,
                SETTINGS_TITLE,
                include_bytes!("../assets/ui/gear.svg"),
            );
            use_svg_icon(&menu, ABOUT_TITLE, include_bytes!("../assets/ui/info.svg"));
        }
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("tapbridge");
        // macOS: template icons, recoloured by the system for the menu bar.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icons.for_state(TrayState::Disconnected, 0));
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icons.for_state(TrayState::Disconnected, 0));
        let tray = builder.build().map_err(|e| e.to_string())?;
        Ok(Self {
            tray,
            status_item,
            icons,
            shown: Some((TrayState::Disconnected, 0)),
            text: ("tapbridge starting".to_owned(), "tapbridge".to_owned()),
            pair_action: false,
        })
    }

    /// Shows `text` and the icon for `state`; `frame` is the waiting-for-tap
    /// animation frame (ignored for every other state). With `pair_action`
    /// the status line is enabled and clicking it starts pairing; otherwise
    /// it is a plain, disabled label.
    pub fn set_status(&mut self, text: &str, state: TrayState, frame: usize, pair_action: bool) {
        if self.pair_action != pair_action {
            self.status_item.set_enabled(pair_action);
            self.pair_action = pair_action;
        }
        let frame = if state == TrayState::Waiting {
            frame
        } else {
            0
        };
        if self.shown != Some((state, frame)) {
            self.shown = Some((state, frame));
            let icon = self.icons.for_state(state, frame);
            #[cfg(target_os = "macos")]
            let _ = self.tray.set_icon_templated(Some(icon));
            #[cfg(not(target_os = "macos"))]
            let _ = self.tray.set_icon(Some(icon));
        }
        let tooltip = if state == TrayState::MissingUsbip {
            "Install USB/IP driver"
        } else {
            text
        };
        if self.text.0 != text {
            self.status_item.set_text(text);
            self.text.0 = text.to_owned();
        }
        if self.text.1 != tooltip {
            let _ = self.tray.set_tooltip(Some(tooltip.to_owned()));
            self.text.1 = tooltip.to_owned();
        }
    }

    /// Drains pending menu actions (call from the UI tick).
    pub fn actions(&self) -> Vec<MenuAction> {
        let receiver = MenuEvent::receiver();
        let mut actions = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            actions.push(match event.id().0.as_str() {
                "status" if self.pair_action => MenuAction::Pair,
                "settings" => MenuAction::OpenSettings,
                "about" => MenuAction::About,
                "quit" => MenuAction::Quit,
                _ => continue,
            });
        }
        actions
    }
}

/// Replaces a menu item's image with one of our SVG glyphs (SF Symbols
/// exports), as a template so it follows the menu's light/dark appearance.
/// NSImage reads SVG itself (macOS 13+).
#[cfg(target_os = "macos")]
fn use_svg_icon(menu: &Menu, title: &str, svg: &[u8]) {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSImage, NSMenu};
    use objc2_foundation::{NSData, NSSize, NSString};
    use tray_icon::menu::ContextMenu;
    // SAFETY: the NSMenu lives as long as `menu` (and the tray retains it);
    // the tray is built on the main thread, where AppKit calls belong.
    let ns_menu = unsafe { &*(menu.ns_menu() as *const NSMenu) };
    let Some(item) = ns_menu.itemWithTitle(&NSString::from_str(title)) else {
        return;
    };
    let data = NSData::with_bytes(svg);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    // About the size of an SF Symbol at the menu font size (these SVG
    // exports are tight to the glyph); keep the aspect ratio.
    let natural = image.size();
    let scale = 13.0 / natural.width.max(natural.height).max(1.0);
    image.setSize(NSSize::new(natural.width * scale, natural.height * scale));
    image.setTemplate(true);
    item.setImage(Some(&image));
}
