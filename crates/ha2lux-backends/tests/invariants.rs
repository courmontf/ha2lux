//! Invariants de `ARCHITECTURE.md` §2.4, vérifiés en proptest sur des suites
//! aléatoires d'événements, avec backend factice et horloge virtuelle.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use ha2lux_core::{
    BaseMode, Clock, ControllerConfig, DisplayState, Event, Feature, HaHealth, InitialState,
    Notice, Point, Schedule,
};
use proptest::prelude::*;
use support::{World, id, t};

#[derive(Clone, Debug)]
enum Action {
    SetMaster(i64),
    Step(i64),
    Preset(i64),
    SetDisplay(bool, i64),
    SetRange(bool, i64, i64),
    Lux(f64),
    Ha(HaHealth),
    Probe(u64, bool),
    Activity,
    Wake { reset_hardware: bool },
    Advance(u64),
    Feature(Feature, bool),
    Respond(bool, bool),
    Rescan,
    ResetCurve,
    Undo,
}

fn action() -> impl Strategy<Value = Action> {
    let health = prop_oneof![
        Just(HaHealth::Ok),
        Just(HaHealth::Offline),
        Just(HaHealth::Degraded)
    ];
    let feature = prop_oneof![
        Just(Feature::Schedule),
        Just(Feature::IdleDim),
        Just(Feature::Auto)
    ];
    prop_oneof![
        3 => (-5i64..=105).prop_map(Action::SetMaster),
        2 => (-30i64..=30).prop_map(Action::Step),
        2 => (0i64..=100).prop_map(Action::Preset),
        2 => (any::<bool>(), -5i64..=105).prop_map(|(b, l)| Action::SetDisplay(b, l)),
        1 => (any::<bool>(), -20i64..=120, -20i64..=120).prop_map(|(b, lo, hi)| Action::SetRange(b, lo, hi)),
        6 => (0.0f64..3000.0).prop_map(Action::Lux),
        2 => health.prop_map(Action::Ha),
        3 => (0u64..=10, any::<bool>()).prop_map(|(m, b)| Action::Probe(m, b)),
        2 => Just(Action::Activity),
        2 => any::<bool>().prop_map(|r| Action::Wake { reset_hardware: r }),
        5 => (0u64..=200_000).prop_map(Action::Advance),
        1 => (feature, any::<bool>()).prop_map(|(f, on)| Action::Feature(f, on)),
        1 => (any::<bool>(), any::<bool>()).prop_map(|(b, r)| Action::Respond(b, r)),
        2 => Just(Action::Rescan),
        1 => Just(Action::ResetCurve),
        1 => Just(Action::Undo),
    ]
}

fn setup() -> World {
    let mut cfg = ControllerConfig {
        schedule: Schedule::new(vec![
            Point {
                time: t("08:00"),
                level: 80,
            },
            Point {
                time: t("08:30"),
                level: 20,
            },
            Point {
                time: t("09:15"),
                level: 60,
            },
        ])
        .unwrap(),
        schedule_enabled: true,
        auto_enabled: true,
        auto_configured: true,
        ..ControllerConfig::default()
    };
    cfg.idle.enabled = true;
    let mut w = World::new(
        "07:50",
        cfg,
        InitialState {
            master: Some(50),
            ..InitialState::default()
        },
    );
    w.plug_with_max("A", 255, 50);
    w.plug("B", 50);
    w.rescan().unwrap();
    w.send_ok(Event::SetRange {
        display: id("B"),
        low: 40,
        high: 100,
    });
    w
}

fn name(second: bool) -> &'static str {
    if second { "B" } else { "A" }
}

/// Niveau d'inactivité attendu (configuration par défaut : niveau 5, remappé).
fn dim_level(range: ha2lux_core::Range) -> u8 {
    range.display_level(5)
}

fn check_sequence(actions: &[Action]) -> Result<(), TestCaseError> {
    let mut w = setup();
    let mut hardware_dirty = false;
    for action in actions {
        let before = w.status();
        let log_len = w.log.len();
        let notices_len = w.notices.len();
        let mut result = Ok(());
        let mut manual = false;
        let mut advanced_ms = None;
        let mut wake_checked = false;
        match action.clone() {
            Action::SetMaster(m) => {
                result = w.send(Event::SetMaster(m));
                manual = result.is_ok();
            }
            Action::Step(d) => {
                result = w.send(Event::Step(d));
                manual = result.is_ok();
            }
            Action::Preset(l) => {
                result = w.send(Event::Preset(l));
                manual = result.is_ok();
            }
            Action::SetDisplay(b, level) => {
                result = w.send(Event::SetDisplay {
                    display: id(name(b)),
                    level,
                });
                manual = result.is_ok();
            }
            Action::SetRange(b, low, high) => {
                result = w.send(Event::SetRange {
                    display: id(name(b)),
                    low,
                    high,
                });
            }
            Action::Lux(lux) => w.send_ok(Event::Lux(lux)),
            Action::Ha(h) => w.send_ok(Event::Ha(h)),
            Action::Probe(minutes, blocked) => {
                w.send_ok(Event::IdleProbe {
                    idle_ms: minutes * 60_000,
                    blocked,
                });
            }
            Action::Activity => w.send_ok(Event::Activity),
            Action::Wake { reset_hardware } => {
                if reset_hardware {
                    w.reset_hardware();
                    hardware_dirty = true;
                }
                w.send_ok(Event::Wake);
                w.rescan().unwrap();
                wake_checked = !w.notices[notices_len..]
                    .iter()
                    .any(|n| matches!(n, Notice::ListRejected { .. }));
            }
            Action::Advance(ms) => {
                w.advance(ms);
                advanced_ms = Some(ms);
            }
            Action::Feature(f, on) => w.send_ok(Event::Feature(f, on)),
            Action::Respond(b, r) => w.set_responding(name(b), r),
            Action::Rescan => w.rescan().unwrap(),
            Action::ResetCurve => w.send_ok(Event::ResetCurve),
            Action::Undo => w.send_ok(Event::UndoLearn),
        }
        let after = w.status();

        // Une requête invalide est refusée sans aucun effet.
        if result.is_err() {
            prop_assert_eq!(&before, &after);
            prop_assert_eq!(w.log.len(), log_len);
        }

        // Invariant 2 : jamais de valeur hors bornes vers le matériel ; maître dans 0-100.
        for write in &w.log[log_len..] {
            prop_assert!(write.level <= 100);
            let max = if write.display == id("A") { 255 } else { 100 };
            prop_assert!(write.raw <= max);
        }
        if let Some(m) = after.master {
            prop_assert!(m <= 100);
        }

        // Invariant 1 : une seule source décide (mode cohérent avec ses conditions).
        if after.mode == BaseMode::AutoLux {
            prop_assert!(after.has_measure && after.ha_health == HaHealth::Ok);
        }

        // Invariant 4 : toute rampe appartient à l'auto, jamais à l'atténuation ;
        // un événement manuel annule rampe et atténuation.
        if after.ramp_target.is_some() {
            prop_assert!(after.mode == BaseMode::AutoLux && !after.dimmed);
        }
        if manual {
            prop_assert!(after.ramp_target.is_none() && !after.dimmed);
        }

        // Invariant 3 : l'atténuation ne modifie pas le maître mémorisé.
        if before.master.is_some()
            && (!before.dimmed && after.dimmed || before.dimmed && after.dimmed)
        {
            prop_assert_eq!(before.master, after.master);
        }

        // Invariant 5 : « appliqué » implique que chaque écran contrôlable est bien
        // au niveau voulu (un échec d'écriture n'est jamais mémorisé comme appliqué).
        if let (Some(m), false) = (after.applied_master, hardware_dirty) {
            for (display, state, level, range) in &after.displays {
                if *state == DisplayState::Controllable {
                    let expected = range.display_level(i64::from(m));
                    prop_assert_eq!(*level, Some(expected), "écran {}", display);
                    prop_assert_eq!(w.level_of(display.as_str()), expected);
                }
            }
        }
        for write in w.log[log_len..].iter().filter(|wr| !wr.ok) {
            let later = w.log[log_len..]
                .iter()
                .any(|o| o.display == write.display && o.at_ms >= write.at_ms && o != write);
            let entry = after
                .displays
                .iter()
                .find(|d| d.0 == write.display)
                .unwrap();
            if !later {
                prop_assert_ne!(entry.2, Some(write.level), "échec mémorisé comme appliqué");
            }
        }

        // Invariant 6 : après un réveil, l'état final suit la priorité (confluent).
        if wake_checked {
            let now = w.clock.now();
            for (display, state, _, range) in &after.displays {
                if *state != DisplayState::Controllable {
                    continue;
                }
                let expected = if after.dimmed {
                    Some(dim_level(*range))
                } else {
                    after.master.map(|m| range.display_level(i64::from(m)))
                };
                prop_assert_eq!(Some(w.level_of(display.as_str())), expected);
            }
            if after.mode == BaseMode::Planning && !after.dimmed && after.suspension.is_none() {
                let schedule = setup_schedule();
                prop_assert_eq!(after.master, schedule.value_at(now.minute));
            }
        }

        // Invariant 7 : le planning ne reprend jamais avant le prochain point (oracle
        // indépendant : arcs calculés ici, pas par `Suspension`) et, avec une
        // évaluation par minute, au plus tard à ce point.
        for (at, suspension) in w.resumes.drain(..) {
            let arc = |from: u16, to: u16| (i32::from(to) - i32::from(from)).rem_euclid(1440);
            let (start, resume) = (suspension.start().minutes(), suspension.resume().minutes());
            let target = arc(start, resume);
            prop_assert!(
                target == 0 || arc(start, at.minute.minutes()) >= target,
                "reprise trop tôt : suspendu {start}, reprise {resume}, levée à {}",
                at.minute
            );
        }
        if let Some(ms) = advanced_ms {
            let now = w.clock.now();
            if after.mode == BaseMode::Planning && !after.dimmed && ms >= 60_000 {
                if let Some(s) = after.suspension {
                    let arc =
                        |from: u16, to: u16| (i32::from(to) - i32::from(from)).rem_euclid(1440);
                    let (start, resume) = (s.start().minutes(), s.resume().minutes());
                    let target = arc(start, resume);
                    prop_assert!(
                        target != 0 && arc(start, now.minute.minutes()) < target,
                        "reprise en retard"
                    );
                }
            }
        }
    }
    Ok(())
}

fn setup_schedule() -> Schedule {
    Schedule::new(vec![
        Point {
            time: t("08:00"),
            level: 80,
        },
        Point {
            time: t("08:30"),
            level: 20,
        },
        Point {
            time: t("09:15"),
            level: 60,
        },
    ])
    .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn invariants_hold_for_any_event_sequence(actions in proptest::collection::vec(action(), 1..40)) {
        check_sequence(&actions)?;
    }
}
