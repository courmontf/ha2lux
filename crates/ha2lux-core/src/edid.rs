//! Parsing of a base EDID block (128 bytes) and stable identity of a display.

use thiserror::Error;

/// Size of a base EDID block.
pub const EDID_BLOCK_LEN: usize = 128;
const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// EDID parsing errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EdidError {
    /// Fewer than 128 bytes.
    #[error("EDID too short ({0} bytes)")]
    TooShort(usize),
    /// Fixed header missing.
    #[error("EDID without a valid header")]
    BadHeader,
}

/// Information extracted from an EDID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edid {
    /// Three-letter PNP manufacturer code (e.g. `DEL`).
    pub manufacturer: String,
    /// Product code.
    pub product: u16,
    /// Numeric serial number (0 if absent).
    pub serial: u32,
    /// "Monitor name" descriptor (0xFC).
    pub name: Option<String>,
    /// Text "serial number" descriptor (0xFF).
    pub serial_text: Option<String>,
    /// Week of manufacture.
    pub week: u8,
    /// Year of manufacture.
    pub year: u16,
    /// Number of extension blocks.
    pub extensions: u8,
    /// The block checksum is correct (many real displays get it wrong).
    pub checksum_ok: bool,
    /// 64-bit FNV-1a fingerprint of the first 128 bytes (fallback identity, non-cryptographic).
    pub fingerprint: u64,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn descriptor_text(block: &[u8]) -> Option<String> {
    let text: String = block[5..18]
        .iter()
        .take_while(|b| **b != 0x0A && **b != 0x00)
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                char::from(*b)
            } else {
                '?'
            }
        })
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

impl Edid {
    /// Parses the first 128 bytes of an EDID.
    pub fn parse(data: &[u8]) -> Result<Self, EdidError> {
        if data.len() < EDID_BLOCK_LEN {
            return Err(EdidError::TooShort(data.len()));
        }
        if data[..8] != HEADER {
            return Err(EdidError::BadHeader);
        }
        let block = &data[..EDID_BLOCK_LEN];
        let id = u16::from_be_bytes([block[8], block[9]]);
        let letter = |shift: u16| {
            let value = u8::try_from((id >> shift) & 0x1F).unwrap_or(0);
            if (1..=26).contains(&value) {
                char::from(b'A' + value - 1)
            } else {
                '?'
            }
        };
        let manufacturer: String = [letter(10), letter(5), letter(0)].iter().collect();

        let mut name = None;
        let mut serial_text = None;
        for offset in [54, 72, 90, 108] {
            let descriptor = &block[offset..offset + 18];
            if descriptor[0] == 0 && descriptor[1] == 0 {
                match descriptor[3] {
                    0xFC => name = descriptor_text(descriptor),
                    0xFF => serial_text = descriptor_text(descriptor),
                    _ => {}
                }
            }
        }
        Ok(Self {
            manufacturer,
            product: u16::from_le_bytes([block[10], block[11]]),
            serial: u32::from_le_bytes([block[12], block[13], block[14], block[15]]),
            name,
            serial_text,
            week: block[16],
            year: 1990 + u16::from(block[17]),
            extensions: block[126],
            checksum_ok: block.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) == 0,
            fingerprint: fnv1a(block),
        })
    }

    /// The numeric serial number is usable: neither 0 nor the common
    /// filler values (`0x01010101`, `0xFFFFFFFF`) carried by many
    /// mass-produced displays, which therefore do not distinguish two units.
    pub fn has_real_serial(&self) -> bool {
        !matches!(self.serial, 0 | 0x0101_0101 | 0xFFFF_FFFF)
    }

    /// Stable identity: manufacturer + product + numeric serial; failing that,
    /// text serial; failing that, connector + EDID fingerprint.
    ///
    /// Two identical displays with different serials remain distinct; a
    /// connector change does not change the identity when a serial exists.
    pub fn identity(&self, connector: &str) -> String {
        if self.has_real_serial() {
            format!(
                "{}-{:04X}-{:08X}",
                self.manufacturer, self.product, self.serial
            )
        } else if let Some(text) = &self.serial_text {
            format!("{}-{:04X}-s{}", self.manufacturer, self.product, text)
        } else {
            format!("conn:{connector}:{:016x}", self.fingerprint)
        }
    }

    /// Display name: name descriptor, otherwise the connector name.
    pub fn display_name(&self, connector: &str) -> String {
        self.name.clone().unwrap_or_else(|| connector.to_owned())
    }

    /// Builds a valid synthetic EDID (fake displays and tests).
    pub fn build_block(
        manufacturer: &str,
        product: u16,
        serial: u32,
        name: Option<&str>,
        serial_text: Option<&str>,
    ) -> Vec<u8> {
        let mut block = vec![0u8; EDID_BLOCK_LEN];
        block[..8].copy_from_slice(&HEADER);
        let letters: Vec<u16> = manufacturer
            .bytes()
            .take(3)
            .map(|b| u16::from(b.to_ascii_uppercase().saturating_sub(b'A') + 1))
            .collect();
        let id = letters.iter().fold(0u16, |acc, l| (acc << 5) | (l & 0x1F));
        block[8..10].copy_from_slice(&id.to_be_bytes());
        block[10..12].copy_from_slice(&product.to_le_bytes());
        block[12..16].copy_from_slice(&serial.to_le_bytes());
        block[16] = 12;
        block[17] = 30;
        let mut put = |offset: usize, tag: u8, text: &str| {
            block[offset + 3] = tag;
            let mut field = [b' '; 13];
            let bytes = text.as_bytes();
            let n = bytes.len().min(12);
            field[..n].copy_from_slice(&bytes[..n]);
            field[n] = 0x0A;
            block[offset + 5..offset + 18].copy_from_slice(&field);
        };
        if let Some(name) = name {
            put(54, 0xFC, name);
        }
        if let Some(text) = serial_text {
            put(72, 0xFF, text);
        }
        let sum = block[..127].iter().fold(0u8, |s, b| s.wrapping_add(*b));
        block[127] = 0u8.wrapping_sub(sum);
        block
    }
}
