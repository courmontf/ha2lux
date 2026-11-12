//! Le backend factice respecte le contrat `DisplayBackend`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use ha2lux_backends::mock::MockBackend;
use ha2lux_core::{BackendError, DisplayBackend, DisplayKind};
use support::block_on;

#[test]
fn probe_read_write_round_trip() {
    let (mut backend, control) = MockBackend::new("X", DisplayKind::External, 255, 128);
    assert_eq!(block_on(backend.probe()).unwrap().max, 255);
    assert_eq!(block_on(backend.read()).unwrap(), 128);
    block_on(backend.write(200)).unwrap();
    assert_eq!(control.raw(), 200);
    assert_eq!(control.writes(), [200]);
    assert_eq!(control.reads(), 1);
}

#[test]
fn max_zero_is_not_controllable() {
    let (mut backend, _) = MockBackend::new("X", DisplayKind::External, 0, 0);
    assert!(matches!(
        block_on(backend.probe()),
        Err(BackendError::Invalid(_))
    ));
}

#[test]
fn rejects_value_above_max() {
    let (mut backend, control) = MockBackend::new("X", DisplayKind::External, 100, 0);
    assert!(block_on(backend.write(101)).is_err());
    assert!(control.writes().is_empty());
}

#[test]
fn scripted_failures_then_recovery() {
    let (mut backend, control) = MockBackend::new("X", DisplayKind::External, 100, 0);
    control.fail_next_writes(2, BackendError::Transient("occupé".to_owned()));
    assert!(block_on(backend.write(10)).unwrap_err().is_transient());
    assert!(block_on(backend.write(10)).is_err());
    block_on(backend.write(10)).unwrap();
    control.fail_always(Some(BackendError::Disconnected("débranché".to_owned())));
    let error = block_on(backend.write(20)).unwrap_err();
    assert!(!error.is_transient());
    control.fail_always(None);
    block_on(backend.write(20)).unwrap();
    assert_eq!(control.writes(), [10, 20]);
}

#[test]
fn error_classification() {
    assert!(BackendError::Timeout.is_transient());
    assert!(!BackendError::PermissionDenied("udev".into()).is_transient());
    assert!(!BackendError::Invalid("x".into()).is_transient());
}
