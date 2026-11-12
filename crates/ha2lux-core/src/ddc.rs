//! Trames DDC/CI (VESA) pour le code VCP de luminosité : encodage et analyse,
//! sans aucune E/S.
//!
//! Format (à confirmer sur du matériel réel, voir `specs/VALIDATION_MATERIEL.md`) :
//! - requête « Get VCP » envoyée à l'adresse esclave 0x37 :
//!   `51 82 01 <vcp> <chk>` ;
//! - requête « Set VCP » : `51 84 03 <vcp> <val_hi> <val_lo> <chk>` ;
//!   dans les deux cas `chk = 0x6E ⊕ (tous les octets précédents)` ;
//! - réponse « Get VCP » (11 octets) :
//!   `6E 88 02 <résultat> <vcp> <type> <max_hi> <max_lo> <cur_hi> <cur_lo> <chk>`
//!   avec `chk = 0x50 ⊕ (tous les octets précédents)` ;
//! - « null message » : `6E 80 <chk>` (l'écran n'est pas prêt : échec transitoire).

use thiserror::Error;

/// Adresse esclave I²C du canal DDC/CI.
pub const DDC_ADDRESS: u16 = 0x37;
/// Code VCP de la luminosité.
pub const VCP_BRIGHTNESS: u8 = 0x10;
/// Taille d'une réponse « Get VCP » complète.
pub const GET_VCP_REPLY_LEN: usize = 11;

const HOST_ADDRESS: u8 = 0x51;
const DISPLAY_WRITE_ADDRESS: u8 = 0x6E;
const REPLY_CHECKSUM_SEED: u8 = 0x50;
const OPCODE_GET_VCP: u8 = 0x01;
const OPCODE_GET_VCP_REPLY: u8 = 0x02;
const OPCODE_SET_VCP: u8 = 0x03;

/// Erreurs d'analyse d'une réponse DDC/CI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum DdcError {
    /// Réponse trop courte pour être analysée.
    #[error("DDC/CI response too short ({0} bytes)")]
    TooShort(usize),
    /// L'écran répond « null message » : pas prêt, à réessayer.
    #[error("the display answers \"null message\" (not ready)")]
    NullMessage,
    /// Somme de contrôle invalide.
    #[error("invalid DDC/CI checksum")]
    BadChecksum,
    /// Trame inattendue (adresse, longueur ou opcode).
    #[error("trame DDC/CI inattendue")]
    UnexpectedFrame,
    /// La réponse concerne un autre code VCP que celui demandé.
    #[error("response for another VCP code (0x{0:02X})")]
    WrongVcp(u8),
    /// L'écran ne prend pas en charge ce code VCP.
    #[error("VCP code not supported by the display")]
    UnsupportedVcp,
}

impl DdcError {
    /// Vrai si une nouvelle tentative peut réussir (écran pas prêt, trame corrompue).
    pub fn is_transient(self) -> bool {
        !matches!(self, Self::UnsupportedVcp)
    }
}

fn xor(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |acc, b| acc ^ b)
}

/// Requête « Get VCP » pour le code `vcp`.
pub fn encode_get_vcp(vcp: u8) -> [u8; 5] {
    let body = [HOST_ADDRESS, 0x82, OPCODE_GET_VCP, vcp];
    let checksum = xor(DISPLAY_WRITE_ADDRESS, &body);
    [body[0], body[1], body[2], body[3], checksum]
}

/// Requête « Set VCP » : écrit `value` (16 bits) dans le code `vcp`.
pub fn encode_set_vcp(vcp: u8, value: u16) -> [u8; 7] {
    let [hi, lo] = value.to_be_bytes();
    let body = [HOST_ADDRESS, 0x84, OPCODE_SET_VCP, vcp, hi, lo];
    let checksum = xor(DISPLAY_WRITE_ADDRESS, &body);
    [
        body[0], body[1], body[2], body[3], body[4], body[5], checksum,
    ]
}

/// Valeur lue d'un code VCP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VcpReading {
    /// Code VCP.
    pub vcp: u8,
    /// Valeur courante.
    pub current: u16,
    /// Valeur maximale.
    pub maximum: u16,
}

/// Analyse une réponse « Get VCP » attendue pour `expected_vcp`.
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

/// Construit une réponse « Get VCP » valide (utile aux écrans factices et aux tests).
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

/// Construit un « null message » valide.
pub fn encode_null_message() -> [u8; 3] {
    let body = [DISPLAY_WRITE_ADDRESS, 0x80];
    [body[0], body[1], xor(REPLY_CHECKSUM_SEED, &body)]
}

/// Décode une requête reçue par un écran (pour les écrans factices) : renvoie
/// l'opcode, le code VCP et la valeur éventuelle, après vérification de la somme.
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
