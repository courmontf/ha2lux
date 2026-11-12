//! T-CLI-10..13: display selection.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ha2lux_cli::select::{SelectError, select};
use ha2lux_ipc::DisplayDto;

fn d(id: &str, name: &str) -> DisplayDto {
    DisplayDto {
        id: id.into(),
        name: name.into(),
        kind: "external".into(),
        state: "controllable".into(),
        level: 50,
        low: 0,
        high: 100,
        max_raw: 100,
    }
}

fn three() -> Vec<DisplayDto> {
    vec![
        d("DEL-1", "DELL U2720Q"),
        d("AUS-1", "ASUS VG248"),
        d("AUS-2", "ASUS PG27"),
    ]
}

#[test]
fn t_cli_10_number_is_the_position_in_the_list() {
    let list = three();
    let (n, display) = select(&list, "2").unwrap();
    assert_eq!((n, display.id.as_str()), (2, "AUS-1"));
    assert_eq!(select(&list, "1").unwrap().1.id, "DEL-1");
    assert_eq!(select(&list, " 3 ").unwrap().0, 3);
    for bad in ["0", "4", "99", "00"] {
        let error = select(&list, bad).unwrap_err();
        assert!(
            matches!(error, SelectError::OutOfRange { count: 3, .. }),
            "{bad} → {error:?}"
        );
    }
    assert!(select(&[], "1").is_err());
}

#[test]
fn t_cli_11_exact_name_is_case_insensitive() {
    let list = three();
    assert_eq!(select(&list, "dell u2720q").unwrap().0, 1);
    assert_eq!(select(&list, "Asus VG248").unwrap().0, 2);
    assert_eq!(
        select(&list, "ASUS PG27").unwrap().0,
        3,
        "le nom exact l'emporte sur la sous-chaîne"
    );
}

#[test]
fn t_cli_12_ambiguous_exact_name_lists_the_numbers() {
    let list = vec![d("A-1", "Twin"), d("B-1", "Other"), d("A-2", "Twin")];
    let error = select(&list, "twin").unwrap_err();
    assert_eq!(
        error,
        SelectError::Ambiguous {
            spec: "twin".into(),
            numbers: vec![1, 3]
        }
    );
    assert!(error.to_string().contains("n°1") && error.to_string().contains("n°3"));
}

#[test]
fn t_cli_13_substring_unique_multiple_none() {
    let list = three();
    assert_eq!(select(&list, "dell").unwrap().0, 1);
    assert_eq!(select(&list, "vg2").unwrap().0, 2);
    let many = select(&list, "asus").unwrap_err();
    assert_eq!(
        many,
        SelectError::Ambiguous {
            spec: "asus".into(),
            numbers: vec![2, 3]
        }
    );
    assert_eq!(
        select(&list, "xyz").unwrap_err(),
        SelectError::NotFound("xyz".into())
    );
}

#[test]
fn identity_is_also_accepted() {
    let list = three();
    assert_eq!(select(&list, "del-1").unwrap().0, 1);
    assert_eq!(select(&list, "AUS-2").unwrap().0, 3);
}

#[test]
fn numeric_looking_names_are_numbers_first() {
    let list = vec![d("X", "27"), d("Y", "Other")];
    assert_eq!(
        select(&list, "2").unwrap().0,
        2,
        "un nombre est une position, jamais un nom"
    );
    assert!(select(&list, "27").is_err());
}
