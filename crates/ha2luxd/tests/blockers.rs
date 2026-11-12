//! Sondes de blocage : analyseurs purs, faux services D-Bus, faux /proc, pw-dump réel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use ha2luxd::blockers::{
    BlockerOptions, SystemBlockers, parse_logind_inhibitors, parse_pw_dump, scan_video_users,
};
use ha2luxd::dbus_idle::MutterIdleTime;
use ha2luxd::idle::{Blockers, IdleTime};
use proptest::prelude::*;
use tempfile::TempDir;

fn inhibitor(
    what: &str,
    who: &str,
    why: &str,
    mode: &str,
    pid: u32,
) -> (String, String, String, String, u32, u32) {
    (what.into(), who.into(), why.into(), mode.into(), 1000, pid)
}

#[test]
fn logind_only_idle_block_inhibitors_count() {
    // Liste relevée sur un poste KDE réel : aucun n'est un inhibiteur d'inactivité.
    let real = vec![
        inhibitor(
            "sleep",
            "code",
            "Application cleanup before suspend",
            "delay",
            16088,
        ),
        inhibitor(
            "sleep",
            "NetworkManager",
            "NetworkManager needs to turn off networks",
            "delay",
            1166,
        ),
        inhibitor(
            "handle-power-key:handle-suspend-key:handle-hibernate-key:handle-lid-switch",
            "PowerDevil",
            "KDE handles power events",
            "block",
            1842,
        ),
        inhibitor(
            "shutdown",
            "Unattended Upgrades Shutdown",
            "Stop ongoing upgrades",
            "delay",
            1308,
        ),
    ];
    assert!(
        parse_logind_inhibitors(&real, 1).is_empty(),
        "pas de faux positif sur PowerDevil & co"
    );

    let list = vec![
        inhibitor("idle", "mpv", "Playing video", "block", 500),
        inhibitor("sleep:idle", "Firefox", "", "block", 501),
        inhibitor("idle", "someone", "delay only", "delay", 502),
        inhibitor("idle:sleep", "ha2luxd", "nous-mêmes", "block", 42),
        inhibitor("idleness", "x", "faux ami", "block", 503),
    ];
    let reasons = parse_logind_inhibitors(&list, 42);
    assert_eq!(
        reasons,
        [
            "idle inhibitor: mpv (Playing video)",
            "idle inhibitor: Firefox"
        ]
    );
}

fn node(class: &str, state: &str, extra: &str) -> String {
    format!(
        r#"{{"id":1,"type":"PipeWire:Interface:Node","info":{{"state":"{state}","props":{{"media.class":"{class}"{extra}}}}}}}"#
    )
}

#[test]
fn pipewire_dump_detects_microphone_and_camera_in_use() {
    let dump = |nodes: &[String]| format!("[{}]", nodes.join(","));
    assert!(
        parse_pw_dump(&dump(&[
            node("Audio/Source", "suspended", ""),
            node("Video/Source", "idle", "")
        ]))
        .is_empty()
    );
    assert_eq!(
        parse_pw_dump(&dump(&[node("Audio/Source", "running", "")])),
        ["microphone in use"]
    );
    assert_eq!(
        parse_pw_dump(&dump(&[node(
            "Stream/Input/Audio",
            "running",
            r#","application.name":"Zoom""#
        )])),
        ["microphone in use"]
    );
    assert_eq!(
        parse_pw_dump(&dump(&[node("Video/Source", "running", "")])),
        ["camera in use"]
    );
    assert_eq!(
        parse_pw_dump(&dump(&[node("Stream/Input/Video", "running", "")])),
        ["camera in use"]
    );
    let both = parse_pw_dump(&dump(&[
        node("Audio/Source", "running", ""),
        node("Video/Source", "running", ""),
    ]));
    assert_eq!(both, ["microphone in use", "camera in use"]);
    // Lecture de musique (sortie) : pas un blocage.
    assert!(
        parse_pw_dump(&dump(&[
            node("Stream/Output/Audio", "running", ""),
            node("Audio/Sink", "running", "")
        ]))
        .is_empty()
    );
    // Vumètre qui écoute le moniteur d'une sortie : pas le microphone.
    assert!(
        parse_pw_dump(&dump(&[node(
            "Stream/Input/Audio",
            "running",
            r#","stream.capture.sink":"true""#
        )]))
        .is_empty()
    );
    assert!(
        parse_pw_dump(&dump(&[node(
            "Stream/Input/Audio",
            "running",
            r#","stream.capture.sink":true"#
        )]))
        .is_empty()
    );
    assert!(
        parse_pw_dump(&dump(&[node(
            "Stream/Input/Audio",
            "running",
            r#","media.category":"Monitor""#
        )]))
        .is_empty()
    );
}

#[test]
fn pipewire_dump_tolerates_garbage() {
    for bad in [
        "",
        "not json",
        "{}",
        "[]",
        "[1,2,3]",
        r#"[{"type":"PipeWire:Interface:Node"}]"#,
        r#"[{"type":"PipeWire:Interface:Node","info":{"state":"running"}}]"#,
        "null",
    ] {
        assert!(parse_pw_dump(bad).is_empty(), "{bad:?}");
    }
}

proptest! {
    #[test]
    fn pw_dump_parser_never_panics(text in ".{0,200}") {
        let _ = parse_pw_dump(&text);
    }
}

#[test]
fn camera_opened_directly_through_v4l2_is_detected_from_proc() {
    let proc = TempDir::new().unwrap();
    let make = |pid: u32, comm: &str, targets: &[&str]| {
        let dir = proc.path().join(pid.to_string());
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        for (i, target) in targets.iter().enumerate() {
            std::os::unix::fs::symlink(target, dir.join("fd").join(i.to_string())).unwrap();
        }
    };
    make(100, "firefox", &["/dev/pts/0", "/home/u/x.txt"]);
    make(200, "obs", &["/dev/video0", "/dev/null"]);
    make(300, "zoom", &["/dev/video1"]);
    make(42, "ha2luxd", &["/dev/video0"]); // nous-mêmes : ignoré
    std::fs::create_dir_all(proc.path().join("self")).unwrap(); // entrée non numérique
    std::fs::write(proc.path().join("version"), "x").unwrap();
    assert_eq!(
        scan_video_users(proc.path(), 42),
        ["camera opened by obs", "camera opened by zoom"]
    );
    assert!(scan_video_users(std::path::Path::new("/nonexistent/proc"), 1).is_empty());
}

struct PrivateBus {
    child: Child,
    address: String,
    _dir: TempDir,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n<busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\n<policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>\n",
                dir.path().display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        let address = address.trim().to_owned();
        (!address.is_empty()).then_some(Self {
            child,
            address,
            _dir: dir,
        })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        // Le bus privé n'a plus d'utilité ; une erreur ici n'a aucune conséquence.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

macro_rules! bus_or_skip {
    () => {
        match PrivateBus::start() {
            Some(bus) => bus,
            None => {
                eprintln!("SKIPPED : dbus-daemon introuvable");
                return;
            }
        }
    };
}

async fn connection(bus: &PrivateBus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}

struct FakeLogind(Vec<(String, String, String, String, u32, u32)>);

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl FakeLogind {
    fn list_inhibitors(&self) -> Vec<(String, String, String, String, u32, u32)> {
        self.0.clone()
    }
}

struct FakePower(bool);

#[zbus::interface(name = "org.freedesktop.PowerManagement.Inhibit")]
impl FakePower {
    fn has_inhibit(&self) -> bool {
        self.0
    }
}

struct FakeGnome(bool);

#[zbus::interface(name = "org.gnome.SessionManager")]
impl FakeGnome {
    fn is_inhibited(&self, flags: u32) -> bool {
        self.0 && flags == 8
    }
}

struct FakeMutter(u64);

#[zbus::interface(name = "org.gnome.Mutter.IdleMonitor")]
impl FakeMutter {
    fn get_idletime(&self) -> u64 {
        self.0
    }
}

async fn serve<I: zbus::object_server::Interface>(
    bus: &PrivateBus,
    name: &str,
    path: &str,
    iface: I,
) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(path, iface)
        .unwrap()
        .name(name.to_owned())
        .unwrap()
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn logind_inhibitors_are_read_over_dbus() {
    let bus = bus_or_skip!();
    let _logind = serve(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        FakeLogind(vec![inhibitor("idle", "mpv", "Playing", "block", 500)]),
    )
    .await;
    let mut blockers = SystemBlockers::new(BlockerOptions {
        system: Some(connection(&bus).await),
        ..BlockerOptions::default()
    });
    let report = blockers.probe().await;
    assert!(report.blocked());
    assert_eq!(report.reasons, ["idle inhibitor: mpv (Playing)"]);
}

#[tokio::test]
async fn desktop_inhibitors_are_read_over_the_session_bus() {
    let bus = bus_or_skip!();
    let _power = serve(
        &bus,
        "org.freedesktop.PowerManagement.Inhibit",
        "/org/freedesktop/PowerManagement/Inhibit",
        FakePower(true),
    )
    .await;
    let _gnome = serve(
        &bus,
        "org.gnome.SessionManager",
        "/org/gnome/SessionManager",
        FakeGnome(true),
    )
    .await;
    let mut blockers = SystemBlockers::new(BlockerOptions {
        session: Some(connection(&bus).await),
        ..BlockerOptions::default()
    });
    let reasons = blockers.probe().await.reasons;
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(
        reasons.iter().any(|r| r.contains("PowerManagement"))
            && reasons.iter().any(|r| r.contains("GNOME"))
    );
}

#[tokio::test]
async fn missing_services_are_silently_ignored() {
    let bus = bus_or_skip!();
    let conn = connection(&bus).await; // aucun service sur ce bus
    let mut blockers = SystemBlockers::new(BlockerOptions {
        system: Some(conn.clone()),
        session: Some(conn),
        pipewire: true,
        pw_dump: Some("/nonexistent/pw-dump".into()),
        proc_dir: Some("/nonexistent/proc".into()),
    });
    assert!(
        !blockers.probe().await.blocked(),
        "aucune information n'est pas un blocage"
    );
    assert!(
        !blockers.probe().await.blocked(),
        "un pw-dump absent n'est pas relancé en boucle"
    );
    let mut idle = SystemBlockers::new(BlockerOptions::default());
    assert!(!idle.probe().await.blocked());
}

#[tokio::test]
async fn pw_dump_output_is_parsed_from_a_real_process() {
    // Faux `pw-dump` : un script qui affiche un microphone actif (sans toucher au vrai PipeWire).
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("pw-dump");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat <<'EOF'\n[{}]\nEOF\n",
            node("Audio/Source", "running", "")
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut blockers = SystemBlockers::new(BlockerOptions {
        pipewire: true,
        pw_dump: Some(script),
        ..BlockerOptions::default()
    });
    assert_eq!(blockers.probe().await.reasons, ["microphone in use"]);
}

#[tokio::test]
async fn a_hanging_pw_dump_does_not_block_the_probe() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("pw-dump");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut blockers = SystemBlockers::new(BlockerOptions {
        pipewire: true,
        pw_dump: Some(script),
        ..BlockerOptions::default()
    });
    let started = std::time::Instant::now();
    assert!(!blockers.probe().await.blocked());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(6),
        "sonde bornée dans le temps : {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn mutter_idle_time_is_read_over_dbus() {
    let bus = bus_or_skip!();
    let _mutter = serve(
        &bus,
        "org.gnome.Mutter.IdleMonitor",
        "/org/gnome/Mutter/IdleMonitor/Core",
        FakeMutter(123_456),
    )
    .await;
    let mut idle = MutterIdleTime::new(connection(&bus).await);
    assert_eq!(idle.idle_ms().await, Some(123_456));
}

#[tokio::test]
async fn mutter_absent_gives_no_value() {
    let bus = bus_or_skip!();
    let mut idle = MutterIdleTime::new(connection(&bus).await);
    assert_eq!(idle.idle_ms().await, None);
}

/// `pw-dump` réel de la session (lecture seule) : le résultat doit être cohérent, sans panique.
#[tokio::test]
async fn the_real_pw_dump_if_present_is_parsed_without_error() {
    let Ok(output) = tokio::process::Command::new("pw-dump").output().await else {
        eprintln!("SKIPPED : pw-dump absent");
        return;
    };
    if !output.status.success() {
        eprintln!("SKIPPED : PipeWire ne répond pas");
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let reasons = parse_pw_dump(&text);
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_ok(),
        "pw-dump doit produire du JSON valide"
    );
    println!("blocages PipeWire actuels : {reasons:?}");
}
