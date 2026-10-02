//! Pure CCID 1.1 short-APDU codec (USB-IF CCID Rev 1.1 §6). No I/O.
//!
//! Wire layout: bMessageType(1) dwLength(4, LE) bSlot(1) bSeq(1) msg-specific
//! fields then abData. All multi-byte integers little-endian per §1.3.

/// Opaque parse/validation error (no details leak into wire or logs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

/// Bulk-OUT bMessageType values (§6.1-1).
pub const ICC_POWER_ON: u8 = 0x62;
pub const ICC_POWER_OFF: u8 = 0x63;
pub const GET_SLOT_STATUS: u8 = 0x65;
pub const XFR_BLOCK: u8 = 0x6F;
pub const GET_PARAMETERS: u8 = 0x6C;
pub const RESET_PARAMETERS: u8 = 0x6D;
pub const SET_PARAMETERS: u8 = 0x61;
pub const ABORT: u8 = 0x72;

/// Bulk-IN bMessageType values (§6.2-1).
pub const RDR_DATA_BLOCK: u8 = 0x80;
pub const RDR_SLOT_STATUS: u8 = 0x81;
pub const RDR_PARAMETERS: u8 = 0x82;

/// Slot error register values (Table 6.2-2).
pub const ERR_CMD_ABORTED: u8 = 0xFF;
pub const ERR_ICC_MUTE: u8 = 0xFE;
pub const ERR_HW_ERROR: u8 = 0xFB;
pub const ERR_CMD_SLOT_BUSY: u8 = 0xE0;
pub const ERR_CMD_NOT_SUPPORTED: u8 = 0x00;

/// Maximum message size we accept on bulk-OUT. Short-APDU device:
/// dwMaxCCIDMessageLength 271 like the class descriptor advertises.
pub const MAX_MESSAGE_LENGTH: usize = 271;

/// One parsed PC_to_RDR command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkOutCommand {
    pub kind: BulkOutKind,
    pub slot: u8,
    pub seq: u8,
    pub ab_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkOutKind {
    IccPowerOn { power_select: u8 },
    IccPowerOff,
    GetSlotStatus,
    XfrBlock { bwi: u8 },
    GetParameters,
    ResetParameters,
    SetParameters { protocol_num: u8 },
    Abort,
    Unknown(u8),
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Parse a complete bulk-OUT CCID message. `msg` must be the whole message
/// (the USB/IP layer has already reassembled the transfer). Rejects short
/// headers and dwLength/actual-length mismatches — never silently truncates.
pub fn parse_bulk_out(msg: &[u8]) -> Result<BulkOutCommand, ParseError> {
    if msg.len() < 10 {
        return Err(ParseError);
    }
    let kind_byte = msg[0];
    let dw_length = le32(&msg[1..5]) as usize;
    let slot = msg[5];
    let seq = msg[6];
    let body = &msg[10..];
    if body.len() < dw_length || msg.len() > MAX_MESSAGE_LENGTH {
        return Err(ParseError);
    }
    // Trailing garbage beyond dwLength is a malformed message.
    if body.len() != dw_length {
        return Err(ParseError);
    }
    let kind = match kind_byte {
        ICC_POWER_ON => BulkOutKind::IccPowerOn {
            power_select: msg[7],
        },
        ICC_POWER_OFF => BulkOutKind::IccPowerOff,
        GET_SLOT_STATUS => BulkOutKind::GetSlotStatus,
        XFR_BLOCK => BulkOutKind::XfrBlock { bwi: msg[7] },
        GET_PARAMETERS => BulkOutKind::GetParameters,
        RESET_PARAMETERS => BulkOutKind::ResetParameters,
        SET_PARAMETERS => {
            if msg[7] != 0x01 {
                // Only T=1 structure supported (short-APDU, T=1 contactless).
                return Err(ParseError);
            }
            BulkOutKind::SetParameters {
                protocol_num: msg[7],
            }
        }
        ABORT => {
            if dw_length != 0 {
                return Err(ParseError);
            }
            BulkOutKind::Abort
        }
        other => BulkOutKind::Unknown(other),
    };
    Ok(BulkOutCommand {
        kind,
        slot,
        seq,
        ab_data: body.to_vec(),
    })
}

/// bStatus register (§6.2-3): bmICCStatus bits 0-1, bmCommandStatus bits 6-7.
/// Time extension is 0b10 in bits 6-7 (0x80).
pub fn b_status(present: bool, command_status: CommandStatus) -> u8 {
    let icc = if present { 0b00 } else { 0b10 };
    let cs: u8 = match command_status {
        CommandStatus::Ok => 0b00,
        CommandStatus::Failed => 0b01,
        CommandStatus::TimeExtension => 0b10,
    };
    icc | (cs << 6)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    Ok,
    Failed,
    TimeExtension,
}

/// RDR_to_PC_SlotStatus (§6.2.2), 10 bytes.
pub fn slot_status_msg(slot: u8, seq: u8, present: bool, error: Option<u8>) -> Vec<u8> {
    let (status, err) = match error {
        None => (CommandStatus::Ok, 0u8),
        Some(e) => (CommandStatus::Failed, e),
    };
    let mut m = vec![RDR_SLOT_STATUS];
    m.extend_from_slice(&0u32.to_le_bytes());
    m.push(slot);
    m.push(seq);
    m.push(b_status(present, status));
    m.push(err);
    m.push(0x00); // bClockStatus: clock running
    m
}

/// RDR_to_PC_DataBlock (§6.2.1).
pub fn data_block(slot: u8, seq: u8, data: Vec<u8>, failed: bool) -> Vec<u8> {
    let mut m = vec![RDR_DATA_BLOCK];
    m.extend_from_slice(&(data.len() as u32).to_le_bytes());
    m.push(slot);
    m.push(seq);
    m.push(b_status(
        true,
        if failed {
            CommandStatus::Failed
        } else {
            CommandStatus::Ok
        },
    ));
    m.push(if failed { ERR_ICC_MUTE } else { 0 });
    m.push(0x00); // bChainParameter RFU for short APDU
    m.extend_from_slice(&data);
    m
}

/// RDR_to_PC_Parameters (§6.2.3) with the 7-byte T=1 protocol data structure.
pub fn parameters(slot: u8, seq: u8, present: bool, data: &[u8; 7]) -> Vec<u8> {
    let mut m = vec![RDR_PARAMETERS];
    m.extend_from_slice(&7u32.to_le_bytes());
    m.push(slot);
    m.push(seq);
    m.push(b_status(present, CommandStatus::Ok));
    m.push(0);
    m.push(0x01); // bProtocolNum: T=1
    m.extend_from_slice(data);
    m
}

/// Default T=1 protocol data structure (bmTCCKST1=0x12 direct convention,
/// GT=0, clock stop 0, IFSC=0xFE per class descriptor).
pub fn default_t1_parameters() -> [u8; 7] {
    [0x11, 0x10, 0x00, 0x4d, 0x00, 0xfe, 0x00]
}

/// RDR_to_PC_NotifySlotChange (§6.3.1): TWO bytes. bmSlotICCState bits:
/// bit0 = slot 0 present, bit1 = slot 0 changed.
pub fn notify_slot_change(present: bool, changed: bool) -> Vec<u8> {
    let mut state = 0u8;
    if present {
        state |= 0b01;
    }
    if changed {
        state |= 0b10;
    }
    vec![0x50, state]
}

/// ATR validation for the pseudo-ATR the phone supplies (ISO/IEC 7816-3):
/// TS must be 0x3B or 0x3F; TCK (XOR of T0..TK including TCK) must be 0 when
/// only T=0 is indicated or one protocol byte present; length <= 33.
/// Returns Ok(()) for a standards-valid ATR — never invents one.
pub fn validate_atr(atr: &[u8]) -> Result<(), ParseError> {
    if atr.len() < 2 || atr.len() > 33 {
        return Err(ParseError);
    }
    if !matches!(atr[0], 0x3B | 0x3F) {
        return Err(ParseError); // BAD_ATR_TS
    }
    let t0 = atr[1];
    let historical = (t0 & 0x0F) as usize;
    // Walk interface byte groups: Y1 = t0>>4, each TD_i chains Y_{i+1}.
    let mut i = 2usize;
    let mut y = t0 >> 4;
    let mut any_t_nonzero = false;
    while y != 0 {
        if i >= atr.len() {
            return Err(ParseError);
        }
        // Count present TA_i..TD_i; TD_i is the last byte of the group.
        let has_td = y & 0x08 != 0;
        let cnt = y.count_ones() as usize;
        if i + cnt > atr.len() {
            return Err(ParseError);
        }
        let td_value = if has_td {
            let td = atr[i + cnt - 1];
            if td & 0x0F != 0 {
                any_t_nonzero = true;
            }
            td
        } else {
            0
        };
        i += cnt;
        y = td_value >> 4;
    }
    // TCK: present iff any protocol T != 0 is indicated; XOR of T0..TK == 0.
    let expected = i + historical + usize::from(any_t_nonzero);
    if atr.len() != expected {
        return Err(ParseError);
    }
    let tck_present = any_t_nonzero;
    if any_t_nonzero {
        if !tck_present {
            return Err(ParseError);
        }
        let x = atr[1..].iter().fold(0u8, |acc, b| acc ^ b);
        if x != 0 {
            return Err(ParseError); // BAD_ATR_TCK
        }
    } else if tck_present {
        return Err(ParseError); // T=0 only: TCK must be absent
    }
    Ok(())
}
