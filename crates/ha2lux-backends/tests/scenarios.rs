//! T-ST, T-IDL and T-SCH-21: state machine scenarios with a mock
//! backend and a virtual clock (no real `sleep`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use ha2lux_core::{
    BaseMode, Command, ControlError, ControllerConfig, DisplayState, Event, Feature, HaHealth,
    InitialState, Notice, Persist, Point, Schedule,
};
use support::{World, id, t};

fn schedule(points: &[(&str, u8)]) -> Schedule {
    Schedule::new(
        points
            .iter()
            .map(|&(h, level)| Point { time: t(h), level })
            .collect(),
    )
    .unwrap()
}

fn planning_a() -> ControllerConfig {
    ControllerConfig {
        schedule: schedule(&[("08:00", 80), ("20:00", 20)]),
        schedule_enabled: true,
        ..ControllerConfig::default()
    }
}

fn auto_cfg() -> ControllerConfig {
    ControllerConfig {
        auto_enabled: true,
        auto_configured: true,
        ..ControllerConfig::default()
    }
}

fn auto_and_planning() -> ControllerConfig {
    ControllerConfig {
        auto_enabled: true,
        auto_configured: true,
        ..planning_a()
    }
}

fn idle_cfg() -> ControllerConfig {
    let mut cfg = ControllerConfig::default();
    cfg.idle.enabled = true;
    cfg.idle.level = 5;
    cfg.idle.minutes = 5;
    cfg
}

fn initial(master: Option<u8>) -> InitialState {
    InitialState {
        master,
        ..InitialState::default()
    }
}

/// World with the given displays (initial levels), enumeration done and log cleared.
fn world(start: &str, cfg: ControllerConfig, master: Option<u8>, displays: &[(&str, u8)]) -> World {
    let mut w = World::new(start, cfg, initial(master));
    for &(name, level) in displays {
        w.plug(name, level);
    }
    w.rescan().unwrap();
    w.clear_log();
    w
}

/// Enables auto: initial measurement `lux`, which becomes the owner.
fn auto_world(cfg: ControllerConfig, lux: f64) -> World {
    let mut w = world("14:00", cfg, Some(50), &[("A", 50)]);
    w.send_ok(Event::Lux(lux));
    w.clear_log();
    w
}

// ------------------------------------------------------------------ T-ST ---

#[test]
fn t_st_01_manual_suspends_planning_until_next_point() {
    let mut w = world("14:00", planning_a(), None, &[("A", 40)]);
    w.advance(0);
    assert_eq!(w.level_of("A"), 50, "planning à 14:00");
    w.send_ok(Event::SetMaster(70));
    assert_eq!(w.level_of("A"), 70);
    let s = w.status().suspension.expect("planning suspendu");
    assert_eq!((s.start(), s.resume()), (t("14:00"), t("20:00")));
    w.advance_minutes(5 * 60 + 59); // 19:59
    assert_eq!(w.level_of("A"), 70, "jamais plus tôt que le prochain point");
    w.advance_minutes(1); // 20:00
    assert_eq!(w.level_of("A"), 20);
    assert!(w.status().suspension.is_none());
}

#[test]
fn t_st_02_manual_under_auto_learns_and_keeps_planning_active() {
    let mut w = auto_world(auto_and_planning(), 120.0);
    assert_eq!(w.status().mode, BaseMode::AutoLux);
    w.persisted.clear();
    w.send_ok(Event::SetMaster(60));
    assert_eq!(w.level_of("A"), 60, "niveau tenu");
    assert_eq!(w.ctl.curve().level_at(120.0), 60, "point (120, 60) appris");
    assert!(w.persisted.contains(&Persist::Curve));
    assert!(w.status().suspension.is_none(), "planning non suspendu");
}

#[test]
fn t_st_03_ha_offline_falls_back_to_planning() {
    let mut w = auto_world(auto_and_planning(), 1000.0);
    assert_eq!(w.level_of("A"), 100);
    w.send_ok(Event::Ha(HaHealth::Offline));
    assert_eq!(w.status().ha_health, HaHealth::Offline);
    assert_eq!(w.status().mode, BaseMode::Planning);
    assert_eq!(w.level_of("A"), 50, "valeur du planning forcée");
}

#[test]
fn t_st_04_ha_back_ramps_towards_target() {
    let mut w = auto_world(auto_and_planning(), 1000.0);
    w.send_ok(Event::Ha(HaHealth::Offline));
    w.send_ok(Event::Ha(HaHealth::Ok));
    assert_eq!(w.status().ramp_target, Some(100));
    w.advance(1000);
    assert_eq!(w.master(), Some(60), "1 point toutes les 100 ms");
    w.advance(4000);
    assert_eq!(w.level_of("A"), 100);
    assert_eq!(w.status().ramp_target, None);
}

#[test]
fn t_st_05_auto_without_measure_is_not_owner() {
    let mut w = world("14:00", auto_and_planning(), Some(50), &[("A", 50)]);
    assert_eq!(w.status().mode, BaseMode::Planning);
    w.advance(0);
    assert_eq!(w.level_of("A"), 50);
    w.send_ok(Event::SetMaster(70));
    assert!(
        w.status().suspension.is_some(),
        "le planning est suspendu comme sans auto"
    );
    // A wake-up restores the last master: auto is not the owner.
    w.send_ok(Event::Wake);
    w.rescan().unwrap();
    assert_eq!(w.level_of("A"), 70);
}

#[test]
fn t_st_06_wake_reapplies_forced_without_ramp() {
    let mut w = auto_world(auto_cfg(), 10.0);
    assert_eq!(w.level_of("A"), 40);
    w.plug("A", 100); // the display reset itself to 100% (same identity)
    w.send_ok(Event::Wake);
    assert!(
        w.rescans
            .iter()
            .any(|(target, delay)| target.is_none() && *delay > 0)
    );
    w.rescan().unwrap();
    assert_eq!(w.level_of("A"), 40, "niveau pour le lux courant, immédiat");
    assert_eq!(w.status().ramp_target, None);
}

#[test]
fn wake_reapplies_twice() {
    let mut w = world("14:00", ControllerConfig::default(), Some(60), &[("A", 60)]);
    w.send_ok(Event::Wake);
    w.rescan().unwrap();
    w.plug("A", 100); // some displays reset to 100% after the first adjustment
    w.advance(3_000);
    assert_eq!(w.level_of("A"), 60, "seconde ré-application après ~3 s");
}

#[test]
fn t_st_07_manual_stops_ramp() {
    let mut w = auto_world(auto_cfg(), 300.0);
    let start = w.master().unwrap();
    w.send_ok(Event::Lux(8.0));
    w.send_ok(Event::Lux(8.0));
    assert!(w.status().ramp_target.is_some(), "rampe vers le bas");
    w.advance(500);
    assert!(w.master().unwrap() < start);
    w.send_ok(Event::SetMaster(60));
    assert_eq!(w.status().ramp_target, None);
    w.advance(5_000);
    assert_eq!(w.level_of("A"), 60, "la valeur manuelle reste");
}

#[test]
fn t_st_08_idle_dim_stops_ramp() {
    let mut cfg = auto_cfg();
    cfg.idle.enabled = true;
    let mut w = auto_world(cfg, 300.0);
    w.send_ok(Event::Lux(8.0));
    w.send_ok(Event::Lux(8.0));
    assert!(w.status().ramp_target.is_some());
    w.send_ok(Event::IdleProbe {
        idle_ms: 300_000,
        blocked: false,
    });
    assert_eq!(w.status().ramp_target, None);
    w.advance(5_000);
    assert_eq!(w.level_of("A"), 5, "la rampe périmée n'écrit plus rien");
}

#[test]
fn t_st_09_ramp_writes_only_changed_displays() {
    let mut w = world("14:00", auto_cfg(), Some(50), &[("A", 50), ("B", 50)]);
    w.send_ok(Event::SetRange {
        display: id("B"),
        low: 40,
        high: 60,
    });
    w.send_ok(Event::Lux(300.0));
    w.send_ok(Event::Lux(8.0));
    w.send_ok(Event::Lux(8.0));
    w.clear_log();
    w.advance(2_000); // 20 ramp steps
    let a = w.writes_to("A");
    let b = w.writes_to("B");
    assert_eq!(a.len(), 20, "A change à chaque pas");
    assert!(
        b.len() <= 5,
        "B (largeur 20) change environ tous les 5 pas : {b:?}"
    );
    assert!(
        b.windows(2).all(|p| p[0] != p[1]),
        "aucune écriture inutile"
    );
}

#[test]
fn t_st_10_returning_display_gets_only_master() {
    let mut w = world(
        "14:00",
        ControllerConfig::default(),
        Some(50),
        &[("A", 50), ("B", 50)],
    );
    w.set_responding("B", false);
    w.rescan().unwrap();
    assert_eq!(
        w.status()
            .displays
            .iter()
            .find(|d| d.0 == id("B"))
            .map(|d| d.1),
        Some(DisplayState::Unavailable)
    );
    w.clear_log();
    w.set_responding("B", true);
    w.rescan().unwrap();
    assert_eq!(w.writes_to("B"), [50]);
    assert!(w.writes_to("A").is_empty(), "A n'est pas touché");
}

#[test]
fn t_st_11_fake_list_rejected_with_retries() {
    let mut w = world("14:00", ControllerConfig::default(), Some(50), &[("A", 50)]);
    w.set_responding("A", false);
    for expected in [2_000, 5_000, 10_000, 20_000] {
        w.rescan().unwrap();
        assert_eq!(w.rescans.last().unwrap().1, expected);
        assert_eq!(
            w.status().displays[0].1,
            DisplayState::Controllable,
            "ancienne liste gardée"
        );
    }
    assert!(matches!(
        w.notices.last(),
        Some(Notice::ListRejected {
            retry_in_ms: 20_000
        })
    ));
    w.rescan().unwrap();
    assert_eq!(
        w.status().displays[0].1,
        DisplayState::Unavailable,
        "enfin acceptée"
    );
}

#[test]
fn t_st_12_three_write_failures_make_display_unavailable() {
    let mut w = world(
        "14:00",
        ControllerConfig::default(),
        Some(50),
        &[("A", 50), ("B", 50)],
    );
    w.set_responding("A", false);
    for level in [60, 61, 62] {
        w.send_ok(Event::SetMaster(level));
    }
    let state = |w: &World, n: &str| w.status().displays.iter().find(|d| d.0 == id(n)).unwrap().1;
    assert_eq!(state(&w, "A"), DisplayState::Unavailable);
    assert!(w.rescans.contains(&(Some(id("A")), 0)), "rescan ciblé");
    assert!(w.notices.contains(&Notice::DisplayUnavailable(id("A"))));
    assert_eq!(w.level_of("B"), 62, "les autres écrans ne sont pas bloqués");
}

#[test]
fn t_st_13_range_change_reapplies_all_unless_dimmed() {
    let mut cfg = idle_cfg();
    cfg.idle.enabled = true;
    let mut w = world("14:00", cfg, Some(50), &[("A", 50), ("B", 50)]);
    w.send_ok(Event::SetRange {
        display: id("A"),
        low: 70,
        high: 80,
    });
    assert_eq!(
        w.level_of("A"),
        70,
        "plage normalisée (60, 80) : 60 + 50 % de 20"
    );
    assert_eq!(w.writes_to("B"), [50], "tous les écrans ré-appliqués");
    assert!(w.persisted.contains(&Persist::Ranges));

    w.send_ok(Event::IdleProbe {
        idle_ms: 300_000,
        blocked: false,
    });
    w.clear_log();
    w.send_ok(Event::SetRange {
        display: id("A"),
        low: 40,
        high: 100,
    });
    assert!(w.log.is_empty(), "rien n'est écrit pendant l'atténuation");
    w.send_ok(Event::Activity);
    assert_eq!(
        w.level_of("A"),
        70,
        "la restauration utilise la nouvelle plage : 40 + 50 % de 60"
    );
}

#[test]
fn t_st_14_preset_under_auto_pauses_without_learning() {
    let mut w = auto_world(auto_cfg(), 100.0);
    assert_eq!(w.level_of("A"), 70);
    w.persisted.clear();
    w.send_ok(Event::Preset(30));
    assert_eq!(w.level_of("A"), 30);
    assert!(w.status().auto_paused);
    assert!(w.ctl.curve().points().is_empty(), "preset non appris");
    assert!(!w.persisted.contains(&Persist::Curve));
    w.send_ok(Event::Lux(100.0));
    assert_eq!(w.level_of("A"), 30, "auto en pause");
    w.advance_minutes(31);
    w.send_ok(Event::Lux(100.0));
    assert_eq!(
        w.status().ramp_target,
        Some(70),
        "pause terminée : l'auto reprend"
    );
}

#[test]
fn t_st_14_notable_light_change_ends_pause() {
    let mut w = auto_world(auto_cfg(), 100.0);
    w.send_ok(Event::Preset(30));
    w.send_ok(Event::Lux(2000.0));
    w.send_ok(Event::Lux(2000.0));
    assert!(!w.status().auto_paused);
    assert_eq!(w.status().ramp_target, Some(100));
}

#[test]
fn t_st_15_unknown_master_is_reconstructed_then_restored() {
    let mut w = World::new(
        "14:00",
        ControllerConfig::default(),
        InitialState::default(),
    );
    w.plug("A", 40);
    w.rescan().unwrap();
    assert_eq!(w.master(), Some(40), "reconstruit sans rien écrire");
    assert!(w.log.is_empty());
    assert!(w.persisted.contains(&Persist::Master));
    w.plug("A", 100); // the display resets to 100% on wake-up
    w.send_ok(Event::Wake);
    w.rescan().unwrap();
    assert_eq!(w.level_of("A"), 40, "restauration effective");
}

#[test]
fn t_st_16_commands_are_serialized() {
    let mut w = world("14:00", ControllerConfig::default(), Some(50), &[("A", 50)]);
    w.send_ok(Event::SetMaster(40));
    w.send_ok(Event::SetMaster(80));
    assert_eq!(w.writes_to("A"), [40, 80]);
    assert_eq!(w.master(), Some(80));
}

#[test]
fn invalid_requests_are_rejected_without_effect() {
    let mut w = world("14:00", ControllerConfig::default(), Some(50), &[("A", 50)]);
    let before = w.status();
    for event in [
        Event::SetMaster(101),
        Event::SetMaster(-1),
        Event::Step(101),
        Event::Preset(300),
        Event::SetDisplay {
            display: id("A"),
            level: 101,
        },
        Event::SetDisplay {
            display: id("nope"),
            level: 10,
        },
        Event::SetRange {
            display: id("nope"),
            low: 0,
            high: 100,
        },
    ] {
        assert!(w.send(event).is_err());
    }
    assert_eq!(w.status(), before);
    assert!(w.log.is_empty());
}

#[test]
fn set_without_controllable_display_fails() {
    let mut w = World::new("14:00", ControllerConfig::default(), initial(Some(50)));
    assert_eq!(
        w.send(Event::SetMaster(40)),
        Err(ControlError::NoControllableDisplay)
    );
}

#[test]
fn set_display_refused_when_unavailable_and_rebuilds_master() {
    let mut w = world(
        "14:00",
        ControllerConfig::default(),
        Some(50),
        &[("A", 50), ("B", 50)],
    );
    w.send_ok(Event::SetRange {
        display: id("B"),
        low: 40,
        high: 100,
    });
    w.clear_log();
    w.send_ok(Event::SetDisplay {
        display: id("A"),
        level: 30,
    });
    assert_eq!(w.writes_to("A"), [30]);
    assert!(w.writes_to("B").is_empty());
    // A at 30 (range 0-100 -> 30), B at 70 (range 40-100 -> 50): average 40.
    assert_eq!(w.master(), Some(40));
    w.set_responding("B", false);
    w.rescan().unwrap();
    assert_eq!(
        w.send(Event::SetDisplay {
            display: id("B"),
            level: 10
        }),
        Err(ControlError::DisplayUnavailable(id("B")))
    );
}

#[test]
fn step_changes_master_and_clamps() {
    let mut w = world("14:00", ControllerConfig::default(), Some(98), &[("A", 98)]);
    w.send_ok(Event::Step(5));
    assert_eq!(w.master(), Some(100));
    w.send_ok(Event::Step(-100));
    assert_eq!(w.master(), Some(0));
}

#[test]
fn feature_toggles_and_curve_actions() {
    let mut w = auto_world(auto_cfg(), 100.0);
    w.send_ok(Event::SetMaster(30));
    assert_eq!(w.ctl.curve().level_at(100.0), 30);
    w.send_ok(Event::UndoLearn);
    assert!(w.ctl.curve().points().is_empty());
    w.send_ok(Event::SetMaster(30));
    w.send_ok(Event::ResetCurve);
    assert!(w.ctl.curve().points().is_empty());
    assert_eq!(
        w.level_of("A"),
        70,
        "réinitialisation : application immédiate"
    );
    w.send_ok(Event::Feature(Feature::Auto, false));
    assert_eq!(w.status().mode, BaseMode::Static);
}

#[test]
fn reload_lifts_suspension_and_recalculates() {
    let mut w = world("14:00", planning_a(), Some(50), &[("A", 50)]);
    w.send_ok(Event::SetMaster(70));
    assert!(w.status().suspension.is_some());
    w.send_ok(Event::Reload(Box::new(planning_a())));
    assert!(w.status().suspension.is_none());
    assert_eq!(w.level_of("A"), 50);
}

// --------------------------------------------------------------- T-SCH-21 ---

fn constant_planning() -> ControllerConfig {
    ControllerConfig {
        schedule: schedule(&[("08:00", 70)]),
        schedule_enabled: true,
        ..ControllerConfig::default()
    }
}

#[test]
fn t_sch_21_writes_only_when_changed() {
    let mut w = world("14:00", constant_planning(), Some(50), &[("A", 50)]);
    w.advance_minutes(3);
    assert_eq!(
        w.writes_to("A"),
        [70],
        "une seule écriture sur plusieurs minutes"
    );
}

#[test]
fn t_sch_21_retries_after_failed_write() {
    let mut w = world("14:00", constant_planning(), Some(50), &[("A", 50)]);
    w.set_responding("A", false);
    w.advance(0);
    assert_eq!(
        w.status().applied_master,
        None,
        "un échec n'est pas mémorisé"
    );
    w.set_responding("A", true);
    w.advance_minutes(1);
    assert_eq!(w.writes_to("A"), [70], "la minute suivante retente");
    assert_eq!(w.status().applied_master, Some(70));
}

// --------------------------------------------------------------- T-IDL ----

fn idle_world(initial_master: Option<u8>) -> World {
    let mut w = World::new("14:00", idle_cfg(), initial(initial_master));
    w.plug("A", 50);
    w.plug("B", 50);
    w.rescan().unwrap();
    w.send_ok(Event::SetRange {
        display: id("B"),
        low: 40,
        high: 100,
    });
    w.clear_log();
    w
}

const DIM: Event = Event::IdleProbe {
    idle_ms: 300_000,
    blocked: false,
};

#[test]
fn t_idl_01_not_yet() {
    let mut w = idle_world(Some(50));
    w.send_ok(Event::IdleProbe {
        idle_ms: 299_000,
        blocked: false,
    });
    assert!(w.log.is_empty());
    assert!(!w.status().dimmed);
}

#[test]
fn t_idl_02_03_dim_maps_through_range_or_absolute() {
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    assert_eq!(w.level_of("A"), 5);
    assert_eq!(w.level_of("B"), 43, "40 + round(60 × 0,05)");
    assert_eq!(w.master(), Some(50), "maître inchangé");

    let mut cfg = idle_cfg();
    cfg.idle.level_is_absolute = true;
    let mut w = World::new("14:00", cfg, initial(Some(50)));
    w.plug("B", 50);
    w.rescan().unwrap();
    w.send_ok(Event::SetRange {
        display: id("B"),
        low: 40,
        high: 100,
    });
    w.send_ok(DIM);
    assert_eq!(w.level_of("B"), 5, "niveau absolu, non remappé");
}

#[test]
fn t_idl_04_05_blocked_then_released() {
    let mut w = idle_world(Some(50));
    w.send_ok(Event::IdleProbe {
        idle_ms: 300_000,
        blocked: true,
    });
    w.send_ok(Event::IdleProbe {
        idle_ms: 330_000,
        blocked: true,
    });
    assert!(
        w.log.is_empty() && !w.status().dimmed,
        "inhibiteur ou micro ouvert"
    );
    w.send_ok(Event::IdleProbe {
        idle_ms: 360_000,
        blocked: false,
    });
    assert!(w.status().dimmed, "atténue dès la levée");
}

#[test]
fn t_idl_06_activity_reapplies_by_priority() {
    // Latest master.
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    w.send_ok(Event::Activity);
    assert_eq!((w.level_of("A"), w.level_of("B")), (50, 70));
    // Schedule > master.
    let mut w = world(
        "14:00",
        ControllerConfig {
            idle: idle_cfg().idle,
            ..planning_a()
        },
        Some(10),
        &[("A", 10)],
    );
    w.send_ok(DIM);
    w.send_ok(Event::Activity);
    assert_eq!(w.level_of("A"), 50);
    // Auto > planning.
    let mut cfg = auto_and_planning();
    cfg.idle = idle_cfg().idle;
    let mut w = auto_world(cfg, 1000.0);
    w.send_ok(DIM);
    w.send_ok(Event::Activity);
    assert_eq!(w.level_of("A"), 100);
}

#[test]
fn t_idl_07_planning_recalculated_after_long_dim() {
    let mut w = world(
        "14:00",
        ControllerConfig {
            idle: idle_cfg().idle,
            ..planning_a()
        },
        Some(50),
        &[("A", 50)],
    );
    w.advance(0);
    w.send_ok(DIM);
    w.advance_minutes(3 * 60);
    assert_eq!(
        w.level_of("A"),
        5,
        "le planning ne s'applique pas pendant l'atténuation"
    );
    w.send_ok(Event::Activity); // 17:00
    assert_eq!(
        w.level_of("A"),
        35,
        "valeur du planning pour l'heure courante"
    );
}

#[test]
fn t_idl_08_cli_command_cancels_dim() {
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    w.send_ok(Event::SetMaster(40));
    assert!(!w.status().dimmed);
    assert_eq!(w.level_of("A"), 40);
}

#[test]
fn t_idl_09_wake_keeps_dim_level() {
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    w.send_ok(Event::Wake);
    w.plug("A", 100);
    w.plug("B", 100);
    w.rescan().unwrap();
    assert_eq!(
        (w.level_of("A"), w.level_of("B")),
        (5, 43),
        "niveau d'inactivité conservé (B : plage 40-100)"
    );
}

#[test]
fn t_idl_10_plugged_display_gets_dim_level() {
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    w.plug("C", 100);
    w.rescan().unwrap();
    assert_eq!(w.level_of("C"), 5);
}

#[test]
fn t_idl_11_disabling_restores_immediately() {
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    w.send_ok(Event::Feature(Feature::IdleDim, false));
    assert!(!w.status().dimmed);
    assert_eq!(w.level_of("A"), 50);
}

#[test]
fn t_idl_12_master_rebuilt_before_dim() {
    let mut w = World::new("14:00", idle_cfg(), InitialState::default());
    w.plug("A", 30);
    w.plug("B", 50);
    w.rescan().unwrap();
    let rebuilt = w.master();
    assert_eq!(rebuilt, Some(40), "moyenne des niveaux lus");
    w.send_ok(DIM);
    assert_eq!(w.master(), rebuilt);
}

#[test]
fn t_idl_13_blocker_appearing_after_dim_restores() {
    let mut w = idle_world(Some(50));
    w.send_ok(DIM);
    w.send_ok(Event::IdleProbe {
        idle_ms: 400_000,
        blocked: true,
    });
    assert!(!w.status().dimmed);
    assert_eq!(w.level_of("A"), 50);
}

#[test]
fn idle_disabled_never_dims() {
    let mut w = world("14:00", ControllerConfig::default(), Some(50), &[("A", 50)]);
    w.send_ok(DIM);
    assert!(w.log.is_empty());
}

#[test]
fn rescan_command_is_never_a_write() {
    let mut w = world("14:00", ControllerConfig::default(), Some(50), &[("A", 50)]);
    let commands = w
        .ctl
        .handle(ha2lux_core::Clock::now(&w.clock), Event::Wake)
        .unwrap();
    assert!(
        commands.iter().all(|c| matches!(c, Command::Rescan { .. })),
        "{commands:?}"
    );
}
