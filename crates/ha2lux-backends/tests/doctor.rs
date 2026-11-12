//! `doctor` sur un faux système de fichiers, avec un faux moniteur pour la sonde.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::Path;
use std::sync::Arc;

use ha2lux_backends::ddc::DdcTimings;
use ha2lux_backends::doctor::{DoctorOptions, Report, Status, run};
use ha2lux_core::Edid;
use support::monitor::{FakeOpener, MonitorControl};
use tempfile::TempDir;

fn put(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Poste sain : module, un nœud accessible, un écran externe sur i2c-7, règle udev, groupe.
fn healthy_system() -> TempDir {
    let root = TempDir::new().unwrap();
    let r = root.path();
    std::fs::create_dir_all(r.join("sys/module/i2c_dev")).unwrap();
    put(r, "dev/i2c-7", "");
    let edid = Edid::build_block("DEL", 0xA0B1, 0x1234_5678, Some("DELL U2720Q"), None);
    let dir = r.join("sys/class/drm/card0-DP-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("status"), "connected\n").unwrap();
    std::fs::write(dir.join("edid"), edid).unwrap();
    let adapter = r.join("sys/class/drm/i2c-7");
    std::fs::create_dir_all(&adapter).unwrap();
    std::os::unix::fs::symlink(&adapter, dir.join("ddc")).unwrap();
    put(
        r,
        "etc/udev/rules.d/60-ha2lux-i2c.rules",
        "SUBSYSTEM==\"i2c-dev\", KERNEL==\"i2c-[0-9]*\", TAG+=\"uaccess\"\n",
    );
    put(r, "etc/group", "root:x:0:\ni2c:x:105:alice\n");
    put(
        r,
        "proc/self/status",
        "Name:\tx\nGid:\t1000\t1000\t1000\t1000\nGroups:\t24 105 1000\n",
    );
    root
}

fn options(root: &Path, probe: bool) -> DoctorOptions {
    DoctorOptions {
        root: root.to_owned(),
        probe,
        session_id: Some("3".to_owned()),
        opener: None,
        timings: DdcTimings::none(),
    }
}

fn status_of(report: &Report, id: &str) -> Status {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("contrôle {id} absent"))
        .status
}

#[tokio::test]
async fn healthy_system_is_clean_and_read_only() {
    let root = healthy_system();
    let report = run(&options(root.path(), false)).await;
    assert_eq!(report.exit_code(), 0, "{}", report.render_text());
    for id in [
        "module-i2c-dev",
        "noeuds-i2c",
        "acces-i2c",
        "regle-udev",
        "drm",
    ] {
        assert_eq!(status_of(&report, id), Status::Ok, "{id}");
    }
    assert_eq!(status_of(&report, "groupe-i2c"), Status::Info);
    assert!(report.checks.iter().any(|c| c.id == "groupe-i2c"
        && c.summary.contains("membre")
        && !c.summary.contains("non membre")));
    assert_eq!(report.connectors.len(), 1);
    let c = &report.connectors[0];
    assert_eq!(
        (c.name.as_str(), c.ddc_bus, c.bus_access),
        ("DP-1", Some(7), Some(true))
    );
    assert_eq!(c.edid.as_ref().unwrap().id, "DEL-A0B1-12345678");
    assert!(
        report.probes.is_empty(),
        "aucune sonde sans demande explicite"
    );
    assert_eq!(status_of(&report, "sonde-ddc"), Status::Info);
}

#[tokio::test]
async fn missing_module_and_nodes_fail_with_a_hint() {
    let root = TempDir::new().unwrap();
    let report = run(&options(root.path(), false)).await;
    assert_eq!(report.exit_code(), 3);
    assert_eq!(status_of(&report, "module-i2c-dev"), Status::Fail);
    assert_eq!(status_of(&report, "noeuds-i2c"), Status::Fail);
    let hint = report
        .checks
        .iter()
        .find(|c| c.id == "module-i2c-dev")
        .unwrap()
        .hint
        .clone()
        .unwrap();
    assert!(hint.contains("modprobe i2c-dev"), "{hint}");
    assert_eq!(status_of(&report, "regle-udev"), Status::Warn);
}

#[tokio::test]
async fn unreadable_bus_node_for_a_connected_display_fails() {
    use std::os::unix::fs::PermissionsExt;
    let root = healthy_system();
    let node = root.path().join("dev/i2c-7");
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&node)
        .is_ok()
    {
        return; // exécuté en root : les permissions n'ont pas d'effet
    }
    let report = run(&options(root.path(), false)).await;
    assert_eq!(status_of(&report, "acces-i2c"), Status::Fail);
    assert_eq!(report.exit_code(), 3);
    assert_eq!(report.connectors[0].bus_access, Some(false));
    let hint = report
        .checks
        .iter()
        .find(|c| c.id == "acces-i2c")
        .unwrap()
        .hint
        .clone()
        .unwrap();
    assert!(hint.contains("60-ha2lux-i2c.rules"), "{hint}");
}

#[tokio::test]
async fn external_display_without_ddc_bus_is_a_warning() {
    let root = healthy_system();
    std::fs::remove_file(root.path().join("sys/class/drm/card0-DP-1/ddc")).unwrap();
    let report = run(&options(root.path(), false)).await;
    assert_eq!(status_of(&report, "ddc-connecteurs"), Status::Warn);
    assert!(
        report.connectors[0]
            .notes
            .iter()
            .any(|n| n.contains("aucun bus DDC"))
    );
}

#[tokio::test]
async fn display_without_edid_is_flagged() {
    let root = healthy_system();
    std::fs::write(root.path().join("sys/class/drm/card0-DP-1/edid"), b"").unwrap();
    let report = run(&options(root.path(), false)).await;
    assert_eq!(status_of(&report, "edid"), Status::Warn);
}

#[tokio::test]
async fn ddcci_driver_and_nvidia_are_reported() {
    let root = healthy_system();
    std::fs::create_dir_all(root.path().join("sys/module/ddcci")).unwrap();
    std::fs::create_dir_all(root.path().join("proc/driver/nvidia")).unwrap();
    let report = run(&options(root.path(), false)).await;
    assert_eq!(status_of(&report, "pilote-ddcci"), Status::Warn);
    assert_eq!(status_of(&report, "pilote-nvidia"), Status::Info);
}

#[tokio::test]
async fn backlight_and_session_checks() {
    let root = healthy_system();
    let panel = root.path().join("sys/class/backlight/intel_backlight");
    std::fs::create_dir_all(&panel).unwrap();
    for (file, value) in [
        ("type", "raw"),
        ("max_brightness", "96000"),
        ("brightness", "48000"),
    ] {
        std::fs::write(panel.join(file), format!("{value}\n")).unwrap();
    }
    let report = run(&options(root.path(), false)).await;
    assert_eq!(status_of(&report, "backlight"), Status::Ok);
    assert_eq!(status_of(&report, "session-logind"), Status::Ok);
    assert_eq!(report.backlights[0].current, Some(48000));

    let mut no_session = options(root.path(), false);
    no_session.session_id = None;
    let report = run(&no_session).await;
    assert_eq!(status_of(&report, "session-logind"), Status::Warn);
}

#[tokio::test]
async fn internal_panel_without_backlight_warns() {
    let root = healthy_system();
    let dir = root.path().join("sys/class/drm/card0-eDP-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("status"), "connected\n").unwrap();
    let report = run(&options(root.path(), false)).await;
    assert_eq!(status_of(&report, "backlight"), Status::Warn);
}

#[tokio::test]
async fn probe_reports_max_and_level_through_a_fake_monitor() {
    let root = healthy_system();
    let monitor = MonitorControl::new(128, 255);
    let mut opts = options(root.path(), true);
    opts.opener = Some(FakeOpener::with_monitor(7, monitor.clone()) as _);
    let report = run(&opts).await;
    assert_eq!(report.probes.len(), 1);
    let probe = &report.probes[0];
    assert!(probe.ok, "{probe:?}");
    assert_eq!(
        (probe.max, probe.current, probe.level),
        (Some(255), Some(128), Some(50))
    );
    assert_eq!(status_of(&report, "sonde-ddc"), Status::Ok);
    assert!(
        monitor.set_values().is_empty(),
        "le diagnostic n'écrit jamais de luminosité"
    );
    assert!(report.render_text().contains("max 255, courant 128 = 50 %"));
}

#[tokio::test]
async fn failing_probe_is_a_failure_with_advice() {
    let root = healthy_system();
    let monitor = MonitorControl::new(10, 100);
    monitor.with(|s| s.null_messages = 100);
    let mut opts = options(root.path(), true);
    opts.opener = Some(FakeOpener::with_monitor(7, monitor) as _);
    let report = run(&opts).await;
    assert!(!report.probes[0].ok);
    assert_eq!(status_of(&report, "sonde-ddc"), Status::Fail);
    assert_eq!(report.exit_code(), 3);
    let hint = report
        .checks
        .iter()
        .find(|c| c.id == "sonde-ddc")
        .unwrap()
        .hint
        .clone()
        .unwrap();
    assert!(hint.contains("OSD"), "{hint}");
}

#[tokio::test]
async fn probe_with_unopenable_bus_reports_the_error() {
    let root = healthy_system();
    let opener = Arc::new(FakeOpener::default()); // aucun moniteur : ouverture impossible
    let mut opts = options(root.path(), true);
    opts.opener = Some(opener as _);
    let report = run(&opts).await;
    assert!(!report.probes[0].ok);
    assert!(report.probes[0].error.is_some());
}

#[tokio::test]
async fn json_output_is_valid_and_complete() {
    let root = healthy_system();
    let report = run(&options(root.path(), false)).await;
    let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
    assert!(
        json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "module-i2c-dev" && c["status"] == "ok")
    );
    assert_eq!(json["connectors"][0]["name"], "DP-1");
    assert_eq!(json["connectors"][0]["edid"]["manufacturer"], "DEL");
    assert!(json["probes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn text_output_lists_checks_hints_and_connectors() {
    let root = TempDir::new().unwrap();
    let text = run(&options(root.path(), false)).await.render_text();
    assert!(text.contains("[FAIL] module-i2c-dev"), "{text}");
    assert!(
        text.contains("→ à lancer vous-même : sudo modprobe i2c-dev"),
        "{text}"
    );
    let healthy = healthy_system();
    let text = run(&options(healthy.path(), false)).await.render_text();
    assert!(
        text.contains("DP-1 : connecté, externe, i2c-7 (accès ok)"),
        "{text}"
    );
    assert!(text.contains("DEL-A0B1-12345678"), "{text}");
}

/// Diagnostic du système réel, en lecture seule (sans `--probe`). Ignoré par défaut.
#[tokio::test]
#[ignore = "lit le système réel ; à lancer à la main avec --ignored"]
async fn real_system_smoke_without_probe() {
    let report = run(&DoctorOptions::system(false)).await;
    assert!(!report.checks.is_empty());
    assert!(report.probes.is_empty());
    println!("{}", report.render_text());
}

/// Poste NVIDIA : l'écran n'a pas de lien ddc ; son bus est trouvé par EDID sous --probe.
fn nvidia_root() -> (TempDir, Vec<u8>) {
    let root = healthy_system();
    std::fs::remove_file(root.path().join("sys/class/drm/card0-DP-1/ddc")).unwrap();
    std::fs::create_dir_all(root.path().join("proc/driver/nvidia")).unwrap();
    put(
        root.path(),
        "sys/class/i2c-dev/i2c-7/name",
        "NVIDIA i2c adapter 7 at 1:00.0\n",
    );
    put(
        root.path(),
        "sys/class/i2c-dev/i2c-8/name",
        "SMBus I801 adapter at f000\n",
    );
    put(root.path(), "dev/i2c-8", "");
    let edid = Edid::build_block("DEL", 0xA0B1, 0x1234_5678, Some("DELL U2720Q"), None);
    (root, edid)
}

#[tokio::test]
async fn nvidia_without_probe_lists_buses_and_touches_no_bus() {
    let (root, edid) = nvidia_root();
    let opener = Arc::new(FakeOpener::default());
    opener.add_edid(7, edid);
    opener.add(7, MonitorControl::new(10, 100));
    let mut opts = options(root.path(), false);
    opts.opener = Some(Arc::clone(&opener) as _);
    let report = run(&opts).await;
    assert!(
        opener.opened().is_empty(),
        "aucune transaction I²C sans --probe"
    );
    assert_eq!(status_of(&report, "ddc-connecteurs"), Status::Warn);
    let hint = report
        .checks
        .iter()
        .find(|c| c.id == "ddc-connecteurs")
        .unwrap()
        .hint
        .clone()
        .unwrap();
    assert!(hint.contains("--probe"), "{hint}");
    let gpu: Vec<_> = report
        .buses
        .iter()
        .filter(|b| b.gpu_ddc)
        .map(|b| b.bus)
        .collect();
    assert_eq!(gpu, [7]);
    assert!(
        report
            .render_text()
            .contains("[bus DDC de carte graphique]")
    );
}

#[tokio::test]
async fn nvidia_probe_resolves_the_bus_by_edid_then_reads_brightness() {
    let (root, edid) = nvidia_root();
    let monitor = MonitorControl::new(64, 100);
    let opener = Arc::new(FakeOpener::default());
    opener.add_edid(7, edid);
    opener.add(7, monitor.clone());
    let mut opts = options(root.path(), true);
    opts.opener = Some(Arc::clone(&opener) as _);
    let report = run(&opts).await;
    assert_eq!(report.probes.len(), 1, "{}", report.render_text());
    let probe = &report.probes[0];
    assert!(probe.ok, "{probe:?}");
    assert_eq!((probe.bus, probe.bus_source.as_str()), (7, "edid-i2c"));
    assert_eq!(
        (probe.max, probe.current, probe.level),
        (Some(100), Some(64), Some(64))
    );
    assert!(
        opener.opened().iter().all(|(bus, _)| *bus == 7),
        "le bus SMBus i2c-8 n'est jamais ouvert"
    );
    assert!(
        monitor.set_values().is_empty(),
        "le diagnostic n'écrit jamais de luminosité"
    );
}
