// TDD RED: ccid codec tests written before implementation (slice 1).
// Protocol source: USB-IF CCID Rev 1.1, sections 6.1, 6.2, 6.3.
use tapbridge_host::ccid::*;

fn slot_status(present: bool, error: Option<u8>) -> Vec<u8> {
    slot_status_msg(0, 0x2a, present, error)
}

#[test]
fn notify_slot_change_is_two_bytes_present_bit0_changed_bit1() {
    let m = notify_slot_change(true, false);
    assert_eq!(m, vec![0x50, 0b01]);
    let m = notify_slot_change(false, true);
    assert_eq!(m, vec![0x50, 0b10]);
    let m = notify_slot_change(true, true);
    assert_eq!(m, vec![0x50, 0b11]);
    let m = notify_slot_change(false, false);
    assert_eq!(m, vec![0x50, 0b00]);
}

#[test]
fn slot_status_msg_shape() {
    let m = slot_status(true, None);
    assert_eq!(m[0], 0x81);
    assert_eq!(&m[1..5], &[0, 0, 0, 0]); // dwLength 0
    assert_eq!(m[5], 0); // bSlot
    assert_eq!(m[6], 0x2a); // bSeq
    assert_eq!(m[7], 0x00); // present+ok
    assert_eq!(m[8], 0x00);
    assert_eq!(m[9], 0x00); // bClockStatus running
    assert_eq!(m.len(), 10);
}

#[test]
fn slot_status_register_encoding() {
    // bmICCStatus bits0-1: 0 present, 2 absent. bmCommandStatus bits6-7:
    // 0 ok, 1 failed, 2 time extension (0x80, NOT 0xC0).
    let m = slot_status(false, None);
    assert_eq!(m[7], 0x02);
    let m = slot_status(true, Some(0xFE)); // ICC_MUTE
    assert_eq!(m[7], 0x40); // present + failed
    assert_eq!(m[8], 0xFE);
    let m = slot_status(false, Some(0xFE));
    assert_eq!(m[7], 0x42, "absent + failed (classic error block)");
    let m = slot_status(true, Some(0x00));
    assert_eq!(
        m[7], 0x40,
        "unsupported command error zero is still a failure"
    );
}

#[test]
fn parse_icc_power_on() {
    let msg = [0x62, 0, 0, 0, 0, 0, 1, 0x01, 0, 0];
    let cmd = parse_bulk_out(&msg).unwrap();
    assert_eq!(cmd.seq, 1);
    assert!(matches!(cmd.kind, BulkOutKind::IccPowerOn { .. }));
    assert!(cmd.ab_data.is_empty());
}

#[test]
fn parse_xfr_block_with_data() {
    let mut msg = vec![0x6F, 4, 0, 0, 0, 0, 7, 0x05, 0x00, 0x00];
    msg.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let cmd = parse_bulk_out(&msg).unwrap();
    assert_eq!(cmd.seq, 7);
    assert_eq!(cmd.ab_data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn parse_rejects_short_and_length_mismatch() {
    assert!(parse_bulk_out(&[0x62, 0, 0]).is_err());
    // dwLength says 4 but no data present
    let msg = [0x6F, 4, 0, 0, 0, 0, 7, 0x05, 0x00, 0x00];
    assert!(parse_bulk_out(&msg).is_err());
}

#[test]
fn data_block_response_roundtrip() {
    let m = data_block(0x11, 0x00, vec![0xAA, 0xBB], false);
    assert_eq!(m[0], 0x80);
    assert_eq!(&m[1..5], &2u32.to_le_bytes());
    assert_eq!(m[5], 0x11);
    assert_eq!(m[7], 0x00);
    assert_eq!(&m[10..], &[0xAA, 0xBB]);
}

#[test]
fn parse_abort() {
    let msg = [0x72, 0, 0, 0, 0, 0, 3, 0, 0, 0];
    let cmd = parse_bulk_out(&msg).unwrap();
    assert_eq!(cmd.seq, 3);
    assert!(matches!(cmd.kind, BulkOutKind::Abort));
}

#[test]
fn parse_set_parameters_t1_and_get_params() {
    let mut msg = vec![0x61, 7, 0, 0, 0, 0, 9, 0x01, 0, 0];
    // bmTCCKST1 etc: 7-byte T=1 protocol data structure
    msg.extend_from_slice(&[0x12, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFE]);
    let cmd = parse_bulk_out(&msg).unwrap();
    assert_eq!(cmd.ab_data.len(), 7);
    let msg = [0x6C, 0, 0, 0, 0, 0, 2, 0, 0, 0];
    assert!(parse_bulk_out(&msg).is_ok());
}
