//! EDID : analyse et identité stable (T-DDC-08 côté pur).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{Edid, EdidError};
use proptest::prelude::*;

fn block(serial: u32, serial_text: Option<&str>) -> Vec<u8> {
    Edid::build_block("DEL", 0xA0B1, serial, Some("DELL U2720Q"), serial_text)
}

#[test]
fn parses_manufacturer_product_serial_and_names() {
    let data = block(0x1234_5678, Some("ABC123"));
    assert_eq!(
        &data[8..10],
        [0x10, 0xAC],
        "DEL = 0x10AC, comme les EDID Dell réels"
    );
    let edid = Edid::parse(&data).unwrap();
    assert_eq!(edid.manufacturer, "DEL");
    assert_eq!(edid.product, 0xA0B1);
    assert_eq!(edid.serial, 0x1234_5678);
    assert_eq!(edid.name.as_deref(), Some("DELL U2720Q"));
    assert_eq!(edid.serial_text.as_deref(), Some("ABC123"));
    assert_eq!((edid.year, edid.week, edid.extensions), (2020, 12, 0));
    assert!(edid.checksum_ok);
}

#[test]
fn rejects_short_or_headerless_data() {
    assert_eq!(Edid::parse(&[0; 10]), Err(EdidError::TooShort(10)));
    assert_eq!(Edid::parse(&[]), Err(EdidError::TooShort(0)));
    assert_eq!(Edid::parse(&[0; 128]), Err(EdidError::BadHeader));
}

#[test]
fn bad_checksum_is_flagged_but_still_parsed() {
    let mut data = block(1, None);
    data[127] ^= 0x55;
    let edid = Edid::parse(&data).unwrap();
    assert!(!edid.checksum_ok);
    assert_eq!(edid.manufacturer, "DEL");
}

#[test]
fn extension_blocks_do_not_break_parsing() {
    let mut data = block(1, None);
    data[126] = 1;
    data.extend_from_slice(&[0x02; 128]);
    assert_eq!(Edid::parse(&data).unwrap().extensions, 1);
}

#[test]
fn identity_prefers_numeric_serial() {
    let edid = Edid::parse(&block(0x1234_5678, Some("ABC"))).unwrap();
    assert_eq!(edid.identity("DP-1"), "DEL-A0B1-12345678");
}

#[test]
fn identity_falls_back_to_text_serial_then_connector_fingerprint() {
    let text = Edid::parse(&block(0, Some("ABC123"))).unwrap();
    assert_eq!(text.identity("DP-1"), "DEL-A0B1-sABC123");
    let bare = Edid::parse(&block(0, None)).unwrap();
    let id = bare.identity("HDMI-A-1");
    assert!(id.starts_with("conn:HDMI-A-1:"), "{id}");
    assert_ne!(id, bare.identity("DP-2"), "le repli dépend du connecteur");
    assert_eq!(id, bare.identity("HDMI-A-1"), "stable d'un appel à l'autre");
}

#[test]
fn t_ddc_08_identical_models_with_different_serials_stay_distinct() {
    let a = Edid::parse(&block(111, None)).unwrap();
    let b = Edid::parse(&block(222, None)).unwrap();
    assert_ne!(a.identity("DP-1"), b.identity("DP-1"));
    // L'identité suit l'écran, pas le connecteur ni l'ordre d'énumération.
    assert_eq!(a.identity("DP-1"), a.identity("HDMI-A-3"));
    let order1 = [a.identity("DP-1"), b.identity("DP-2")];
    let order2 = [b.identity("DP-1"), a.identity("DP-2")];
    assert!(order1.iter().all(|id| order2.contains(id)));
}

#[test]
fn same_model_without_any_serial_differs_only_by_connector() {
    let a = Edid::parse(&block(0, None)).unwrap();
    let b = Edid::parse(&block(0, None)).unwrap();
    assert_ne!(a.identity("DP-1"), b.identity("DP-2"));
}

#[test]
fn display_name_falls_back_to_connector() {
    let named = Edid::parse(&block(1, None)).unwrap();
    assert_eq!(named.display_name("DP-1"), "DELL U2720Q");
    let anonymous = Edid::parse(&Edid::build_block("XYZ", 1, 1, None, None)).unwrap();
    assert_eq!(anonymous.display_name("DP-1"), "DP-1");
}

#[test]
fn non_ascii_descriptor_bytes_are_neutralized() {
    let mut data = block(1, None);
    data[54 + 5] = 0xE9;
    data[54 + 6] = 0x01;
    let name = Edid::parse(&data).unwrap().name.unwrap();
    assert!(name.starts_with("??"), "{name}");
}

#[test]
fn invalid_manufacturer_letters_do_not_panic() {
    let mut data = block(1, None);
    data[8] = 0xFF;
    data[9] = 0xFF;
    assert_eq!(Edid::parse(&data).unwrap().manufacturer.len(), 3);
}

proptest! {
    #[test]
    fn parser_never_panics_on_arbitrary_data(tail in proptest::collection::vec(any::<u8>(), 120..400)) {
        let mut data = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        data.extend(tail);
        let edid = Edid::parse(&data).unwrap();
        let _ = edid.identity("DP-1");
        let _ = edid.display_name("DP-1");
    }

    #[test]
    fn random_prefix_never_panics(data in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = Edid::parse(&data);
    }

    #[test]
    fn built_blocks_round_trip(product in any::<u16>(), serial in any::<u32>()) {
        let edid = Edid::parse(&Edid::build_block("ABC", product, serial, Some("X"), None)).unwrap();
        prop_assert_eq!((edid.manufacturer.as_str(), edid.product, edid.serial), ("ABC", product, serial));
        prop_assert!(edid.checksum_ok);
    }
}

#[test]
fn placeholder_serials_do_not_count_as_a_real_serial() {
    for placeholder in [0u32, 0x0101_0101, 0xFFFF_FFFF] {
        let edid = Edid::parse(&block(placeholder, None)).unwrap();
        assert!(!edid.has_real_serial(), "{placeholder:#X}");
        assert!(
            edid.identity("DP-1").starts_with("conn:DP-1:"),
            "repli connecteur + empreinte"
        );
    }
    assert!(
        Edid::parse(&block(0x1234_5678, None))
            .unwrap()
            .has_real_serial()
    );
}

#[test]
fn placeholder_numeric_serial_falls_back_to_text_serial() {
    let edid = Edid::parse(&block(0x0101_0101, Some("SN42"))).unwrap();
    assert_eq!(edid.identity("DP-1"), "DEL-A0B1-sSN42");
}

#[test]
fn two_monitors_with_placeholder_serial_stay_distinct_by_fingerprint_and_connector() {
    let a = Edid::parse(&Edid::build_block(
        "AUS",
        0x24AC,
        0x0101_0101,
        Some("VG248"),
        None,
    ))
    .unwrap();
    let b = Edid::parse(&Edid::build_block(
        "AUS",
        0x27F5,
        0x0101_0101,
        Some("PG27"),
        None,
    ))
    .unwrap();
    assert_ne!(a.identity("DP-1"), b.identity("DP-1"));
}
