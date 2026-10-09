//! Embedded tray icons (generated PNGs under assets/icons, decoded to RGBA).
//!
//! macOS gets monochrome menu bar glyphs drawn as template images (the system
//! tints them for light/dark menu bars and the highlighted state); Windows
//! keeps the full-colour brand icon with a state badge.

use crate::tray::TrayState;

pub struct Icons {
    #[cfg(target_os = "macos")]
    disconnected: tray_icon::Icon,
    #[cfg(target_os = "macos")]
    connected: tray_icon::Icon,
    /// Waiting for a tap: one wave filled at a time, inner to outer.
    #[cfg(target_os = "macos")]
    tap: [tray_icon::Icon; 3],
    #[cfg(not(target_os = "macos"))]
    idle: tray_icon::Icon,
    #[cfg(not(target_os = "macos"))]
    phone: tray_icon::Icon,
    card: tray_icon::Icon,
    error: tray_icon::Icon,
}

fn load(embedded: &[u8]) -> tray_icon::Icon {
    let decoder = png::Decoder::new(std::io::Cursor::new(embedded));
    let mut reader = decoder.read_info().expect("valid embedded icon png");
    let mut data = vec![0u8; reader.output_buffer_size().unwrap_or(0).max(1)];
    let info = reader.next_frame(&mut data).expect("png frame");
    data.truncate(info.buffer_size());
    tray_icon::Icon::from_rgba(data, info.width, info.height).expect("valid icon")
}

impl Icons {
    /// Whether these icons must be drawn as macOS template images.
    pub const TEMPLATE: bool = cfg!(target_os = "macos");

    #[cfg(target_os = "macos")]
    pub fn load() -> Self {
        Self {
            disconnected: load(include_bytes!("../assets/icons/menubar-disconnected.png")),
            connected: load(include_bytes!("../assets/icons/menubar-connected.png")),
            tap: [
                load(include_bytes!("../assets/icons/menubar-tap1.png")),
                load(include_bytes!("../assets/icons/menubar-tap2.png")),
                load(include_bytes!("../assets/icons/menubar-tap3.png")),
            ],
            card: load(include_bytes!("../assets/icons/menubar-card.png")),
            error: load(include_bytes!("../assets/icons/menubar-error.png")),
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn load() -> Self {
        Self {
            idle: load(include_bytes!("../assets/icons/tray-idle.png")),
            phone: load(include_bytes!("../assets/icons/tray-phone.png")),
            card: load(include_bytes!("../assets/icons/tray-card.png")),
            error: load(include_bytes!("../assets/icons/tray-error.png")),
        }
    }

    /// The icon for `state`; `frame` picks the waiting-for-tap animation
    /// frame (macOS only; Windows has no animation).
    pub fn for_state(&self, state: TrayState, frame: usize) -> tray_icon::Icon {
        match state {
            TrayState::Error | TrayState::MissingUsbip => self.error.clone(),
            TrayState::Card => self.card.clone(),
            #[cfg(target_os = "macos")]
            TrayState::Disconnected => self.disconnected.clone(),
            #[cfg(target_os = "macos")]
            TrayState::Connected => self.connected.clone(),
            #[cfg(target_os = "macos")]
            TrayState::Waiting => self.tap[frame % self.tap.len()].clone(),
            #[cfg(not(target_os = "macos"))]
            TrayState::Disconnected => {
                let _ = frame;
                self.idle.clone()
            }
            #[cfg(not(target_os = "macos"))]
            TrayState::Connected | TrayState::Waiting => self.phone.clone(),
        }
    }
}
