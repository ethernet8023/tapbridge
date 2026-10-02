//! Embedded tray icons (generated PNGs under assets/icons, decoded to RGBA).

pub struct Icons {
    pub idle: tray_icon::Icon,
    pub phone: tray_icon::Icon,
    pub card: tray_icon::Icon,
    pub error: tray_icon::Icon,
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
    pub fn load() -> Self {
        Self {
            idle: load(include_bytes!("../assets/icons/tray-idle.png")),
            phone: load(include_bytes!("../assets/icons/tray-phone.png")),
            card: load(include_bytes!("../assets/icons/tray-card.png")),
            error: load(include_bytes!("../assets/icons/tray-error.png")),
        }
    }
}
