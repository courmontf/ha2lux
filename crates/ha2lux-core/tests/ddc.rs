//! Trames DDC/CI : encodage, analyse, cas d'erreur.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::ddc::{
    DdcError, VCP_BRIGHTNESS, decode_request, encode_get_vcp, encode_get_vcp_reply,
    encode_null_message, encode_set_vcp, parse_get_vcp_reply,
};
use proptest::prelude::*;

/// Recalcule la somme de contrôle d'une réponse (graine 0x50) après modification.
fn fix_reply_checksum(frame: &mut [u8]) {
    let n = frame.len() - 1;
    frame[n] = frame[..n].iter().fold(0x50, |a, b| a ^ b);
}

#[test]
fn get_vcp_request_matches_hand_computed_frame() {
    // 0x6E ^ 0x51 ^ 0x82 ^ 0x01 ^ 0x10 = 0xAC
    assert_eq!(
        encode_get_vcp(VCP_BRIGHTNESS),
        [0x51, 0x82, 0x01, 0x10, 0xAC]
    );
}

#[test]
fn set_vcp_request_matches_hand_computed_frame() {
    // 0x6E ^ 0x51 ^ 0x84 ^ 0x03 ^ 0x10 ^ 0x00 ^ 0x32 = 0x9A
    assert_eq!(
        encode_set_vcp(VCP_BRIGHTNESS, 50),
        [0x51, 0x84, 0x03, 0x10, 0x00, 0x32, 0x9A]
    );
    assert_eq!(
        encode_set_vcp(0x10, 0x0100)[4..6],
        [0x01, 0x00],
        "valeur sur 16 bits, poids fort d'abord"
    );
}

#[test]
fn parses_a_hand_built_reply() {
    // max 100, courant 50 ; 0x50 ^ (6E 88 02 00 10 00 00 64 00 32) = 0xF2
    let reply = [
        0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, 0x00, 0x64, 0x00, 0x32, 0xF2,
    ];
    let reading = parse_get_vcp_reply(&reply, VCP_BRIGHTNESS).unwrap();
    assert_eq!(
        (reading.vcp, reading.current, reading.maximum),
        (0x10, 50, 100)
    );
}

#[test]
fn reply_round_trip_including_16_bit_values() {
    for (current, maximum) in [
        (0, 100),
        (100, 100),
        (128, 255),
        (0xFFFF, 0xFFFF),
        (12345, 65535),
    ] {
        let frame = encode_get_vcp_reply(0x10, current, maximum);
        let reading = parse_get_vcp_reply(&frame, 0x10).unwrap();
        assert_eq!((reading.current, reading.maximum), (current, maximum));
    }
}

#[test]
fn null_message_is_a_transient_error() {
    let error = parse_get_vcp_reply(&encode_null_message(), 0x10).unwrap_err();
    assert_eq!(error, DdcError::NullMessage);
    assert!(error.is_transient());
}

#[test]
fn corrupted_checksum_is_rejected() {
    let mut frame = encode_get_vcp_reply(0x10, 50, 100);
    frame[10] ^= 0xFF;
    assert_eq!(
        parse_get_vcp_reply(&frame, 0x10),
        Err(DdcError::BadChecksum)
    );
    let mut frame = encode_get_vcp_reply(0x10, 50, 100);
    frame[8] ^= 0x01; // donnée altérée, somme inchangée
    assert_eq!(
        parse_get_vcp_reply(&frame, 0x10),
        Err(DdcError::BadChecksum)
    );
}

#[test]
fn short_or_garbage_frames_are_rejected() {
    assert_eq!(parse_get_vcp_reply(&[], 0x10), Err(DdcError::TooShort(0)));
    assert_eq!(
        parse_get_vcp_reply(&[0x6E, 0x88], 0x10),
        Err(DdcError::TooShort(2))
    );
    let frame = encode_get_vcp_reply(0x10, 50, 100);
    assert!(matches!(
        parse_get_vcp_reply(&frame[..8], 0x10),
        Err(DdcError::TooShort(8))
    ));
    assert_eq!(
        parse_get_vcp_reply(&[0xFF; 11], 0x10),
        Err(DdcError::UnexpectedFrame)
    );
    assert_eq!(
        parse_get_vcp_reply(&[0x00; 11], 0x10),
        Err(DdcError::UnexpectedFrame)
    );
}

#[test]
fn reply_for_another_vcp_is_rejected() {
    let frame = encode_get_vcp_reply(0x12, 50, 100);
    assert_eq!(
        parse_get_vcp_reply(&frame, 0x10),
        Err(DdcError::WrongVcp(0x12))
    );
}

#[test]
fn unsupported_vcp_is_not_transient() {
    let mut frame = encode_get_vcp_reply(0x10, 50, 100);
    frame[3] = 0x01; // code de résultat « non pris en charge »
    fix_reply_checksum(&mut frame);
    let error = parse_get_vcp_reply(&frame, 0x10).unwrap_err();
    assert_eq!(error, DdcError::UnsupportedVcp);
    assert!(!error.is_transient());
}

#[test]
fn wrong_opcode_or_length_is_rejected() {
    let mut frame = encode_get_vcp_reply(0x10, 50, 100);
    frame[2] = 0x03;
    fix_reply_checksum(&mut frame);
    assert_eq!(
        parse_get_vcp_reply(&frame, 0x10),
        Err(DdcError::UnexpectedFrame)
    );
}

#[test]
fn requests_decode_back() {
    assert_eq!(
        decode_request(&encode_get_vcp(0x10)),
        Some((0x01, 0x10, None))
    );
    assert_eq!(
        decode_request(&encode_set_vcp(0x10, 77)),
        Some((0x03, 0x10, Some(77)))
    );
    let mut bad = encode_set_vcp(0x10, 77);
    bad[6] ^= 1;
    assert_eq!(decode_request(&bad), None);
    assert_eq!(decode_request(&[0x51, 0x82]), None);
    assert_eq!(decode_request(&[]), None);
}

proptest! {
    #[test]
    fn parser_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..40), vcp in any::<u8>()) {
        let _ = parse_get_vcp_reply(&bytes, vcp);
        let _ = decode_request(&bytes);
    }

    #[test]
    fn any_reading_round_trips(vcp in any::<u8>(), current in any::<u16>(), maximum in any::<u16>()) {
        let reading = parse_get_vcp_reply(&encode_get_vcp_reply(vcp, current, maximum), vcp).unwrap();
        prop_assert_eq!((reading.current, reading.maximum), (current, maximum));
    }

    #[test]
    fn any_single_bit_flip_is_detected(current in any::<u16>(), maximum in any::<u16>(), byte in 0usize..11, bit in 0u8..8) {
        let mut frame = encode_get_vcp_reply(0x10, current, maximum);
        frame[byte] ^= 1 << bit;
        prop_assert!(parse_get_vcp_reply(&frame, 0x10).is_err());
    }

    #[test]
    fn set_requests_round_trip(vcp in any::<u8>(), value in any::<u16>()) {
        prop_assert_eq!(decode_request(&encode_set_vcp(vcp, value)), Some((0x03, vcp, Some(value))));
    }
}
