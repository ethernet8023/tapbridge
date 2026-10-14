//! Tray icon + menu via gpui-tray: one implementation for macOS, Windows and
//! Linux (StatusNotifierItem over D-Bus, no GTK). The tray is created on the
//! gpui main thread and runs no event loop of its own; menu clicks dispatch
//! ordinary gpui actions (`OpenSettings`, `OpenAbout`, `Quit`, `PairPhone`).
//!
//! The menu stays small: a status line, About, Settings… and Quit. Everything
//! configurable lives in the Settings window.
use crate::{
    app_state::{OpenAbout, OpenSettings, PairPhone, Quit},
    assets,
};
use gpui::{App, MenuItem, NoAction};
use std::{cell::RefCell, rc::Rc};

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

const ABOUT_TITLE: &str = "About tapbridge";
const QUIT_TITLE: &str = "Quit tapbridge";

/// macOS keeps the classic "Preferences…" wording; elsewhere it says "Settings…".
const SETTINGS_TITLE: &str = if cfg!(target_os = "macos") {
    "Preferences\u{2026}"
} else {
    "Settings\u{2026}"
};

/// What the menu shows. The menu is rebuilt from this on every change, so it
/// lives in a cell the (`'static`) builder closure shares with [`Tray`].
struct MenuState {
    /// The status line.
    text: String,
    /// Whether the status line is a clickable "pair" action right now.
    pair_action: bool,
}

fn build_menu(state: &MenuState) -> Vec<MenuItem> {
    let status = if state.pair_action {
        MenuItem::action(state.text.clone(), PairPhone)
    } else {
        // A plain, disabled label.
        MenuItem::action(state.text.clone(), NoAction).disabled(true)
    };
    vec![
        status,
        MenuItem::separator(),
        MenuItem::action(ABOUT_TITLE, OpenAbout),
        MenuItem::action(SETTINGS_TITLE, OpenSettings),
        MenuItem::separator(),
        MenuItem::action(QUIT_TITLE, Quit),
    ]
}

pub struct Tray {
    native: gpui_tray::Tray,
    menu: Rc<RefCell<MenuState>>,
    icons: assets::Icons,
    /// What is drawn now: icon swaps only happen on a change, so the
    /// animation tick costs nothing between frames.
    shown: Option<(TrayState, usize)>,
    /// Tooltip as last set.
    tooltip: String,
}

impl Tray {
    pub fn new(icons: assets::Icons, cx: &mut App) -> Result<Self, String> {
        let menu = Rc::new(RefCell::new(MenuState {
            text: "tapbridge starting".to_owned(),
            pair_action: false,
        }));
        let builder_state = menu.clone();
        let builder = gpui_tray::Tray::builder();
        // macOS menus carry a glyph beside Settings and About, as SF Symbols do.
        #[cfg(target_os = "macos")]
        let builder = builder
            .menu_item_image(SETTINGS_TITLE, &include_bytes!("../assets/ui/gear.svg")[..])
            .menu_item_image(ABOUT_TITLE, &include_bytes!("../assets/ui/info.svg")[..]);
        let native = builder
            .icon(icons.for_state(TrayState::Disconnected, 0))
            .title("tapbridge")
            .tooltip("tapbridge")
            .menu(move |_| build_menu(&builder_state.borrow()))
            .build(cx)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            native,
            menu,
            icons,
            shown: Some((TrayState::Disconnected, 0)),
            tooltip: "tapbridge".to_owned(),
        })
    }

    /// Shows `text` and the icon for `state`; `frame` is the waiting-for-tap
    /// animation frame (ignored for every other state). With `pair_action`
    /// the status line is enabled and clicking it starts pairing; otherwise
    /// it is a plain, disabled label.
    pub fn set_status(
        &mut self,
        text: &str,
        state: TrayState,
        frame: usize,
        pair_action: bool,
        cx: &mut App,
    ) {
        let frame = if state == TrayState::Waiting {
            frame
        } else {
            0
        };
        if self.shown != Some((state, frame)) {
            self.shown = Some((state, frame));
            let _ = self
                .native
                .set_icon(Some(self.icons.for_state(state, frame)), cx);
        }
        let tooltip = if state == TrayState::MissingUsbip {
            "Install USB/IP driver"
        } else {
            text
        };
        if self.tooltip != tooltip {
            let _ = self.native.set_tooltip(Some(tooltip.to_owned()), cx);
            self.tooltip = tooltip.to_owned();
        }
        let changed = {
            let mut menu = self.menu.borrow_mut();
            let changed = menu.text != text || menu.pair_action != pair_action;
            if changed {
                menu.text = text.to_owned();
                menu.pair_action = pair_action;
            }
            changed
        };
        if changed {
            let _ = self.native.refresh_menu(cx);
        }
    }
}
