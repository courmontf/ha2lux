//! Analyse d'un bloc EDID de base (128 octets) et identité stable d'un écran.

use thiserror::Error;

/// Taille d'un bloc EDID de base.
pub const EDID_BLOCK_LEN: usize = 128;
const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// Erreurs d'analyse EDID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum EdidError {
    /// Moins de 128 octets.
    #[error("EDID too short ({0} bytes)")]
    TooShort(usize),
    /// En-tête fixe absent.
    #[error("EDID without a valid header")]
    BadHeader,
}

/// Informations extraites d'un EDID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edid {
    /// Code fabricant PNP sur trois lettres (ex. `DEL`).
    pub manufacturer: String,
    /// Code produit.
    pub product: u16,
    /// Numéro de série numérique (0 si absent).
    pub serial: u32,
    /// Descripteur « nom du moniteur » (0xFC).
    pub name: Option<String>,
    /// Descripteur « numéro de série » texte (0xFF).
    pub serial_text: Option<String>,
    /// Semaine de fabrication.
    pub week: u8,
    /// Année de fabrication.
    pub year: u16,
    /// Nombre de blocs d'extension.
    pub extensions: u8,
    /// La somme de contrôle du bloc est correcte (beaucoup d'écrans réels l'ont fausse).
    pub checksum_ok: bool,
    /// Empreinte FNV-1a 64 bits des 128 premiers octets (identité de repli, non cryptographique).
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
    /// Analyse les 128 premiers octets d'un EDID.
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

    /// Le numéro de série numérique est exploitable : ni 0, ni les valeurs de
    /// remplissage courantes (`0x01010101`, `0xFFFFFFFF`) que portent de nombreux
    /// écrans de série et qui ne distinguent donc pas deux exemplaires.
    pub fn has_real_serial(&self) -> bool {
        !matches!(self.serial, 0 | 0x0101_0101 | 0xFFFF_FFFF)
    }

    /// Identité stable : fabricant + produit + série numérique ; à défaut série
    /// texte ; à défaut connecteur + empreinte de l'EDID.
    ///
    /// Deux écrans identiques de séries différentes restent distincts ; un
    /// changement de connecteur ne change pas l'identité quand une série existe.
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

    /// Nom affiché : descripteur de nom, sinon le nom du connecteur.
    pub fn display_name(&self, connector: &str) -> String {
        self.name.clone().unwrap_or_else(|| connector.to_owned())
    }

    /// Construit un EDID synthétique valide (écrans factices et tests).
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
