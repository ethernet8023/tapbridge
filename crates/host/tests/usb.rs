// TDD RED: usb descriptor tests (slice 2). Derived from USB 2.0 ch9 and
// CCID 1.1 §4/§5; descriptors must match our real capabilities.
use tapbridge_host::usb::*;

#[test]
fn device_descriptor_identity() {
    let d = device_descriptor();
    assert_eq!(d[0], 0x12); // bLength
    assert_eq!(d[1], 0x01); // DEVICE
    assert_eq!(id_vendor(), 0x1209);
    assert_eq!(id_product(), 0x7A71);
    let idv = u16::from_le_bytes([d[8], d[9]]);
    assert_eq!(idv, 0x1209);
    let idp = u16::from_le_bytes([d[10], d[11]]);
    assert_eq!(idp, 0x7A71);
    assert_eq!(d[4], 0x00); // class at interface level
    assert_eq!(d[17], 1); // num configurations
    assert_eq!(d[0], 0x12); // total len 18
}

#[test]
fn config_has_ccid_class_descriptor_matching_capabilities() {
    let c = config_descriptor();
    assert_eq!(c[0], 9);
    assert_eq!(c[1], 0x02);
    let total = u16::from_le_bytes([c[2], c[3]]);
    assert_eq!(total as usize, c.len());
    // Interface class 0x0B (Smart Card), one alt setting.
    assert_eq!(c[9], 9);
    assert_eq!(c[10], 0x04);
    assert_eq!(c[14], 0x0B);
    // Endpoints: 1 OUT (0x01), 2 bulk IN (0x82), 3 interrupt IN (0x83).
    assert!(has_endpoint(&c, 0x01, 0x02), "bulk OUT ep1");
    assert!(has_endpoint(&c, 0x82, 0x02), "bulk IN ep2");
    assert!(has_endpoint(&c, 0x83, 0x03), "interrupt IN ep3");
    // CS interface descriptor (CCID class descriptor).
    assert!(
        find_sub(&c, 0x21).is_some(),
        "CCID class descriptor present"
    );
    let cs = find_sub(&c, 0x21).unwrap();
    assert_eq!(cs[1], 0x21, "bDescriptorType CCID");
    // dwFeatures at cs[10..14]: 0x000400FE
    let features = u32::from_le_bytes([cs[40], cs[41], cs[42], cs[43]]);
    assert_eq!(features, 0x0002_00FE);
    // bMaxCCIDBusySlots is the last byte
    assert_eq!(cs[cs.len() - 1], 1);
    // dwMaxCCIDMessageLength at cs[14..18] = 271
    let maxlen = u32::from_le_bytes([cs[44], cs[45], cs[46], cs[47]]);
    assert_eq!(maxlen, 271);
}

#[test]
fn string_descriptor_zero_and_indices() {
    assert_eq!(string_descriptor(0).unwrap()[1], 0x03);
    assert!(string_descriptor(1).unwrap().len() >= 2);
    assert!(string_descriptor(9).is_none(), "out of range -> stall");
}

#[test]
fn control_get_descriptor_dispatch() {
    // GET_DESCRIPTOR(CONFIG) standard request
    let setup = [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 0x40, 0x00];
    let r = handle_control(&setup);
    assert!(matches!(r, ControlReply::Data(d) if d[1] == 0x02));
    // GET_DESCRIPTOR(DEVICE)
    let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
    assert!(matches!(handle_control(&setup), ControlReply::Data(d) if d.len() == 18));
    // SET_CONFIGURATION
    let setup = [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    assert!(matches!(handle_control(&setup), ControlReply::Ok));
    // Unknown -> Stall
    let setup = [0x40, 0xFF, 0, 0, 0, 0, 0, 0];
    assert!(matches!(handle_control(&setup), ControlReply::Stall));
}

#[test]
fn ccid_class_control_abort_and_get_requests() {
    // ABORT: bmRequestType 0x21, bRequest 0x01, wValue = bSeq<<8 | bSlot
    let setup = [0x21, 0x01, 0x02, 0x03, 0x00, 0x00, 0x00, 0x00];
    assert!(matches!(
        handle_control(&setup),
        ControlReply::Abort { slot: 2, seq: 3 }
    ));
    // GET_CLOCK_FREQUENCIES
    let setup = [0xA1, 0x02, 0, 0, 0, 0, 4, 0];
    match handle_control(&setup) {
        ControlReply::Data(d) => assert_eq!(d.len() % 4, 0),
        _ => panic!("expected data"),
    }
    // GET_DATA_RATES
    let setup = [0xA1, 0x03, 0, 0, 0, 0, 4, 0];
    match handle_control(&setup) {
        ControlReply::Data(d) => assert_eq!(d.len() % 4, 0),
        _ => panic!("expected data"),
    }
}
