//! Linux USB/IP 1.1.1 wire format. All integers are network byte order.
use crate::usb;
pub const USBIP_VERSION: u16 = 0x0111;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 5;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 3;
pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const CMD_SUBMIT_SIZE: usize = 48;
pub const CMD_UNLINK_SIZE: usize = 48;
pub const RET_SUBMIT_SIZE: usize = 48;
pub const RET_UNLINK_SIZE: usize = 48;
pub const ECONNRESET: i32 = -104;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpHeader {
    ReqDevlist,
    ReqImport { busid: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRepDevlist {
    pub path: String,
    pub busid: String,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub b_configuration_value: u8,
    pub b_num_configurations: u8,
    pub b_num_interfaces: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdSubmit {
    pub seqnum: u32,
    pub devid: u32,
    pub dir_in: bool,
    pub ep: u32,
    pub transfer_flags: u32,
    pub buffer_length: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdUnlink {
    pub seqnum: u32,
    pub unlink_seqnum: u32,
}
fn word(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
pub fn parse_op_header(b: &[u8]) -> Result<OpHeader, ParseError> {
    if b.len() < 8 || b[..2] != USBIP_VERSION.to_be_bytes() || word(b, 4) != 0 {
        return Err(ParseError);
    }
    match u16::from_be_bytes([b[2], b[3]]) {
        OP_REQ_DEVLIST if b.len() == 8 => Ok(OpHeader::ReqDevlist),
        OP_REQ_IMPORT if b.len() == 40 => {
            let end = b[8..].iter().position(|v| *v == 0).ok_or(ParseError)?;
            if end == 0 {
                return Err(ParseError);
            };
            Ok(OpHeader::ReqImport {
                busid: std::str::from_utf8(&b[8..8 + end])
                    .map_err(|_| ParseError)?
                    .into(),
            })
        }
        _ => Err(ParseError),
    }
}
fn common(op: u16, status: u32) -> Vec<u8> {
    let mut b = vec![];
    b.extend(USBIP_VERSION.to_be_bytes());
    b.extend(op.to_be_bytes());
    b.extend(status.to_be_bytes());
    b
}
fn device(d: &OpRepDevlist) -> Vec<u8> {
    let mut b = vec![0; 312];
    let path = b"/tapbridge/1-1";
    b[..path.len()].copy_from_slice(path);
    let bus = d.busid.as_bytes();
    let n = bus.len().min(31);
    b[256..256 + n].copy_from_slice(&bus[..n]);
    for (o, v) in [(288, d.busnum), (292, d.devnum), (296, d.speed)] {
        b[o..o + 4].copy_from_slice(&v.to_be_bytes())
    }
    for (o, v) in [(300, d.id_vendor), (302, d.id_product), (304, d.bcd_device)] {
        b[o..o + 2].copy_from_slice(&v.to_be_bytes())
    }
    b[306..312].copy_from_slice(&[
        d.device_class,
        d.device_subclass,
        d.device_protocol,
        d.b_configuration_value,
        d.b_num_configurations,
        d.b_num_interfaces,
    ]);
    b
}
fn ours() -> OpRepDevlist {
    OpRepDevlist {
        path: String::new(),
        busid: "1-1".into(),
        busnum: 1,
        devnum: 1,
        speed: 2,
        id_vendor: usb::id_vendor(),
        id_product: usb::id_product(),
        bcd_device: 0x100,
        device_class: 0,
        device_subclass: 0,
        device_protocol: 0,
        b_configuration_value: 1,
        b_num_configurations: 1,
        b_num_interfaces: 1,
    }
}
pub fn encode_op_rep_devlist(devices: &[OpRepDevlist]) -> Vec<u8> {
    let mut b = common(OP_REP_DEVLIST, 0);
    b.extend((devices.len() as u32).to_be_bytes());
    for d in devices {
        b.extend(device(d));
        for _ in 0..d.b_num_interfaces {
            b.extend([0x0b, 0, 0, 0]);
        }
    }
    b
}
pub fn encode_op_rep_import(_devid: u32) -> Vec<u8> {
    let mut b = common(OP_REP_IMPORT, 0);
    b.extend(device(&ours()));
    b
}
pub fn encode_op_rep_import_error() -> Vec<u8> {
    common(OP_REP_IMPORT, 1)
}
pub fn devlist() -> Vec<u8> {
    encode_op_rep_devlist(&[ours()])
}
pub fn parse_cmd_submit(b: &[u8]) -> Result<CmdSubmit, ParseError> {
    if b.len() != 48
        || word(b, 0) != 1
        || word(b, 12) > 1
        || word(b, 16) > 15
        || word(b, 24) > 65600
        || ![0, u32::MAX].contains(&word(b, 32))
    {
        return Err(ParseError);
    }
    Ok(CmdSubmit {
        seqnum: word(b, 4),
        devid: word(b, 8),
        dir_in: word(b, 12) == 1,
        ep: word(b, 16),
        transfer_flags: word(b, 20),
        buffer_length: word(b, 24),
    })
}
pub fn parse_cmd_unlink(b: &[u8]) -> Result<CmdUnlink, ParseError> {
    if b.len() != 48 || word(b, 0) != 2 {
        return Err(ParseError);
    }
    Ok(CmdUnlink {
        seqnum: word(b, 4),
        unlink_seqnum: word(b, 20),
    })
}
pub fn encode_ret_submit(seq: u32, _devid: u32, length: u32, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 48];
    b[..4].copy_from_slice(&3u32.to_be_bytes());
    b[4..8].copy_from_slice(&seq.to_be_bytes());
    b[24..28].copy_from_slice(&length.to_be_bytes());
    b.extend(data);
    b
}
pub fn encode_ret_submit_error(seq: u32, status: i32) -> Vec<u8> {
    let mut b = encode_ret_submit(seq, 0, 0, &[]);
    b[20..24].copy_from_slice(&status.to_be_bytes());
    b
}
pub fn encode_ret_unlink(seq: u32, status: i32) -> Vec<u8> {
    let mut b = vec![0; 48];
    b[..4].copy_from_slice(&4u32.to_be_bytes());
    b[4..8].copy_from_slice(&seq.to_be_bytes());
    b[20..24].copy_from_slice(&status.to_be_bytes());
    b
}
