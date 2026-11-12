//! Shared D-Bus contract: signatures and names must not drift without this test saying so.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ha2lux_ipc::{BUS_NAME, DisplayDto, ERROR_PREFIX, ErrorKind, INTERFACE, OBJECT_PATH, classify};
use zbus::zvariant::Type;

#[test]
fn display_dto_signature_is_stable() {
    // id, name, kind, state: s; level: i (−1 if unknown); low, high, max_raw: u
    assert_eq!(DisplayDto::SIGNATURE.to_string(), "(ssssiuuu)");
}

#[test]
fn well_known_names() {
    assert_eq!(
        (BUS_NAME, OBJECT_PATH, INTERFACE),
        (
            "org.ha2lux.Daemon",
            "/org/ha2lux/Daemon",
            "org.ha2lux.Daemon1"
        )
    );
    assert_eq!(ERROR_PREFIX, "org.ha2lux.Error");
}

#[test]
fn non_method_errors_are_not_daemon_errors() {
    assert_eq!(classify(&zbus::Error::InterfaceNotFound), None);
    assert_eq!(classify(&zbus::Error::Failure("x".into())), None);
    let _ = ErrorKind::Usage;
}
