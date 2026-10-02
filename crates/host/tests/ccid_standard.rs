use tapbridge_host::{ccid, usb};
#[test]
fn ccid_descriptor_is_complete_and_t1_short_apdu() {
    let config = usb::config_descriptor();
    let descriptor = usb::find_sub(&config, 0x21).unwrap();
    assert_eq!(descriptor.len(), 54);
    assert_eq!(u32::from_le_bytes(descriptor[6..10].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(descriptor[40..44].try_into().unwrap()) & 0x70000,
        0x20000
    );
    assert_eq!(
        u32::from_le_bytes(descriptor[44..48].try_into().unwrap()),
        271
    );
    assert_eq!(descriptor[53], 1);
}
#[test]
fn t1_parameters_use_correct_field_positions() {
    let params = ccid::default_t1_parameters();
    assert_eq!(params, [0x11, 0x10, 0, 0x4d, 0, 0xfe, 0]);
}
#[test]
fn invalid_atr_is_rejected_without_underflow_or_trailing_bytes() {
    for atr in [
        vec![0x3b, 0x0f],
        vec![0x3b, 0, 0, 0],
        vec![0x3b, 0x80, 0x80, 1, 0x3a],
    ] {
        assert!(ccid::validate_atr(&atr).is_err());
    }
    assert!(ccid::validate_atr(&[0x3b, 0x80, 0x80, 1, 1]).is_ok());
}
#[test]
fn unsupported_command_zero_is_failure_not_time_extension() {
    assert_eq!(ccid::slot_status_msg(0, 9, true, Some(0))[7], 0x40);
}
