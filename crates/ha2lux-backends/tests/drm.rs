//! DRM/sysfs discovery on a fake tree (no access to the real system).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ha2lux_backends::drm::{external_candidates, is_internal_connector, scan};
use ha2lux_core::Edid;
use tempfile::TempDir;

/// Creates `card1-<name>` with status, EDID and `ddc` link to `i2c-<bus>`.
fn connector(root: &Path, dir: &str, status: &str, edid: Option<&[u8]>, bus: Option<u32>) {
    let path = root.join(dir);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("status"), format!("{status}\n")).unwrap();
    if let Some(edid) = edid {
        std::fs::write(path.join("edid"), edid).unwrap();
    }
    if let Some(bus) = bus {
        let adapter = root.join(format!("i2c-{bus}"));
        std::fs::create_dir_all(&adapter).unwrap();
        std::os::unix::fs::symlink(&adapter, path.join("ddc")).unwrap();
    }
}

fn edid(serial: u32, name: &str) -> Vec<u8> {
    Edid::build_block("DEL", 0xA0B1, serial, Some(name), None)
}

#[test]
fn missing_directory_gives_an_empty_list() {
    assert!(scan(Path::new("/nonexistent/ha2lux/drm")).is_empty());
}

#[test]
fn scan_ignores_non_connector_entries_and_sorts() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("card1")).unwrap();
    std::fs::create_dir(root.path().join("renderD128")).unwrap();
    std::fs::write(root.path().join("version"), "x").unwrap();
    std::fs::create_dir(root.path().join("card1-")).unwrap();
    std::fs::create_dir(root.path().join("cardX-DP-1")).unwrap();
    connector(root.path(), "card1-HDMI-A-1", "disconnected", None, None);
    connector(
        root.path(),
        "card0-DP-2",
        "connected",
        Some(&edid(1, "A")),
        Some(4),
    );
    connector(
        root.path(),
        "card1-DP-1",
        "connected",
        Some(&edid(2, "B")),
        Some(7),
    );
    let found = scan(root.path());
    let names: Vec<_> = found
        .iter()
        .map(|c| (c.card.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [("card0", "DP-2"), ("card1", "DP-1"), ("card1", "HDMI-A-1")]
    );
    assert_eq!(found[0].ddc_bus, Some(4));
    assert_eq!(found[1].ddc_bus, Some(7));
    assert!(found[0].connected && !found[2].connected);
    assert_eq!(found[1].edid.as_ref().unwrap().name.as_deref(), Some("B"));
}

#[test]
fn internal_connectors_are_recognised() {
    for name in ["eDP-1", "LVDS-1", "DSI-1", "edp-2"] {
        assert!(is_internal_connector(name), "{name}");
    }
    for name in ["DP-1", "HDMI-A-1", "DVI-D-1", "VGA-1", "Virtual-1"] {
        assert!(!is_internal_connector(name), "{name}");
    }
}

#[test]
fn edid_problems_are_reported_not_fatal() {
    let root = TempDir::new().unwrap();
    connector(root.path(), "card0-DP-1", "connected", Some(&[]), Some(3));
    connector(
        root.path(),
        "card0-DP-2",
        "connected",
        Some(&[0u8; 128]),
        Some(4),
    );
    connector(
        root.path(),
        "card0-DP-3",
        "connected",
        Some(&[0u8; 10]),
        Some(5),
    );
    connector(root.path(), "card0-DP-4", "connected", None, Some(6));
    let found = scan(root.path());
    assert!(
        found
            .iter()
            .all(|c| c.edid.is_none() && c.edid_problem.is_some())
    );
    assert_eq!(found[0].edid_problem.as_deref(), Some("EDID vide"));
    assert!(
        external_candidates(&found).is_empty(),
        "sans EDID, pas d'identité : pas piloté"
    );
}

#[test]
fn no_ddc_link_means_no_bus() {
    let root = TempDir::new().unwrap();
    connector(
        root.path(),
        "card0-DP-1",
        "connected",
        Some(&edid(1, "A")),
        None,
    );
    let found = scan(root.path());
    assert_eq!(found[0].ddc_bus, None);
    assert!(external_candidates(&found).is_empty());
}

#[test]
fn candidates_are_only_connected_external_displays_with_bus_and_edid() {
    let root = TempDir::new().unwrap();
    connector(
        root.path(),
        "card0-eDP-1",
        "connected",
        Some(&edid(9, "Panel")),
        Some(2),
    );
    connector(
        root.path(),
        "card0-DP-1",
        "connected",
        Some(&edid(1, "Ext")),
        Some(5),
    );
    connector(
        root.path(),
        "card0-DP-2",
        "disconnected",
        Some(&edid(2, "Gone")),
        Some(6),
    );
    connector(
        root.path(),
        "card0-HDMI-A-1",
        "connected",
        Some(&edid(3, "NoBus")),
        None,
    );
    let found = scan(root.path());
    let candidates = external_candidates(&found);
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(candidates[0].connector, "DP-1");
    assert_eq!(candidates[0].bus, 5);
    assert_eq!(candidates[0].info.name, "Ext");
    assert_eq!(candidates[0].info.max, 0, "maximum fixé par la sonde");
    assert_eq!(candidates[0].info.id.as_str(), "DEL-A0B1-00000001");
}

#[test]
fn t_ddc_08_identity_follows_the_edid_not_the_connector() {
    let a = edid(111, "Left");
    let b = edid(222, "Right");
    let first = TempDir::new().unwrap();
    connector(first.path(), "card0-DP-1", "connected", Some(&a), Some(5));
    connector(first.path(), "card0-DP-2", "connected", Some(&b), Some(6));
    // The two displays swap their enumeration order / their connectors.
    let second = TempDir::new().unwrap();
    connector(second.path(), "card0-DP-1", "connected", Some(&b), Some(8));
    connector(second.path(), "card0-DP-2", "connected", Some(&a), Some(9));
    let before = external_candidates(&scan(first.path()));
    let after = external_candidates(&scan(second.path()));
    let find = |list: &[ha2lux_backends::drm::DdcCandidate], name: &str| {
        list.iter().find(|c| c.info.name == name).cloned().unwrap()
    };
    for name in ["Left", "Right"] {
        let (x, y) = (find(&before, name), find(&after, name));
        assert_eq!(
            x.info.id, y.info.id,
            "{name} garde son identité (donc sa plage)"
        );
        assert_ne!(x.connector, y.connector);
        assert_ne!(x.bus, y.bus, "le bus est ré-associé, jamais mémorisé");
    }
}

#[test]
fn identical_ids_are_disambiguated_by_connector() {
    let root = TempDir::new().unwrap();
    let same = edid(5, "Twin");
    connector(root.path(), "card0-DP-1", "connected", Some(&same), Some(5));
    connector(root.path(), "card0-DP-2", "connected", Some(&same), Some(6));
    let candidates = external_candidates(&scan(root.path()));
    let ids: Vec<_> = candidates
        .iter()
        .map(|c| c.info.id.as_str().to_owned())
        .collect();
    assert_eq!(ids, ["DEL-A0B1-00000005@DP-1", "DEL-A0B1-00000005@DP-2"]);
}
