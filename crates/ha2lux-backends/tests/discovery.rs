//! Display to bus association by EDID (proprietary NVIDIA): safety guard rails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::Path;

use ha2lux_backends::discovery::{BusSource, discover_external, list_adapters};
use ha2lux_core::Edid;
use support::monitor::FakeOpener;
use tempfile::TempDir;

fn connector(root: &Path, name: &str, status: &str, edid: &[u8]) {
    let dir = root.join("sys/class/drm").join(format!("card0-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("status"), format!("{status}\n")).unwrap();
    std::fs::write(dir.join("edid"), edid).unwrap();
}

fn adapter(root: &Path, bus: u32, name: &str) {
    let dir = root.join(format!("sys/class/i2c-dev/i2c-{bus}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
}

fn edid(serial: u32, name: &str) -> Vec<u8> {
    Edid::build_block("AUS", 0x24AC, serial, Some(name), None)
}

/// NVIDIA machine: displays without a `ddc` link, GPU adapters, and buses that are NOT display buses.
fn nvidia_system() -> (TempDir, std::sync::Arc<FakeOpener>, Vec<u8>, Vec<u8>) {
    let root = TempDir::new().unwrap();
    let (a, b) = (edid(111, "Left"), edid(222, "Right"));
    connector(root.path(), "DP-1", "connected", &a);
    connector(root.path(), "HDMI-A-1", "connected", &b);
    connector(root.path(), "DP-2", "disconnected", &[]);
    adapter(root.path(), 1, "NVIDIA i2c adapter 1 at 1:00.0");
    adapter(root.path(), 2, "NVIDIA i2c adapter 2 at 1:00.0");
    adapter(root.path(), 3, "NVIDIA i2c adapter 3 at 1:00.0");
    adapter(root.path(), 4, "SMBus I801 adapter at f000");
    adapter(root.path(), 5, "Synopsys DesignWare I2C adapter");
    let opener = std::sync::Arc::new(FakeOpener::default());
    opener.add_edid(1, b.clone()); // HDMI-A-1 is on bus 1
    opener.add_edid(2, a.clone()); // DP-1 is on bus 2
    opener.add_edid(4, b.clone()); // trap: an SMBus bus that would answer... if it were opened
    opener.add_edid(5, a.clone());
    (root, opener, a, b)
}

#[test]
fn lists_adapters_with_names_sorted() {
    let (root, _, _, _) = nvidia_system();
    let adapters = list_adapters(&root.path().join("sys/class/i2c-dev"));
    let buses: Vec<u32> = adapters.iter().map(|a| a.bus).collect();
    assert_eq!(buses, [1, 2, 3, 4, 5]);
    assert!(adapters[0].is_gpu_ddc() && adapters[2].is_gpu_ddc());
    assert!(!adapters[3].is_gpu_ddc() && !adapters[4].is_gpu_ddc());
    assert!(list_adapters(Path::new("/nonexistent/x")).is_empty());
}

#[test]
fn without_explicit_request_no_i2c_transaction_happens() {
    let (root, opener, _, _) = nvidia_system();
    let discovery = discover_external(root.path(), opener.as_ref(), false);
    assert!(
        discovery.candidates.is_empty(),
        "sans lien ddc, rien n'est pilotable sans résolution"
    );
    assert!(
        opener.opened().is_empty(),
        "aucun bus ouvert sans demande explicite"
    );
}

#[test]
fn edid_matching_resolves_buses_and_only_touches_gpu_adapters() {
    let (root, opener, _, _) = nvidia_system();
    let discovery = discover_external(root.path(), opener.as_ref(), true);
    let mut found: Vec<_> = discovery
        .candidates
        .iter()
        .map(|(c, source)| (c.connector.clone(), c.bus, *source, c.info.name.clone()))
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        found,
        [
            (
                "DP-1".to_owned(),
                2,
                BusSource::EdidOverI2c,
                "Left".to_owned()
            ),
            (
                "HDMI-A-1".to_owned(),
                1,
                BusSource::EdidOverI2c,
                "Right".to_owned()
            ),
        ]
    );
    let opened = opener.opened();
    assert!(
        opened
            .iter()
            .all(|(bus, address)| [1, 2, 3].contains(bus) && *address == 0x50),
        "{opened:?}"
    );
    assert!(
        opened.iter().all(|(bus, _)| *bus != 4 && *bus != 5),
        "jamais SMBus ni capteurs"
    );
}

#[test]
fn drm_linked_displays_are_not_probed_over_i2c() {
    let root = TempDir::new().unwrap();
    let e = edid(1, "Linked");
    connector(root.path(), "DP-1", "connected", &e);
    let adapter_dir = root.path().join("sys/class/drm/i2c-9");
    std::fs::create_dir_all(&adapter_dir).unwrap();
    std::os::unix::fs::symlink(
        &adapter_dir,
        root.path().join("sys/class/drm/card0-DP-1/ddc"),
    )
    .unwrap();
    adapter(root.path(), 3, "NVIDIA i2c adapter 3 at 1:00.0");
    let opener = FakeOpener::default();
    let discovery = discover_external(root.path(), &opener, true);
    assert_eq!(discovery.candidates.len(), 1);
    assert_eq!(
        (discovery.candidates[0].0.bus, discovery.candidates[0].1),
        (9, BusSource::Drm)
    );
    assert!(
        opener.opened().is_empty(),
        "rien à résoudre : aucune lecture d'EDID"
    );
}

#[test]
fn ambiguous_identical_edids_are_refused() {
    let root = TempDir::new().unwrap();
    let twin = edid(7, "Twin");
    connector(root.path(), "DP-1", "connected", &twin);
    connector(root.path(), "DP-2", "connected", &twin);
    adapter(root.path(), 1, "NVIDIA i2c adapter 1 at 1:00.0");
    adapter(root.path(), 2, "NVIDIA i2c adapter 2 at 1:00.0");
    let opener = FakeOpener::default();
    opener.add_edid(1, twin.clone());
    opener.add_edid(2, twin);
    let discovery = discover_external(root.path(), &opener, true);
    assert!(
        discovery.candidates.is_empty(),
        "association au hasard interdite"
    );
    assert_eq!(
        discovery
            .notes
            .iter()
            .filter(|n| n.contains("ambiguous"))
            .count(),
        2
    );
}

#[test]
fn unmatched_connector_is_reported_and_internal_panels_are_ignored() {
    let root = TempDir::new().unwrap();
    connector(root.path(), "DP-1", "connected", &edid(1, "Nobody"));
    connector(root.path(), "eDP-1", "connected", &edid(2, "Panel"));
    adapter(root.path(), 1, "NVIDIA i2c adapter 1 at 1:00.0");
    let opener = FakeOpener::default();
    opener.add_edid(1, edid(99, "Other"));
    let discovery = discover_external(root.path(), &opener, true);
    assert!(discovery.candidates.is_empty());
    assert!(
        discovery
            .notes
            .iter()
            .any(|n| n.starts_with("DP-1") && n.contains("no GPU adapter")),
        "{:?}",
        discovery.notes
    );
    assert!(
        !discovery.notes.iter().any(|n| n.starts_with("eDP-1")),
        "un panneau interne n'est jamais cherché en DDC"
    );
}

#[test]
fn invalid_edid_on_a_gpu_bus_is_noted_not_fatal() {
    let root = TempDir::new().unwrap();
    connector(root.path(), "DP-1", "connected", &edid(1, "A"));
    adapter(root.path(), 1, "NVIDIA i2c adapter 1 at 1:00.0");
    let opener = FakeOpener::default();
    opener.add_edid(1, vec![0xAB; 128]);
    let discovery = discover_external(root.path(), &opener, true);
    assert!(discovery.candidates.is_empty());
    assert!(discovery.notes.iter().any(|n| n.contains("invalid EDID")));
}
