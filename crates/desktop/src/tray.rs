//! Tray icon + menu via tray-icon. The tray is created on the gpui main
//! thread so its hidden window is pumped by gpui's message loop.
use crate::assets;
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

pub enum MenuAction {
    RequestTap,
    Pair,
    Forget,
    OpenStatus,
    PasskeyOptions,
    ToggleAutostart(bool),
    Quit,
}

/// Tray visual states, in priority order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Error,
    Card,
    Phone,
    Idle,
    MissingUsbip,
}

pub struct Tray {
    tray: TrayIcon,
    status_item: MenuItem,
    autostart_item: CheckMenuItem,
    icons: assets::Icons,
}

impl Tray {
    pub fn new(icons: assets::Icons) -> Result<Self, String> {
        let status_item = MenuItem::with_id("status", "tapbridge starting", false, None);
        let statuswin_item = MenuItem::with_id("statuswin", "Open status", true, None);
        let request_item = MenuItem::with_id("request", "Request tap", true, None);
        let pair_item = MenuItem::with_id("pair", "Pair phone...", true, None);
        let forget_item = MenuItem::with_id("forget", "Forget phone", true, None);
        let passkeys_item = MenuItem::with_id("passkeys", "Passkey options...", true, None);
        let autostart_item =
            CheckMenuItem::with_id("autostart", "Start at login", true, false, None);
        let quit_item = MenuItem::with_id("quit", "Quit", true, None);
        let separator = PredefinedMenuItem::separator();
        let separator2 = PredefinedMenuItem::separator();
        let separator3 = PredefinedMenuItem::separator();
        let menu = Menu::new();
        menu.append_items(&[
            &status_item,
            &statuswin_item,
            &separator,
            &request_item,
            &pair_item,
            &forget_item,
            &passkeys_item,
            &separator2,
            &autostart_item,
            &separator3,
            &quit_item,
        ])
        .map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("tapbridge")
            .with_icon(icons.idle.clone())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            tray,
            status_item,
            autostart_item,
            icons,
        })
    }

    pub fn set_status(&mut self, text: &str, state: TrayState) {
        self.status_item.set_text(text);
        let icon = match state {
            TrayState::Error | TrayState::MissingUsbip => self.icons.error.clone(),
            TrayState::Card => self.icons.card.clone(),
            TrayState::Phone => self.icons.phone.clone(),
            TrayState::Idle => self.icons.idle.clone(),
        };
        let _ = self.tray.set_icon(Some(icon));
        if state == TrayState::MissingUsbip {
            let _ = self
                .tray
                .set_tooltip(Some("Install USB/IP driver".to_owned()));
        } else {
            let _ = self.tray.set_tooltip(Some(text.to_owned()));
        }
    }

    pub fn set_autostart_checked(&mut self, checked: bool) {
        self.autostart_item.set_checked(checked);
    }

    /// Drains pending menu actions (call from the UI tick).
    pub fn actions(&self) -> Vec<MenuAction> {
        let receiver = MenuEvent::receiver();
        let mut actions = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            let id = event.id().0.clone();
            actions.push(match id.as_str() {
                "request" => MenuAction::RequestTap,
                "pair" => MenuAction::Pair,
                "forget" => MenuAction::Forget,
                "statuswin" => MenuAction::OpenStatus,
                "passkeys" => MenuAction::PasskeyOptions,
                // The check item flips itself on click; its new state is the request.
                "autostart" => MenuAction::ToggleAutostart(self.autostart_item.is_checked()),
                "quit" => MenuAction::Quit,
                _ => continue,
            });
        }
        actions
    }
}
