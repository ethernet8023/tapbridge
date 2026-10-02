// Independent USB/IP wire assertions against the standard Linux protocol
// (https://docs.kernel.org/usb/usbip_protocol.html). All integers on the
// wire are BIG-endian. URB commands: 48-byte header, command at 0,
// seqnum at 4, devid at 8, direction at 12, ep at 16.
use tapbridge_host::usbip::*;

fn sample_devlist() -> Vec<OpRepDevlist> {
    vec![OpRepDevlist {
        path: String::new(),
        busid: "1-1".into(),
        busnum: 1,
        devnum: 1,
        speed: 2, // high speed
        id_vendor: 0x1209,
        id_product: 0x7A71,
        bcd_device: 0x0100,
        device_class: 0,
        device_subclass: 0,
        device_protocol: 0,
        b_configuration_value: 1,
        b_num_configurations: 1,
        b_num_interfaces: 1,
    }]
}

#[test]
fn op_req_devlist_roundtrip() {
    // OP common header: version 0x0111 at 0, OP_REQ_DEVLIST 0x8005 at 2,
    // status 0 at 4. Big-endian.
    let req = [0x01, 0x11, 0x80, 0x05, 0x00, 0x00, 0x00, 0x00];
    let op = parse_op_header(&req).unwrap();
    assert_eq!(op, OpHeader::ReqDevlist);
}

#[test]
fn op_req_import_parses_busid() {
    let mut req = [0u8; 8 + 32];
    req[0..2].copy_from_slice(&0x0111u16.to_be_bytes()); // version
    req[2..4].copy_from_slice(&0x8003u16.to_be_bytes()); // OP_REQ_IMPORT
    req[4..8].copy_from_slice(&0u32.to_be_bytes()); // status
    req[8..11].copy_from_slice(b"1-1");
    let op = parse_op_header(&req).unwrap();
    match op {
        OpHeader::ReqImport { busid } => assert_eq!(busid, "1-1"),
        _ => panic!("wrong op"),
    }
}

#[test]
fn op_rep_devlist_encoding() {
    // OP_REP_DEVLIST: common(8) + ndev(4) + per device 312 + interfaces 4n
    // => 12 + 312 + 4 = 328 bytes for one device with one interface.
    let m = encode_op_rep_devlist(&sample_devlist());
    assert_eq!(m.len(), 328);
    assert_eq!(&m[0..2], &0x0111u16.to_be_bytes(), "usbip version");
    assert_eq!(&m[2..4], &0x0005u16.to_be_bytes(), "OP_REP_DEVLIST");
    assert_eq!(&m[4..8], &0u32.to_be_bytes(), "status");
    let count = u32::from_be_bytes([m[8], m[9], m[10], m[11]]);
    assert_eq!(count, 1);
    // usb_device: path(256) busid(32) ... busid starts at 12+256.
    let busid = &m[12 + 256..12 + 256 + 32];
    assert_eq!(&busid[..3], b"1-1");
    // idVendor/idProduct: device-struct offsets 288+12/288+14; the device
    // entry starts after common(8)+ndev(4), so wire offset 300/302 + 12.
    let vid = u16::from_be_bytes([m[312], m[313]]);
    assert_eq!(vid, 0x1209);
    let pid = u16::from_be_bytes([m[314], m[315]]);
    assert_eq!(pid, 0x7A71);
    // Interface list after the device entry: 4 bytes, class 0x0B.
    let iface = &m[12 + 312..12 + 312 + 4];
    assert_eq!(iface[0], 0x0B, "smart-card class");
}

#[test]
fn op_rep_import_is_320_bytes_no_inline_descriptor() {
    let m = encode_op_rep_import(0x0001_0001);
    // OP_REP_IMPORT: common(8) + usb_device(312) = 320. No devid field and
    // no 18-byte descriptor on the wire (the descriptor is fetched later
    // via EP0 control transfers).
    assert_eq!(m.len(), 320);
    assert_eq!(&m[2..4], &0x0003u16.to_be_bytes(), "OP_REP_IMPORT");
    assert_eq!(&m[4..8], &0u32.to_be_bytes(), "status");
    // busid inside the usb_device struct at 8+256.
    assert_eq!(&m[8 + 256..8 + 259], b"1-1");
    assert_eq!(
        u16::from_be_bytes([m[8 + 288 + 12], m[8 + 288 + 13]]),
        0x1209
    );
}

#[test]
fn cmd_submit_header_offsets() {
    // CMD_SUBMIT: command(1) at 0, seqnum at 4, devid at 8, direction at 12
    // (0=OUT, 1=IN), ep at 16, transfer_flags at 20, buffer_length at 24,
    // start_frame 28, number_of_packets 32, interval 36, setup 40..48.
    let mut hdr = [0u8; CMD_SUBMIT_SIZE];
    hdr[0..4].copy_from_slice(&1u32.to_be_bytes()); // CMD_SUBMIT
    hdr[4..8].copy_from_slice(&0x01000001u32.to_be_bytes()); // seqnum
    hdr[8..12].copy_from_slice(&0x0001_0001u32.to_be_bytes()); // devid
    hdr[12..16].copy_from_slice(&0u32.to_be_bytes()); // direction OUT
    hdr[16..20].copy_from_slice(&1u32.to_be_bytes()); // ep1 = bulk OUT
    hdr[24..28].copy_from_slice(&4u32.to_be_bytes()); // buffer_length
    let s = parse_cmd_submit(&hdr).unwrap();
    assert_eq!(s.seqnum, 0x01000001);
    assert_eq!(s.devid, 0x0001_0001);
    assert_eq!(s.ep, 1);
    assert!(!s.dir_in);
    assert_eq!(s.buffer_length, 4);

    hdr[12..16].copy_from_slice(&1u32.to_be_bytes()); // IN
    hdr[16..20].copy_from_slice(&2u32.to_be_bytes()); // ep2 = bulk IN
    let s = parse_cmd_submit(&hdr).unwrap();
    assert!(s.dir_in);
    assert_eq!(s.ep, 2);
}

#[test]
fn cmd_unlink_header_offsets() {
    // CMD_UNLINK: command(2) at 0, seqnum of the unlink itself at 4,
    // target seqnum at 20. Fixed 48-byte packet.
    let mut hdr = [0u8; CMD_UNLINK_SIZE];
    hdr[0..4].copy_from_slice(&2u32.to_be_bytes());
    hdr[4..8].copy_from_slice(&7u32.to_be_bytes());
    hdr[20..24].copy_from_slice(&42u32.to_be_bytes());
    let u = parse_cmd_unlink(&hdr).unwrap();
    assert_eq!(u.seqnum, 7);
    assert_eq!(u.unlink_seqnum, 42);
}

#[test]
fn ret_submit_offsets() {
    // RET_SUBMIT: command(3) at 0, seqnum at 4, devid 8, direction 12,
    // ep 16, status at 20, actual_length at 24, start_frame 28,
    // number_of_packets 32, error_count 36, payload from 48.
    let r = encode_ret_submit(0x01000001, 0, 4, &[1, 2, 3, 4]);
    assert_eq!(&r[0..4], &3u32.to_be_bytes(), "RET_SUBMIT command");
    assert_eq!(&r[4..8], &0x01000001u32.to_be_bytes());
    assert_eq!(&r[20..24], &0i32.to_be_bytes(), "status");
    let actual = u32::from_be_bytes([r[24], r[25], r[26], r[27]]);
    assert_eq!(actual, 4);
    assert_eq!(r.len(), RET_SUBMIT_SIZE + 4);
    assert_eq!(&r[RET_SUBMIT_SIZE..RET_SUBMIT_SIZE + 4], &[1, 2, 3, 4]);
}

#[test]
fn ret_submit_short_transfer() {
    let r = encode_ret_submit(5, 0, 2, &[9, 9]);
    let actual = u32::from_be_bytes([r[24], r[25], r[26], r[27]]);
    assert_eq!(actual, 2, "actual < requested is a legal short transfer");
}

#[test]
fn ret_unlink_encoding() {
    // RET_UNLINK: command(4) at 0, seqnum at 4, status at 20, total 48.
    let r = encode_ret_unlink(7, -104);
    assert_eq!(r.len(), 48);
    assert_eq!(&r[0..4], &4u32.to_be_bytes());
    assert_eq!(&r[4..8], &7u32.to_be_bytes());
    let status = i32::from_be_bytes([r[20], r[21], r[22], r[23]]);
    assert_eq!(status, -104, "ECONNRESET for pending unlink");
}

#[test]
fn ret_submit_error_status() {
    let r2 = encode_ret_submit_error(9, -104);
    assert_eq!(r2.len(), RET_SUBMIT_SIZE);
    let status = i32::from_be_bytes([r2[20], r2[21], r2[22], r2[23]]);
    assert_eq!(status, -104);
    assert_eq!(
        u32::from_be_bytes([r2[24], r2[25], r2[26], r2[27]]),
        0,
        "no payload on error"
    );
}

#[test]
fn malformed_headers_rejected() {
    assert!(parse_op_header(&[0u8; 8]).is_err(), "unknown command");
    let mut req = [0u8; 8 + 32];
    req[2..4].copy_from_slice(&0x8003u16.to_be_bytes());
    assert!(parse_op_header(&req).is_err(), "wrong version rejected");
    let mut badver = [0u8; 8];
    badver[2..4].copy_from_slice(&0x8005u16.to_be_bytes());
    assert!(parse_op_header(&badver).is_err(), "version mismatch");
    assert!(parse_cmd_submit(&[0u8; 4]).is_err());
    assert!(parse_cmd_unlink(&[0u8; 4]).is_err());
    assert!(parse_cmd_submit(&[0u8; 48]).is_err(), "command 0 rejected");
}
