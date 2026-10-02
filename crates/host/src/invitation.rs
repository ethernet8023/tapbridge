//! Explicit local pairing display. Never log the capability.
use std::io;
use tapbridge_core::pairing::Invitation;

pub fn page(invitation: &Invitation) -> io::Result<String> {
    let token = invitation.to_string();
    let svg = qrcode::QrCode::new(token.as_bytes())
        .map_err(io::Error::other)?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .build();
    Ok(format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="referrer" content="no-referrer"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'"><title>Pair tapbridge</title><style>body{{font:18px system-ui;max-width:720px;margin:48px auto;padding:20px}}textarea{{width:100%;height:100px}}svg{{display:block;width:320px;height:320px}}</style><h1>Pair your phone</h1><p>Scan this QR in tapbridge, or copy the invitation into the Android app.</p>{svg}<p>One use, expires after 5 minutes. Keep this page private. Keep the host running.</p><textarea readonly aria-label="Pairing invitation">{token}</textarea><p>After pairing, close this page and delete it. The connection is encrypted; NFC reader mode opens only when you open the phone app.</p></html>"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairing_page_contains_qr_and_selectable_invitation() {
        let invitation = Invitation::new(iroh::SecretKey::generate().public());
        let html = page(&invitation).unwrap();
        assert!(html.contains("<svg"));
        assert!(html.contains(&invitation.to_string()));
        assert!(html.contains("5 minutes"));
        assert!(!html.contains("<script"));
    }
}
