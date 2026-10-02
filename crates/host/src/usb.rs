//! Pure USB descriptors and EP0 control-request handler (USB 2.0 ch9,
//! CCID 1.1 §4/§5.3). No I/O; the USB/IP layer routes control transfers here.

pub const VID: u16 = 0x1209;
pub const PID: u16 = 0x7A71;

pub fn id_vendor() -> u16 {
    VID
}

pub fn id_product() -> u16 {
    PID
}

/// 18-byte device descriptor.
pub fn device_descriptor() -> Vec<u8> {
    let mut d = Vec::with_capacity(18);
    d.extend_from_slice(&[0x12, 0x01]); // bLength, DEVICE
    d.extend_from_slice(&0x0110u16.to_le_bytes()); // bcdUSB 1.1
    d.push(0x00); // class per-interface
    d.push(0x00);
    d.push(0x00);
    d.push(0x40); // max packet 64
    d.extend_from_slice(&VID.to_le_bytes());
    d.extend_from_slice(&PID.to_le_bytes());
    d.extend_from_slice(&0x0100u16.to_le_bytes()); // bcdDevice
    d.push(1); // manufacturer string
    d.push(2); // product string
    d.push(0); // serial: none (avoid ambiguity)
    d.push(1); // num configurations
    d
}

fn endpoint_descriptor(addr: u8, attrs: u8, max_packet: u16, interval: u8) -> Vec<u8> {
    let mut e = vec![0x07, 0x05, addr, attrs];
    e.extend_from_slice(&max_packet.to_le_bytes());
    e.push(interval);
    e
}

/// CCID class descriptor body (54 bytes) matching our capabilities:
/// short-APDU exchange, automatic ICC clock/parameter features 0x000200FE,
/// T=1 only, dwMaxCCIDMessageLength 271, 1 busy slot, 1 slot.
fn ccid_class_descriptor() -> Vec<u8> {
    let mut c = vec![54, 0x21, 0x10, 0x01, 0, 1];
    c.extend(2u32.to_le_bytes()); // T=1
    c.extend(4000u32.to_le_bytes()); // default clock kHz
    c.extend(4000u32.to_le_bytes()); // maximum clock
    c.push(1); // one supported clock
    c.extend(10752u32.to_le_bytes());
    c.extend(10752u32.to_le_bytes());
    c.push(1); // one supported data rate
    c.extend(254u32.to_le_bytes()); // IFSD
    c.extend(0u32.to_le_bytes()); // synchronous protocols
    c.extend(0u32.to_le_bytes()); // mechanical features
    c.extend(0x0002_00FEu32.to_le_bytes()); // automatic parameters, short APDU
    c.extend(271u32.to_le_bytes());
    c.extend([0xff, 0xff, 0, 0, 0, 1]);
    c
}

/// Full configuration descriptor (config + interface + CCID class + endpoints).
pub fn config_descriptor() -> Vec<u8> {
    let mut cfg = vec![0x09, 0x02];
    // total length placeholder
    let total_pos = cfg.len();
    cfg.extend_from_slice(&0u16.to_le_bytes());
    cfg.extend_from_slice(&[0x01, 0x01, 0x00, 0x80, 50]); // 1 iface, cfg 1, bmAttr bus-powered, 50mA
    let mut iface = vec![0x09, 0x04, 0x00, 0x00];
    iface.extend_from_slice(&[3, 0x0B, 0x00, 0x00, 0x00]); // 3 endpoints, smart-card class
    let mut body = cfg;
    body.extend_from_slice(&iface);
    body.extend_from_slice(&ccid_class_descriptor());
    body.extend_from_slice(&endpoint_descriptor(0x01, 0x02, 64, 0)); // bulk OUT ep1
    body.extend_from_slice(&endpoint_descriptor(0x82, 0x02, 64, 0)); // bulk IN ep2
    body.extend_from_slice(&endpoint_descriptor(0x83, 0x03, 8, 255)); // interrupt IN ep3
    let total = body.len() as u16;
    body[total_pos..total_pos + 2].copy_from_slice(&total.to_le_bytes());
    body
}

/// True if `desc` contains an endpoint descriptor with the given address/attrs.
pub fn has_endpoint(desc: &[u8], addr: u8, attrs: u8) -> bool {
    let mut i = 0;
    while i + 1 < desc.len() {
        let len = desc[i] as usize;
        if len >= 7
            && desc[i + 1] == 0x05
            && i + len <= desc.len()
            && desc[i + 2] == addr
            && desc[i + 3] == attrs
        {
            return true;
        }
        if len == 0 {
            break;
        }
        i += len;
    }
    false
}

/// Find the first descriptor with the given type; returns its byte slice.
pub fn find_sub(desc: &[u8], kind: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i + 1 < desc.len() {
        let len = desc[i] as usize;
        if len == 0 {
            return None;
        }
        if desc[i + 1] == kind && i + len <= desc.len() {
            return Some(&desc[i..i + len]);
        }
        i += len;
    }
    None
}

/// String descriptors. Index 0 = LANGID; 1 = manufacturer; 2 = product.
pub fn string_descriptor(index: u8) -> Option<Vec<u8>> {
    let s: &[u8] = match index {
        0 => return Some(vec![0x04, 0x03, 0x09, 0x04]), // English US
        1 => b"tapbridge",
        2 => b"tapbridge CCID reader",
        _ => return None,
    };
    let mut d = vec![(2 + 2 * s.len()) as u8, 0x03];
    for &c in s {
        d.push(c);
        d.push(0);
    }
    Some(d)
}

/// Result of an EP0 control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlReply {
    /// Data stage payload (already truncated to wLength by caller if needed).
    Data(Vec<u8>),
    /// Zero-length success.
    Ok,
    /// Request error: STALL.
    Stall,
    /// CCID class ABORT control request (§5.3.1): wValue low byte bSlot,
    /// high byte bSeq. The reader completes the abort handshake when the
    /// matching PC_to_RDR_Abort bulk-OUT arrives.
    Abort { slot: u8, seq: u8 },
}

/// Handle one setup packet. `setup` is the 8-byte SETUP.
pub fn handle_control(setup: &[u8; 8]) -> ControlReply {
    let bm_request_type = setup[0];
    let b_request = setup[1];
    let w_value = u16::from_le_bytes([setup[2], setup[3]]);
    let w_length = u16::from_le_bytes([setup[6], setup[7]]);
    let dir_in = bm_request_type & 0x80 != 0;
    let kind = bm_request_type & 0x60; // 0=standard, 1=class, 2=vendor
    let recipient = bm_request_type & 0x1F;
    match (kind, recipient, b_request) {
        // Standard: GET_DESCRIPTOR
        (0x00, 0x00, 0x06) if dir_in => {
            let (dtype, index) = ((w_value >> 8) as u8, (w_value & 0xFF) as u8);
            let data = match dtype {
                0x01 => device_descriptor(),
                0x02 => config_descriptor(),
                0x03 => match string_descriptor(index) {
                    Some(s) => s,
                    None => return ControlReply::Stall,
                },
                _ => return ControlReply::Stall,
            };
            ControlReply::Data(data.into_iter().take(w_length as usize).collect())
        }
        // Standard: SET_ADDRESS is handled by USB/IP stack; accept.
        (0x00, 0x00, 0x05) => ControlReply::Ok,
        // Standard: SET_CONFIGURATION
        (0x00, 0x00, 0x09) => ControlReply::Ok,
        // Standard: GET_STATUS (device)
        (0x00, 0x00, 0x00) if dir_in => ControlReply::Data(vec![0x00, 0x00]),
        // Standard: GET_CONFIGURATION
        (0x00, 0x00, 0x08) if dir_in => ControlReply::Data(vec![0x01]),
        // Standard: GET_INTERFACE / SET_INTERFACE on interface 0
        (0x00, 0x01, 0x0A) if dir_in => ControlReply::Data(vec![0x00]),
        (0x00, 0x01, 0x0B) if !dir_in => ControlReply::Ok,
        (0x00, 0x02, 0x01) if !dir_in => ControlReply::Ok,
        (0x00, 0x01 | 0x02, 0x00) if dir_in => ControlReply::Data(vec![0, 0]),
        // Class-specific to interface: CCID ABORT (§5.3.1)
        (0x20, 0x01, 0x01) => ControlReply::Abort {
            slot: (w_value & 0xFF) as u8,
            seq: (w_value >> 8) as u8,
        },
        // Class-specific GET_CLOCK_FREQUENCIES (§5.3.2)
        (0xA0 | 0x20, 0x01, 0x02) if dir_in => ControlReply::Data(4000u32.to_le_bytes().to_vec()),
        // GET_DATA_RATES (§5.3.3)
        (0xA0 | 0x20, 0x01, 0x03) if dir_in => ControlReply::Data(10752u32.to_le_bytes().to_vec()),
        _ => ControlReply::Stall,
    }
}
