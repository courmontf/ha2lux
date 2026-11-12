//! DDC/CI (VESA) frames for the brightness VCP code: encoding and parsing,
//! with no I/O at all.
//!
//! Format (to be confirmed on real hardware, see `specs/VALIDATION_MATERIEL.md`):
//! - "Get VCP" request sent to slave address 0x37:
//!   `51 82 01 <vcp> <chk>`;
//! - "Set VCP" request: `51 84 03 <vcp> <val_hi> <val_lo> <chk>`;
//!   in both cases `chk = 0x6E ⊕ (all preceding bytes)`;
//! - "Get VCP" response (11 bytes):
//!   `6E 88 02 <result> <vcp> <type> <max_hi> <max_lo> <cur_hi> <cur_lo> <chk>`
//!   with `chk = 0x50 ⊕ (all preceding bytes)`;
//! - "null message": `6E 80 <chk>` (the display is not ready: transient failure).

use thiserror::Error;

/// I²C slave address of the DDC/CI channel.
pub const DDC_ADDRESS: u16 = 0x37;
/// Brightness VCP code.
pub const VCP_BRIGHTNESS: u8 = 0x10;
/// Size of a complete "Get VCP" response.
pub const GET_VCP_REPLY_LEN: usize = 11;

const HOST_ADDRESS: u8 = 0x51;
const DISPLAY_WRITE_ADDRESS: u8 = 0x6E;
const REPLY_CHECKSUM_SEED: u8 = 0x50;
const OPCODE_GET_VCP: u8 = 0x01;
const OPCODE_GET_VCP_REPLY: u8 = 0x02;
const OPCODE_SET_VCP: u8 = 0x03;

/// Errors when parsing a DDC/CI response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum DdcError {
    /// Response too short to be parsed.
    #[error("DDC/CI response too short ({0} bytes)")]
    TooShort(usize),
    /// The display answers "null message": not ready, retry.
    #[error("the display answers \"null message\" (not ready)")]
    NullMessage,
    /// Invalid checksum.
    #[error("invalid DDC/CI checksum")]
    BadChecksum,
    /// Unexpected frame (address, length or opcode).
    #[error("trame DDC/CI inattendue")]
    UnexpectedFrame,
    /// The response is for a different VCP code than the one requested.
    #[error("response for another VCP code (0x{0:02X})")]
    WrongVcp(u8),
    /// The display does not support this VCP code.
    #[error("VCP code not supported by the display")]
    UnsupportedVcp,
}

impl DdcError {
    /// True if a new attempt may succeed (display not ready, corrupted frame).
    pub fn is_transient(self) -> bool {
        !matches!(self, Self::UnsupportedVcp)
    }
}

fn xor(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |acc, b| acc ^ b)
}

/// "Get VCP" request for VCP code `vcp`.
pub fn encode_get_vcp(vcp: u8) -> [u8; 5] {
    let body = [HOST_ADDRESS, 0x82, OPCODE_GET_VCP, vcp];
    let checksum = xor(DISPLAY_WRITE_ADDRESS, &body);
    [body[0], body[1], body[2], body[3], checksum]
}

/// "Set VCP" request: writes `value` (16 bits) to VCP code `vcp`.
pub fn encode_set_vcp(vcp: u8, value: u16) -> [u8; 7] {
    let [hi, lo] = value.to_be_bytes();
    let body = [HOST_ADDRESS, 0x84, OPCODE_SET_VCP, vcp, hi, lo];
    let checksum = xor(DISPLAY_WRITE_ADDRESS, &body);
    [
        body[0], body[1], body[2], body[3], body[4], body[5], checksum,
    ]
}

/// Value read from a VCP code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VcpReading {
    /// VCP code.
    pub vcp: u8,
    /// Current value.
    pub current: u16,
    /// Maximum value.
    pub maximum: u16,
}

/// Parses a "Get VCP" response expected for `expected_vcp`.
pub fn parse_get_vcp_reply(reply: &[u8], expected_vcp: u8) -> Result<VcpReading, DdcError> {
    if reply.len() < 3 {
        return Err(DdcError::TooShort(reply.len()));
    }
    if reply[0] != DISPLAY_WRITE_ADDRESS || reply[1] & 0x80 == 0 {
        return Err(DdcError::UnexpectedFrame);
    }
    let payload_len = usize::from(reply[1] & 0x7F);
    let total = payload_len + 3;
    if reply.len() < total {
        return Err(DdcError::TooShort(reply.len()));
    }
    if xor(REPLY_CHECKSUM_SEED, &reply[..total - 1]) != reply[total - 1] {
        return Err(DdcError::BadChecksum);
    }
    if payload_len == 0 {
        return Err(DdcError::NullMessage);
    }
    if payload_len != 8 || reply[2] != OPCODE_GET_VCP_REPLY {
        return Err(DdcError::UnexpectedFrame);
    }
    if reply[3] != 0 {
        return Err(DdcError::UnsupportedVcp);
    }
    if reply[4] != expected_vcp {
        return Err(DdcError::WrongVcp(reply[4]));
    }
    Ok(VcpReading {
        vcp: reply[4],
        maximum: u16::from_be_bytes([reply[6], reply[7]]),
        current: u16::from_be_bytes([reply[8], reply[9]]),
    })
}

/// Builds a valid "Get VCP" response (useful for fake displays and tests).
pub fn encode_get_vcp_reply(vcp: u8, current: u16, maximum: u16) -> [u8; GET_VCP_REPLY_LEN] {
    let [mh, ml] = maximum.to_be_bytes();
    let [ch, cl] = current.to_be_bytes();
    let body = [
        DISPLAY_WRITE_ADDRESS,
        0x88,
        OPCODE_GET_VCP_REPLY,
        0x00,
        vcp,
        0x00,
        mh,
        ml,
        ch,
        cl,
    ];
    let mut frame = [0u8; GET_VCP_REPLY_LEN];
    frame[..10].copy_from_slice(&body);
    frame[10] = xor(REPLY_CHECKSUM_SEED, &body);
    frame
}

/// Builds a valid "null message".
pub fn encode_null_message() -> [u8; 3] {
    let body = [DISPLAY_WRITE_ADDRESS, 0x80];
    [body[0], body[1], xor(REPLY_CHECKSUM_SEED, &body)]
}

/// Decodes a request received by a display (for fake displays): returns
/// the opcode, the VCP code and the optional value, after checking the checksum.
pub fn decode_request(frame: &[u8]) -> Option<(u8, u8, Option<u16>)> {
    if frame.len() < 5 || frame[0] != HOST_ADDRESS || frame[1] & 0x80 == 0 {
        return None;
    }
    let total = usize::from(frame[1] & 0x7F) + 3;
    if frame.len() != total || xor(DISPLAY_WRITE_ADDRESS, &frame[..total - 1]) != frame[total - 1] {
        return None;
    }
    match (frame[2], total) {
        (OPCODE_GET_VCP, 5) => Some((OPCODE_GET_VCP, frame[3], None)),
        (OPCODE_SET_VCP, 7) => Some((
            OPCODE_SET_VCP,
            frame[3],
            Some(u16::from_be_bytes([frame[4], frame[5]])),
        )),
        _ => None,
    }
}
