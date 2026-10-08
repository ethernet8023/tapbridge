//! Generates tray icons, app-size PNGs and the .ico from assets/brand/icon.svg.
//! Development-only; nothing here links into the runtime binary.
use std::{fs, path::Path, path::PathBuf};

const MASTER: &str = include_str!("../../../assets/brand/icon.svg");
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/desktop/assets");

fn main() {
    let out = PathBuf::from(OUT);
    let icons = out.join("icons");
    fs::create_dir_all(&icons).unwrap();
    // Tray states: brand icon plus a state badge in the lower right.
    let variants: &[(&str, Option<(&str, &str)>)] = &[
        ("tray-idle", None),
        ("tray-phone", Some(("#3DDC97", "#17121F"))),
        ("tray-card", Some(("#3DDC97", "#FFFFFF"))),
        ("tray-error", Some(("#FF5C5C", "#17121F"))),
    ];
    let mut ico_sizes: Vec<(u32, Vec<u8>)> = Vec::new();
    for size in [16, 24, 32, 48, 64, 128, 256] {
        let png = render(MASTER, size);
        let name = icons.join(format!("app-{size}.png"));
        fs::write(&name, &png).unwrap();
        ico_sizes.push((size, png.clone()));
        // The 32px brand render doubles as the idle tray icon at higher dpi
        // fidelity; the tray uses dedicated 32px variants below.
    }
    fs::write(out.join("tapbridge.ico"), ico(&ico_sizes)).unwrap();
    for (name, badge) in variants {
        let mut svg = MASTER.to_string();
        if let Some((fill, ring)) = badge {
            svg = svg.replace(
                "</svg>",
                &format!(
                    r#"<circle cx="860" cy="860" r="170" fill="{fill}" stroke="{ring}" stroke-width="60"/></svg>"#
                ),
            );
        }
        for size in [24, 32] {
            let png = render(&svg, size);
            fs::write(icons.join(format!("{name}-{size}.png")), png).unwrap();
        }
        // tray-icon embeds the 32px variant.
        fs::write(icons.join(format!("{name}.png")), render(&svg, 32)).unwrap();
    }
    // macOS menu bar template glyphs: 22pt tall, rendered @2x (44px). tray-icon
    // caps the NSImage at 22pt, so the 44px bitmap draws retina-sharp.
    let menubar = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/macos/menubar");
    for state in ["idle", "phone", "card", "error"] {
        let svg = fs::read_to_string(menubar.join(format!("menubar-{state}.svg"))).unwrap();
        fs::write(icons.join(format!("menubar-{state}.png")), render(&svg, 44)).unwrap();
    }
    println!("icons written to {OUT}");
}

fn render(svg: &str, size: u32) -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    // The SVG viewBox is 1024; scale it to the requested pixel size.
    let scale = size as f32 / tree.size().width();
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().unwrap()
}

/// ICO with PNG-encoded entries (valid for sizes >= 16; standard support).
fn ico(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    // ICONDIR
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len();
    for (size, png) in entries {
        out.push((*size % 256) as u8);
        out.push((*size % 256) as u8);
        out.push(0); // colors
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in entries {
        out.extend_from_slice(png);
    }
    out
}
