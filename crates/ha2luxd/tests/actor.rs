//! Démon (acteur + tâches d'écran + persistance) sur matériel factice, temps virtuel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Duration;

use ha2lux_backends::mock::MockControl;
use ha2lux_core::{DisplayKind, Event, MinuteOfDay};
use ha2lux_ipc::DisplayDto;
use ha2luxd::actor::Signal;
use ha2luxd::api::{ApiError, Reply, Request};
use ha2luxd::hardware::MockHardware;
use support::{PLANNING_CONFIG, Rig, wait_file, write_file};

async fn list(rig: &Rig) -> Vec<DisplayDto> {
    match rig.ask(Request::List).await.unwrap() {
        Reply::List(list) => list,
        other => panic!("réponse inattendue : {other:?}"),
    }
}

async fn rig_with_control(config: Option<&str>, state: Option<&str>) -> (Rig, MockControl) {
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
    (
        Rig::start_with(hardware, config, state).await.unwrap(),
        control,
    )
}

#[tokio::test(start_paused = true)]
async fn startup_discovers_displays_and_reconstructs_master() {
    let (rig, _) = rig_with_control(None, None).await;
    let displays = list(&rig).await;
    assert_eq!(displays.len(), 1);
    let a = &displays[0];
    assert_eq!(
        (a.id.as_str(), a.state.as_str(), a.kind.as_str()),
        ("A", "controllable", "external")
    );
    assert_eq!((a.level, a.low, a.high, a.max_raw), (50, 0, 100, 100));
    let snapshot = rig.handle.snapshot();
    assert_eq!(
        snapshot.master,
        Some(50),
        "reconstruit depuis le niveau lu, sans rien écrire"
    );
    assert_eq!(snapshot.mode, "static");
    assert!(!snapshot.version.is_empty());
}

#[tokio::test(start_paused = true)]
async fn set_master_reaches_the_hardware_through_the_display_task() {
    let (rig, control) = rig_with_control(None, None).await;
    rig.ok(Request::SetMaster(40)).await;
    assert_eq!(control.raw(), 40);
    assert_eq!(rig.handle.snapshot().master, Some(40));
    let Reply::Status(status) = rig.ask(Request::Status).await.unwrap() else {
        panic!()
    };
    assert!(status.writes_ok >= 1, "compteurs observables : {status:?}");
    assert_eq!(status.displays.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn max_255_display_gets_rounded_raw_values() {
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 255, 50);
    let rig = Rig::start_with(hardware, None, None).await.unwrap();
    rig.ok(Request::SetMaster(50)).await;
    assert_eq!(control.raw(), 128, "T-HW-02 : 127,5 arrondi au plus proche");
    rig.ok(Request::SetMaster(100)).await;
    assert_eq!(control.raw(), 255);
}

#[tokio::test(start_paused = true)]
async fn t_sec_01_values_out_of_bounds_are_rejected_by_the_daemon_without_effect() {
    let (rig, control) = rig_with_control(None, None).await;
    rig.ok(Request::SetMaster(40)).await;
    let before = rig.handle.snapshot();
    let writes_before = control.writes().len();
    let long_id = "x".repeat(300);
    let rejected = [
        (Request::SetMaster(101), "OutOfRange"),
        (Request::SetMaster(u32::MAX), "OutOfRange"),
        (Request::StepMaster(0), "OutOfRange"),
        (Request::StepMaster(101), "OutOfRange"),
        (Request::StepMaster(-101), "OutOfRange"),
        (Request::StepMaster(i32::MIN), "OutOfRange"),
        (
            Request::SetDisplay {
                id: "A".into(),
                level: 101,
            },
            "OutOfRange",
        ),
        (
            Request::SetRange {
                id: "A".into(),
                low: 0,
                high: 150,
            },
            "OutOfRange",
        ),
        (
            Request::SetRange {
                id: "A".into(),
                low: 101,
                high: 120,
            },
            "OutOfRange",
        ),
        (
            Request::SetDisplay {
                id: String::new(),
                level: 5,
            },
            "InvalidArgs",
        ),
        (
            Request::SetDisplay {
                id: long_id,
                level: 5,
            },
            "InvalidArgs",
        ),
        (
            Request::SetDisplay {
                id: "A\u{0}B".into(),
                level: 5,
            },
            "InvalidArgs",
        ),
        (
            Request::SetDisplay {
                id: "inconnu".into(),
                level: 5,
            },
            "UnknownDisplay",
        ),
        (Request::ApplyPreset(String::new()), "InvalidArgs"),
        (Request::ApplyPreset("n".repeat(64)), "InvalidArgs"),
        (Request::ApplyPreset("Inconnu".into()), "UnknownPreset"),
        (
            Request::SetMode {
                feature: "bogus".into(),
                on: true,
            },
            "InvalidArgs",
        ),
        (
            Request::SetMode {
                feature: "../../etc".into(),
                on: true,
            },
            "InvalidArgs",
        ),
        (
            Request::SetMode {
                feature: "auto".into(),
                on: true,
            },
            "NotConfigured",
        ),
    ];
    for (request, kind) in rejected {
        let error = rig.ask(request.clone()).await.unwrap_err();
        let ok = matches!(
            (&error, kind),
            (ApiError::OutOfRange(_), "OutOfRange")
                | (ApiError::InvalidArgs(_), "InvalidArgs")
                | (ApiError::UnknownDisplay(_), "UnknownDisplay")
                | (ApiError::UnknownPreset(_), "UnknownPreset")
                | (ApiError::NotConfigured(_), "NotConfigured")
        );
        assert!(ok, "{request:?} → {error:?} (attendu {kind})");
    }
    assert_eq!(rig.handle.snapshot(), before, "aucun effet sur l'état");
    assert_eq!(
        control.writes().len(),
        writes_before,
        "aucune écriture matérielle"
    );
}

#[tokio::test(start_paused = true)]
async fn t_st_16_concurrent_requests_are_serialized() {
    let (rig, control) = rig_with_control(None, None).await;
    let (a, b) = tokio::join!(
        rig.handle.request(Request::SetMaster(30)),
        rig.handle.request(Request::SetMaster(70))
    );
    a.unwrap();
    b.unwrap();
    rig.settle().await;
    let master = rig.handle.snapshot().master.unwrap();
    assert!(master == 30 || master == 70);
    assert_eq!(
        u32::from(master),
        control.raw(),
        "l'écran suit exactement la dernière requête traitée"
    );
    let writes = control.writes();
    assert_eq!(*writes.last().unwrap(), u32::from(master));
    assert!(
        writes.iter().all(|w| *w == 30 || *w == 70),
        "aucune valeur intermédiaire : {writes:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn presets_are_case_insensitive_and_errors_list_the_choices() {
    let (rig, control) = rig_with_control(None, None).await;
    rig.ok(Request::ApplyPreset("night".into())).await;
    assert_eq!(control.raw(), 30);
    rig.ok(Request::ApplyPreset("PRESENTATION".into())).await;
    assert_eq!(control.raw(), 100);
    let ApiError::UnknownPreset(message) = rig
        .ask(Request::ApplyPreset("Zzz".into()))
        .await
        .unwrap_err()
    else {
        panic!("erreur attendue : preset inconnu");
    };
    for expected in ["Night=30", "Day=80", "Presentation=100"] {
        assert!(message.contains(expected), "{message}");
    }
}

#[tokio::test(start_paused = true)]
async fn set_display_writes_only_that_display_and_rebuilds_the_master() {
    let hardware = MockHardware::new();
    let a = hardware.add("A", DisplayKind::External, 100, 50);
    let b = hardware.add("B", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware, None, None).await.unwrap();
    rig.ok(Request::SetDisplay {
        id: "A".into(),
        level: 20,
    })
    .await;
    assert_eq!((a.raw(), b.raw()), (20, 50));
    assert_eq!(
        rig.handle.snapshot().master,
        Some(35),
        "moyenne des niveaux des deux écrans"
    );
}

#[tokio::test(start_paused = true)]
async fn range_is_normalized_applied_and_persisted_with_a_backup_of_the_original() {
    let original = "# mes commentaires\n[general]\nstep = 7\n";
    let (rig, control) = rig_with_control(Some(original), None).await;
    rig.ok(Request::SetMaster(50)).await;
    rig.ok(Request::SetRange {
        id: "A".into(),
        low: 70,
        high: 80,
    })
    .await;
    let displays = list(&rig).await;
    assert_eq!(
        (displays[0].low, displays[0].high),
        (60, 80),
        "T-MAP-10 : le bas cède"
    );
    assert_eq!(
        control.raw(),
        70,
        "plage (60,80), maître 50 → 70 : tous les écrans ré-appliqués"
    );
    rig.advance(Duration::from_millis(600)).await;
    let text = wait_file(&rig.paths.config_file, |t| t.contains("range")).await;
    assert!(
        text.contains("[60, 80]") || text.contains("[\n    60,\n    80,\n]"),
        "{text}"
    );
    let backup = wait_file(&rig.paths.config_file.with_extension("toml.bak"), |_| true).await;
    assert_eq!(
        backup, original,
        "l'original (avec ses commentaires) est sauvegardé une fois"
    );
    let reloaded = ha2luxd::store::read_config(&rig.paths.config_file).unwrap();
    assert_eq!(
        reloaded.config.general.step, 7,
        "les autres réglages sont conservés"
    );
}

#[tokio::test(start_paused = true)]
async fn state_is_written_after_a_debounce_not_on_every_change() {
    let (rig, _) = rig_with_control(None, None).await;
    rig.advance(Duration::from_secs(2)).await;
    wait_file(&rig.paths.state_file, |t| t.contains("master = 50")).await; // maître reconstruit au démarrage
    rig.ok(Request::SetMaster(40)).await;
    rig.advance(Duration::from_millis(900)).await;
    assert!(
        std::fs::read_to_string(&rig.paths.state_file)
            .unwrap()
            .contains("master = 50"),
        "pas encore écrit"
    );
    for level in [41, 42, 43] {
        rig.ok(Request::SetMaster(level)).await;
        rig.advance(Duration::from_millis(300)).await;
    }
    // Chaque changement repousse l'écriture de ≥ 1 s : rafale = une seule écriture finale.
    assert!(
        std::fs::read_to_string(&rig.paths.state_file)
            .unwrap()
            .contains("master = 50")
    );
    rig.advance(Duration::from_millis(1_100)).await;
    wait_file(&rig.paths.state_file, |t| t.contains("master = 43")).await;
}

#[tokio::test(start_paused = true)]
async fn restart_keeps_the_state_and_restores_the_master_on_the_screens() {
    let (mut rig, control) = rig_with_control(None, None).await;
    rig.ok(Request::SetMaster(33)).await;
    control.set_raw(100); // l'écran s'est remis à 100 % pendant l'arrêt
    rig.restart().await;
    assert_eq!(
        rig.handle.snapshot().master,
        Some(33),
        "maître restauré du fichier d'état"
    );
    assert_eq!(
        control.raw(),
        33,
        "restauration effective sur l'écran revenu"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_flushes_pending_state_immediately() {
    let (rig, _) = rig_with_control(None, None).await;
    rig.ok(Request::SetMaster(61)).await;
    let (paths, _dir, _) = rig.shutdown().await; // moins d'une seconde plus tard : écriture « à l'arrêt propre »
    let text = std::fs::read_to_string(&paths.state_file).unwrap();
    assert!(text.contains("master = 61"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn learned_curve_and_range_survive_a_restart() {
    let mut cfg = String::new();
    cfg.push_str("[auto]\n");
    let (mut rig, _) =
        rig_with_control(Some(&cfg), Some("master = 20\ncurve = \"5:30,100:70\"\n")).await;
    rig.ok(Request::SetRange {
        id: "A".into(),
        low: 30,
        high: 90,
    })
    .await;
    rig.restart().await;
    let displays = list(&rig).await;
    assert_eq!((displays[0].low, displays[0].high), (30, 90));
    let text = std::fs::read_to_string(&rig.paths.state_file).unwrap();
    assert!(text.contains("5:30,100:70"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn t_cfg_11_reload_with_invalid_file_keeps_the_old_config() {
    let (rig, _) = rig_with_control(Some("[presets]\nZen = 12\n"), None).await;
    let before = std::fs::read(&rig.paths.config_file).unwrap();
    write_file(&rig.paths.config_file, "[presets\nZen = ");
    let broken = std::fs::read(&rig.paths.config_file).unwrap();
    let error = rig.ask(Request::Reload).await.unwrap_err();
    assert!(matches!(error, ApiError::ConfigInvalid(_)), "{error:?}");
    // L'ancienne configuration est toujours active, le fichier n'est pas touché.
    rig.ok(Request::ApplyPreset("zen".into())).await;
    assert_eq!(rig.handle.snapshot().master, Some(12));
    assert_eq!(std::fs::read(&rig.paths.config_file).unwrap(), broken);
    assert_ne!(before, broken);
    assert!(rig.handle.snapshot().config_error.is_none());
    // Corrigé puis rechargé : la nouvelle configuration prend effet.
    write_file(&rig.paths.config_file, "[presets]\nCalm = 22\n");
    rig.ok(Request::Reload).await;
    rig.ok(Request::ApplyPreset("calm".into())).await;
    assert_eq!(rig.handle.snapshot().master, Some(22));
    assert!(matches!(
        rig.ask(Request::ApplyPreset("zen".into())).await,
        Err(ApiError::UnknownPreset(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn reload_never_rewrites_the_file() {
    let original = "# commentaire\n[general]\nstep = 9\n";
    let (rig, _) = rig_with_control(Some(original), None).await;
    rig.ok(Request::Reload).await;
    rig.advance(Duration::from_secs(5)).await;
    assert_eq!(
        std::fs::read_to_string(&rig.paths.config_file).unwrap(),
        original
    );
    assert!(!rig.paths.config_file.with_extension("toml.bak").exists());
}

#[tokio::test(start_paused = true)]
async fn invalid_config_at_startup_runs_with_defaults_and_never_overwrites_the_file() {
    let broken = "[general\nstep = ";
    let (rig, _) = rig_with_control(Some(broken), None).await;
    assert!(rig.handle.snapshot().config_error.is_some());
    rig.ok(Request::ApplyPreset("night".into())).await; // valeurs par défaut en mémoire
    rig.ok(Request::SetRange {
        id: "A".into(),
        low: 10,
        high: 90,
    })
    .await;
    rig.ok(Request::SetMode {
        feature: "schedule".into(),
        on: true,
    })
    .await;
    rig.advance(Duration::from_secs(5)).await;
    assert_eq!(
        std::fs::read_to_string(&rig.paths.config_file).unwrap(),
        broken,
        "fichier préservé"
    );
    write_file(&rig.paths.config_file, "[general]\nstep = 3\n");
    rig.ok(Request::Reload).await;
    assert!(
        rig.handle.snapshot().config_error.is_none(),
        "corrigé puis rechargé"
    );
}

#[tokio::test(start_paused = true)]
async fn t_cfg_08_newer_config_version_refuses_to_start_and_leaves_the_file() {
    let hardware = MockHardware::new();
    let text = "version = 99\n";
    let error = Rig::start_with(hardware, Some(text), None)
        .await
        .err()
        .expect("le démarrage doit échouer");
    assert!(error.to_string().contains("99"), "{error}");
}

#[tokio::test(start_paused = true)]
async fn hotplug_via_rescan_adds_and_removes_displays() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let mut rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    assert_eq!(list(&rig).await.len(), 1);
    rig.drain_signals();
    let b = hardware.add("B", DisplayKind::External, 100, 10);
    rig.ok(Request::Rescan).await;
    assert_eq!(list(&rig).await.len(), 2);
    assert_eq!(b.raw(), 50, "l'écran qui apparaît reçoit seul le maître");
    assert!(rig.drain_signals().contains(&Signal::DisplaysChanged));
    hardware.set_present("B", false);
    rig.ok(Request::Rescan).await;
    assert_eq!(list(&rig).await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn fake_empty_list_is_rejected_and_retried_with_backoff() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    hardware.set_responding("A", false);
    let before = hardware.discoveries();
    rig.ok(Request::Rescan).await;
    assert_eq!(
        list(&rig).await[0].state,
        "controllable",
        "ancienne liste gardée"
    );
    rig.advance(Duration::from_millis(2_100)).await;
    rig.advance(Duration::from_millis(5_100)).await;
    assert!(
        hardware.discoveries() >= before + 3,
        "relances à 2 s puis 5 s : {}",
        hardware.discoveries() - before
    );
    assert_eq!(list(&rig).await[0].state, "controllable");
}

#[tokio::test(start_paused = true)]
async fn rescans_are_single_flight() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    hardware.set_discover_delay(Duration::from_millis(500));
    let before = hardware.discoveries();
    for _ in 0..8 {
        rig.ok(Request::Rescan).await;
    }
    rig.advance(Duration::from_secs(3)).await;
    assert!(
        hardware.discoveries() - before <= 2,
        "une rafale = au plus une énumération + une relance"
    );
}

#[tokio::test(start_paused = true)]
async fn quit_replies_flushes_state_and_closes_the_daemon() {
    let (rig, _) = rig_with_control(None, None).await;
    rig.ok(Request::SetMaster(72)).await;
    assert_eq!(rig.ask(Request::Quit).await.unwrap(), Reply::Done);
    rig.handle.wait_closed().await;
    assert!(matches!(
        rig.handle.request(Request::List).await,
        Err(ApiError::Stopped)
    ));
    let text = std::fs::read_to_string(&rig.paths.state_file).unwrap();
    assert!(text.contains("master = 72"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn signals_follow_level_mode_and_failures() {
    let (mut rig, control) = rig_with_control(
        Some(PLANNING_CONFIG)
            .map(|c| c.replace("enabled = true", "enabled = false"))
            .as_deref(),
        None,
    )
    .await;
    rig.drain_signals();
    rig.ok(Request::SetMaster(44)).await;
    assert!(rig.drain_signals().contains(&Signal::LevelChanged(44)));
    rig.ok(Request::SetMode {
        feature: "schedule".into(),
        on: true,
    })
    .await;
    let signals = rig.drain_signals();
    assert!(
        signals.contains(&Signal::ModeChanged("planning".to_owned())),
        "{signals:?}"
    );
    control.fail_always(Some(ha2lux_core::BackendError::PermissionDenied(
        "test".into(),
    )));
    rig.ok(Request::SetMaster(10)).await;
    rig.advance(Duration::from_secs(1)).await;
    assert!(
        rig.drain_signals()
            .iter()
            .any(|s| matches!(s, Signal::DisplayFailed { id, .. } if id == "A"))
    );
}

#[tokio::test(start_paused = true)]
async fn repeated_write_failures_make_the_display_unavailable() {
    let hardware = MockHardware::new();
    let control = hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    control.fail_always(Some(ha2lux_core::BackendError::Transient("occupé".into())));
    for level in [60, 61] {
        let _ = rig.ask(Request::SetMaster(level)).await;
        rig.advance(Duration::from_secs(10)).await;
    }
    // L'écran ne répond plus du tout (la sonde du rescan ciblé échoue aussi).
    hardware.set_responding("A", false);
    let _ = rig.ask(Request::SetMaster(62)).await;
    rig.advance(Duration::from_secs(10)).await;
    // Liste « vide » rejetée à 2, 5, 10, 20 s, puis acceptée : l'écran devient indisponible.
    rig.advance(Duration::from_secs(60)).await;
    let displays = list(&rig).await;
    assert_eq!(displays[0].state, "unavailable", "{displays:?}");
    assert!(matches!(
        rig.ask(Request::SetMaster(10)).await,
        Err(ApiError::NoControllableDisplay)
    ));
    let Reply::Status(status) = rig.ask(Request::Status).await.unwrap() else {
        panic!()
    };
    assert!(status.writes_failed >= 1, "{status:?}");
    assert!(status.last_error.is_some());
    // L'écran revient : il redevient contrôlable et reçoit seul le maître.
    hardware.set_responding("A", true);
    control.fail_always(None);
    rig.ok(Request::Rescan).await;
    assert_eq!(list(&rig).await[0].state, "controllable");
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(
        control.raw(),
        62,
        "le maître est restauré sur l'écran revenu"
    );
}

#[tokio::test(start_paused = true)]
async fn planning_wins_over_a_stale_persisted_master_at_startup() {
    let (rig, control) = rig_with_control(Some(PLANNING_CONFIG), Some("master = 10\n")).await;
    // 14:00, points 08:00=80 et 20:00=20 : valeur du planning = 50.
    assert_eq!(control.raw(), 50);
    assert_eq!(rig.handle.snapshot().mode, "planning");
    assert_eq!(rig.handle.snapshot().master, Some(50));
}

#[tokio::test(start_paused = true)]
async fn manual_change_suspends_the_planning_until_the_next_point() {
    let (rig, control) = rig_with_control(Some(PLANNING_CONFIG), None).await;
    rig.ok(Request::SetMaster(70)).await;
    assert_eq!(
        rig.handle.snapshot().schedule_resume.as_deref(),
        Some("20:00")
    );
    rig.advance(Duration::from_secs(5 * 3600 + 59 * 60)).await; // 19:59
    assert_eq!(control.raw(), 70);
    rig.advance(Duration::from_secs(60)).await; // 20:00
    assert_eq!(control.raw(), 20, "le planning reprend au prochain point");
    assert!(rig.handle.snapshot().schedule_resume.is_none());
}

#[tokio::test(start_paused = true)]
async fn idle_dim_and_restore_through_system_events() {
    let (rig, control) = rig_with_control(
        Some("[idle]\nenabled = true\nlevel = 5\nminutes = 5\n"),
        None,
    )
    .await;
    rig.ok(Request::SetMaster(60)).await;
    rig.handle
        .send_event(Event::IdleProbe {
            idle_ms: 300_000,
            blocked: false,
        })
        .await
        .unwrap();
    rig.settle().await;
    assert_eq!(control.raw(), 5);
    assert!(rig.handle.snapshot().dimmed);
    assert_eq!(
        rig.handle.snapshot().master,
        Some(60),
        "le maître n'est pas modifié"
    );
    rig.handle.send_event(Event::Activity).await.unwrap();
    rig.settle().await;
    assert_eq!(control.raw(), 60);
}

#[tokio::test(start_paused = true)]
async fn wake_rescans_and_reapplies_twice() {
    let (rig, control) = rig_with_control(None, None).await;
    rig.ok(Request::SetMaster(45)).await;
    control.set_raw(100); // l'écran se remet à 100 % au réveil
    rig.handle.send_event(Event::Wake).await.unwrap();
    rig.advance(Duration::from_millis(1_600)).await;
    assert_eq!(
        control.raw(),
        45,
        "ré-application immédiate après la liste acceptée"
    );
    control.set_raw(100); // certains écrans se remettent à 100 % après le premier réglage
    rig.advance(Duration::from_millis(3_100)).await;
    assert_eq!(control.raw(), 45, "seconde ré-application après ~3 s");
}

#[tokio::test(start_paused = true)]
async fn schedule_toggle_persists_in_the_config() {
    let (rig, _) = rig_with_control(
        Some(
            PLANNING_CONFIG
                .replace("enabled = true", "enabled = false")
                .as_str(),
        ),
        None,
    )
    .await;
    rig.ok(Request::SetMode {
        feature: "schedule".into(),
        on: true,
    })
    .await;
    rig.advance(Duration::from_millis(600)).await;
    let text = wait_file(&rig.paths.config_file, |t| t.contains("enabled = true")).await;
    assert!(
        text.contains("08:00"),
        "les points du planning sont conservés : {text}"
    );
    assert!(MinuteOfDay::parse("08:00").is_ok());
}
