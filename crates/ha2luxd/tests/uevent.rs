//! Événements noyau : analyse, filtrage, sondage de sysfs, socket netlink.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Duration;

use ha2lux_core::DisplayKind;
use ha2luxd::hardware::MockHardware;
use ha2luxd::system::{connector_signature, is_display_event, parse_uevent, poll_connectors};
use proptest::prelude::*;
use support::Rig;
use tempfile::TempDir;

fn message(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(part.as_bytes());
        out.push(0);
    }
    out
}

#[test]
fn parses_a_real_drm_hotplug_event() {
    let raw = message(&[
        "change@/devices/pci0000:00/0000:00:01.1/0000:01:00.0/drm/card1",
        "ACTION=change",
        "DEVPATH=/devices/pci0000:00/0000:00:01.1/0000:01:00.0/drm/card1",
        "SUBSYSTEM=drm",
        "HOTPLUG=1",
        "CONNECTOR=84",
        "DEVNAME=/dev/dri/card1",
        "DEVTYPE=drm_minor",
        "SEQNUM=4731",
    ]);
    let event = parse_uevent(&raw).unwrap();
    assert_eq!(event.action, "change");
    assert!(event.devpath.ends_with("drm/card1"));
    assert_eq!(event.subsystem.as_deref(), Some("drm"));
    assert_eq!(event.var("CONNECTOR"), Some("84"));
    assert!(is_display_event(&event));
}

#[test]
fn display_relevant_events() {
    let make = |action: &str, subsystem: &str, extra: &[&str]| {
        let head = format!("{action}@/devices/x");
        let sub = format!("SUBSYSTEM={subsystem}");
        let mut parts = vec![head.as_str(), sub.as_str()];
        parts.extend_from_slice(extra);
        parse_uevent(&message(&parts)).unwrap()
    };
    assert!(is_display_event(&make("change", "drm", &["HOTPLUG=1"])));
    assert!(is_display_event(&make("add", "drm", &[])));
    assert!(is_display_event(&make("remove", "drm", &[])));
    assert!(
        !is_display_event(&make("change", "drm", &[])),
        "un change DRM sans HOTPLUG (ex. propriété) est ignoré"
    );
    assert!(is_display_event(&make("add", "i2c-dev", &[])));
    assert!(is_display_event(&make("remove", "i2c-dev", &[])));
    assert!(!is_display_event(&make("change", "i2c-dev", &[])));
    for other in ["usb", "block", "input", "net", "sound"] {
        assert!(!is_display_event(&make("add", other, &[])), "{other}");
    }
}

#[test]
fn malformed_and_foreign_messages_are_ignored() {
    // Message libudev (groupe 2) : en-tête binaire, pas « action@devpath ».
    let mut libudev = b"libudev\0".to_vec();
    libudev.extend_from_slice(&[0xfe, 0xed, 0xca, 0xfe, 0x28, 0x00, 0x00, 0x00]);
    assert_eq!(parse_uevent(&libudev), None);
    assert_eq!(parse_uevent(b""), None);
    assert_eq!(parse_uevent(b"\0\0\0"), None);
    assert_eq!(parse_uevent(b"change"), None, "pas de @");
    assert_eq!(parse_uevent(b"@/devices/x\0"), None, "action vide");
    assert_eq!(parse_uevent(b"change@devices/x\0"), None, "devpath relatif");
    assert_eq!(
        parse_uevent(b"ch ange@/devices/x\0"),
        None,
        "action avec espace"
    );
    assert_eq!(
        parse_uevent(&[0xff, 0xfe, b'@', b'/']),
        None,
        "UTF-8 invalide"
    );
    // Variables bizarres : ignorées sans casser le reste.
    let event = parse_uevent(&message(&[
        "add@/devices/x",
        "NOEQUALS",
        "SUBSYSTEM=drm",
        "=vide",
        "K=\u{e9}",
    ]))
    .unwrap();
    assert_eq!(event.subsystem.as_deref(), Some("drm"));
}

proptest! {
    #[test]
    fn parser_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        if let Some(event) = parse_uevent(&bytes) {
            let _ = is_display_event(&event);
        }
    }
}

fn connector(root: &std::path::Path, name: &str, status: &str) {
    let dir = root.join(format!("card0-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("status"), status).unwrap();
}

#[test]
fn connector_signature_changes_with_status_and_is_stable_otherwise() {
    let dir = TempDir::new().unwrap();
    connector(dir.path(), "DP-1", "connected\n");
    let first = connector_signature(dir.path());
    assert_eq!(first, connector_signature(dir.path()));
    connector(dir.path(), "DP-1", "disconnected\n");
    assert_ne!(first, connector_signature(dir.path()));
    connector(dir.path(), "DP-1", "connected\n");
    assert_eq!(
        first,
        connector_signature(dir.path()),
        "revenu à l'état initial"
    );
    connector(dir.path(), "HDMI-A-1", "connected\n");
    assert_ne!(
        first,
        connector_signature(dir.path()),
        "un connecteur de plus"
    );
}

#[tokio::test(start_paused = true)]
async fn the_sysfs_poller_triggers_a_rescan_when_a_connector_changes() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    let dir = TempDir::new().unwrap();
    connector(dir.path(), "DP-1", "disconnected\n");
    let poller = tokio::spawn(poll_connectors(
        dir.path().to_owned(),
        rig.handle.clone(),
        Duration::from_secs(3),
    ));
    rig.advance(Duration::from_secs(10)).await;
    let calm = hardware.discoveries();
    rig.advance(Duration::from_secs(30)).await;
    assert_eq!(hardware.discoveries(), calm, "rien ne change : aucun scan");
    connector(dir.path(), "DP-1", "connected\n");
    for _ in 0..40 {
        rig.advance(Duration::from_millis(250)).await;
        tokio::task::yield_now().await;
    }
    assert!(
        hardware.discoveries() > calm,
        "le branchement est détecté par sondage puis débouncé"
    );
    poller.abort();
}

/// Ouvre réellement le socket netlink d'événements noyau (lecture seule, aucun droit requis).
#[tokio::test]
async fn the_kernel_uevent_socket_can_be_opened_and_bound() {
    use netlink_sys::protocols::NETLINK_KOBJECT_UEVENT;
    use netlink_sys::{AsyncSocket, SocketAddr, TokioSocket};
    match TokioSocket::new(NETLINK_KOBJECT_UEVENT) {
        Ok(mut socket) => socket
            .socket_mut()
            .bind(&SocketAddr::new(0, 1))
            .expect("liaison au groupe 1 des événements noyau"),
        Err(error) => {
            eprintln!("SKIPPED : socket netlink indisponible dans cet environnement ({error})")
        }
    }
}
