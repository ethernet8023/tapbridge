//! Independent Linux USB/IP wire assertions, not encoder round trips.
use tapbridge_host::usbip;
#[test]
fn import_is_standard_common_plus_usb_device() {
    let packet = usbip::encode_op_rep_import(0x10001);
    assert_eq!(&packet[..8], &[1, 17, 0, 3, 0, 0, 0, 0]);
    assert_eq!(packet.len(), 320);
    assert_eq!(&packet[264..267], b"1-1");
}
#[test]
fn returns_have_explicit_command_and_standard_offsets() {
    let packet = usbip::encode_ret_submit(7, 0, 2, &[0x90, 0]);
    assert_eq!(&packet[..8], &[0, 0, 0, 3, 0, 0, 0, 7]);
    assert_eq!(&packet[24..28], &[0, 0, 0, 2]);
    assert_eq!(&packet[48..], &[0x90, 0]);
    let unlink = usbip::encode_ret_unlink(9, -104);
    assert_eq!(unlink.len(), 48);
    assert_eq!(&unlink[..8], &[0, 0, 0, 4, 0, 0, 0, 9]);
    assert_eq!(&unlink[20..24], &(-104i32).to_be_bytes());
}
