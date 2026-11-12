//! Gestionnaire de rescan : T-DDC-03..07 et 09, en temps explicite (aucun sleep).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{RescanAction, RescanConfig, RescanManager, RescanTrigger, ScanOutcome};
use proptest::prelude::*;

fn manager() -> RescanManager {
    RescanManager::new(RescanConfig::default())
}

const ACCEPTED: ScanOutcome = ScanOutcome {
    accepted: true,
    waiting: false,
};
const WAITING: ScanOutcome = ScanOutcome {
    accepted: true,
    waiting: true,
};

/// Fait avancer le temps par pas de `step` ms jusqu'à `until`, en terminant chaque scan
/// immédiatement ; renvoie les instants de départ des scans.
fn run_until(m: &mut RescanManager, from: u64, until: u64, step: u64) -> Vec<u64> {
    let mut starts = Vec::new();
    let mut now = from;
    while now <= until {
        for RescanAction::Start { generation } in m.poll(now) {
            starts.push(now);
            assert!(m.finished(now, generation, ACCEPTED));
        }
        now += step;
    }
    starts
}

#[test]
fn startup_and_manual_are_immediate() {
    let mut m = manager();
    m.trigger(1_000, RescanTrigger::Startup, 0);
    assert_eq!(m.poll(1_000), [RescanAction::Start { generation: 1 }]);
    assert!(m.finished(1_000, 1, ACCEPTED));
    m.trigger(5_000, RescanTrigger::Manual, 0);
    assert_eq!(m.poll(5_000), [RescanAction::Start { generation: 2 }]);
}

#[test]
fn t_ddc_05_a_burst_of_events_gives_a_single_scan() {
    let mut m = manager();
    let mut starts = 0;
    // 20 événements en 300 ms (un tous les 15 ms), le gestionnaire est interrogé à chacun.
    for i in 0..20u64 {
        let now = i * 15;
        m.trigger(now, RescanTrigger::Hotplug, 0);
        for RescanAction::Start { generation } in m.poll(now) {
            starts += 1;
            m.finished(now, generation, ACCEPTED);
        }
    }
    assert_eq!(
        starts, 0,
        "rien tant que la rafale dure : débounce de 600 ms"
    );
    starts += run_until(&mut m, 300, 5_000, 10).len();
    assert_eq!(starts, 1, "un seul scan pour toute la rafale");
}

#[test]
fn the_debounce_is_trailing_and_capped() {
    let mut m = manager();
    // Des événements toutes les 200 ms pendant 10 s ne doivent pas repousser le scan indéfiniment.
    let mut first_start = None;
    for i in 0..50u64 {
        let now = i * 200;
        m.trigger(now, RescanTrigger::Resume, 0);
        for RescanAction::Start { generation } in m.poll(now) {
            first_start.get_or_insert(now);
            m.finished(now, generation, ACCEPTED);
        }
    }
    let start = first_start.expect("le plafond de report doit déclencher un scan");
    assert!(start <= 3_200, "plafond de report de 3 s : {start}");
    assert!(start >= 600, "le débounce est respecté : {start}");
}

#[test]
fn resume_waits_for_the_initial_delay_then_runs() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Resume, 1_500);
    assert!(m.poll(1_499).is_empty());
    assert_eq!(m.next_deadline(), Some(1_500));
    assert_eq!(m.poll(1_500).len(), 1);
}

#[test]
fn t_ddc_06_noisy_hotplug_is_floored_to_30_seconds_and_the_last_one_is_not_lost() {
    let mut m = manager();
    let mut starts = Vec::new();
    // Hotplug « bruyant » toutes les 5 s pendant 2 minutes.
    for second in (0..=120u64).step_by(5) {
        m.trigger(second * 1_000, RescanTrigger::Hotplug, 0);
        starts.extend(run_until(
            &mut m,
            second * 1_000,
            second * 1_000 + 4_999,
            100,
        ));
    }
    starts.extend(run_until(&mut m, 125_000, 200_000, 100));
    assert!(starts.len() >= 3, "{starts:?}");
    for pair in starts.windows(2) {
        assert!(
            pair[1] - pair[0] >= 30_000,
            "plancher de 30 s non respecté : {starts:?}"
        );
    }
    assert!(
        *starts.last().unwrap() >= 120_000,
        "le dernier événement n'est pas perdu : {starts:?}"
    );
    assert!(!m.has_pending());
}

#[test]
fn an_isolated_hotplug_is_not_floored_and_wake_is_never_floored() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Hotplug, 0);
    assert_eq!(
        run_until(&mut m, 0, 2_000, 100),
        [600],
        "premier hotplug : seulement le débounce"
    );
    // Deux réveils rapprochés : aucun plancher.
    m.trigger(3_000, RescanTrigger::Resume, 0);
    assert_eq!(run_until(&mut m, 3_000, 4_000, 100), [3_600]);
    m.trigger(5_000, RescanTrigger::Resume, 0);
    assert_eq!(run_until(&mut m, 5_000, 6_000, 100), [5_600]);
    // Un hotplug mêlé à un réveil n'est plus « seulement hotplug » : pas de plancher.
    m.trigger(7_000, RescanTrigger::Hotplug, 0);
    m.trigger(7_100, RescanTrigger::Resume, 0);
    assert_eq!(run_until(&mut m, 7_000, 9_000, 100), [7_700]);
}

#[test]
fn only_one_scan_at_a_time_and_the_request_is_kept() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Manual, 0);
    assert_eq!(m.poll(0).len(), 1);
    m.trigger(100, RescanTrigger::Manual, 0);
    assert!(
        m.poll(100).is_empty() && m.poll(5_000).is_empty(),
        "le scan en cours n'est pas doublé"
    );
    assert!(m.finished(5_000, 1, ACCEPTED));
    assert_eq!(
        m.poll(5_000),
        [RescanAction::Start { generation: 2 }],
        "la demande conservée part ensuite"
    );
}

#[test]
fn t_ddc_07_a_hung_scan_is_disavowed_and_its_late_result_discarded() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Manual, 0);
    assert_eq!(m.poll(0), [RescanAction::Start { generation: 1 }]);
    assert!(m.poll(9_999).is_empty());
    assert_eq!(m.next_deadline(), Some(10_000));
    assert!(
        m.poll(10_000).is_empty(),
        "désavoué à 10 s ; relance prévue 2 s plus tard"
    );
    assert!(!m.is_running());
    assert!(!m.finished(10_500, 1, ACCEPTED), "résultat tardif jeté");
    assert_eq!(m.poll(12_000), [RescanAction::Start { generation: 2 }]);
}

#[test]
fn t_ddc_07_three_consecutive_disavowals_halt_until_a_new_event() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Manual, 0);
    let mut now = 0;
    let mut starts = 0;
    for _ in 0..200 {
        starts += m.poll(now).len();
        now += 1_000;
    }
    assert_eq!(starts, 3, "3 scans désavoués puis arrêt");
    assert!(m.is_halted());
    assert_eq!(
        m.next_deadline(),
        None,
        "plus aucune échéance : on attend un événement"
    );
    m.trigger(now, RescanTrigger::Hotplug, 0);
    assert!(!m.is_halted());
    assert!(m.next_deadline().is_some());
}

#[test]
fn t_ddc_03_waiting_displays_are_retried_at_2_5_10_20_then_every_60_seconds() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Startup, 0);
    let mut now = 0;
    let mut gaps = Vec::new();
    for _ in 0..8 {
        let actions = m.poll(now);
        let RescanAction::Start { generation } = actions[0];
        assert!(m.finished(now, generation, WAITING));
        let due = m.next_deadline().expect("relance planifiée");
        gaps.push(due - now);
        now = due;
    }
    assert_eq!(
        gaps,
        [2_000, 5_000, 10_000, 20_000, 60_000, 60_000, 60_000, 60_000]
    );
}

#[test]
fn t_ddc_04_displays_that_never_answered_are_not_retried_periodically() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Startup, 0);
    let RescanAction::Start { generation } = m.poll(0)[0];
    m.finished(0, generation, ACCEPTED);
    assert_eq!(m.next_deadline(), None);
    assert!(run_until(&mut m, 0, 600_000, 5_000).is_empty());
    // Un événement (branchement, déverrouillage, demande manuelle) relance bien.
    m.trigger(600_000, RescanTrigger::Resume, 0);
    assert_eq!(run_until(&mut m, 600_000, 602_000, 100).len(), 1);
}

#[test]
fn recovery_resets_the_waiting_ladder() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Startup, 0);
    let RescanAction::Start { generation } = m.poll(0)[0];
    m.finished(0, generation, WAITING);
    m.poll(2_000);
    m.finished(2_000, 2, WAITING);
    assert_eq!(m.next_deadline(), Some(7_000));
    m.poll(7_000);
    m.finished(7_000, 3, ACCEPTED);
    assert_eq!(
        m.next_deadline(),
        None,
        "l'écran est revenu : plus de relance"
    );
    m.trigger(10_000, RescanTrigger::Manual, 0);
    m.poll(10_000);
    m.finished(10_000, 4, WAITING);
    assert_eq!(
        m.next_deadline(),
        Some(12_000),
        "l'échelle repart du premier palier"
    );
}

#[test]
fn a_rejected_list_does_not_start_the_waiting_ladder() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Startup, 0);
    let RescanAction::Start { generation } = m.poll(0)[0];
    m.finished(
        0,
        generation,
        ScanOutcome {
            accepted: false,
            waiting: true,
        },
    );
    assert_eq!(
        m.next_deadline(),
        None,
        "la machine d'états gère elle-même les relances d'une liste factice"
    );
}

#[test]
fn t_ddc_09_retries_are_deferred_while_the_interface_is_open() {
    let mut m = manager();
    m.trigger(0, RescanTrigger::Startup, 0);
    let RescanAction::Start { generation } = m.poll(0)[0];
    m.finished(0, generation, WAITING);
    m.set_ui_open(true);
    assert_eq!(m.next_deadline(), None);
    assert!(
        run_until(&mut m, 0, 120_000, 1_000).is_empty(),
        "aucune relance tant que l'interface est ouverte"
    );
    m.set_ui_open(false);
    assert_eq!(m.poll(120_000).len(), 1, "relance dès la fermeture");
    // Une demande de l'utilisateur, elle, n'est pas reportée par l'interface ouverte.
    m.finished(120_000, 2, ACCEPTED);
    m.set_ui_open(true);
    m.trigger(130_000, RescanTrigger::Manual, 0);
    assert_eq!(m.poll(130_000).len(), 1);
}

#[test]
fn suspension_keeps_requests_and_resumes() {
    let mut m = manager();
    m.set_suspended(true);
    m.trigger(0, RescanTrigger::Resume, 0);
    assert!(m.poll(10_000).is_empty());
    assert_eq!(m.next_deadline(), None);
    m.set_suspended(false);
    assert_eq!(
        m.poll(10_000).len(),
        1,
        "la demande reçue pendant la veille est honorée au réveil"
    );
}

#[derive(Clone, Debug)]
enum Step {
    Trigger(u8),
    Poll(u16),
    Finish(bool, bool),
    Ui(bool),
    Suspend(bool),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0u8..4).prop_map(Step::Trigger),
        (0u16..4_000).prop_map(Step::Poll),
        (any::<bool>(), any::<bool>()).prop_map(|(a, w)| Step::Finish(a, w)),
        any::<bool>().prop_map(Step::Ui),
        any::<bool>().prop_map(Step::Suspend),
    ]
}

proptest! {
    #[test]
    fn never_two_scans_at_once_and_never_while_halted_or_suspended(steps in proptest::collection::vec(step(), 1..120)) {
        let mut m = manager();
        let mut now = 0u64;
        let mut running: Option<u64> = None;
        let mut suspended = false;
        for s in steps {
            match s {
                Step::Trigger(k) => {
                    let t = [RescanTrigger::Startup, RescanTrigger::Manual, RescanTrigger::Resume, RescanTrigger::Hotplug][usize::from(k)];
                    m.trigger(now, t, 0);
                }
                Step::Poll(dt) => {
                    now += u64::from(dt);
                    let actions = m.poll(now);
                    // Un désaveu éventuel libère le scan courant.
                    if !m.is_running() { running = None; }
                    for RescanAction::Start { generation } in actions {
                        prop_assert!(running.is_none(), "deux scans simultanés");
                        prop_assert!(!suspended, "scan lancé pendant la suspension");
                        running = Some(generation);
                    }
                }
                Step::Finish(accepted, waiting) => {
                    if let Some(generation) = running.take() {
                        m.finished(now, generation, ScanOutcome { accepted, waiting });
                    }
                }
                Step::Ui(open) => m.set_ui_open(open),
                Step::Suspend(on) => { suspended = on; m.set_suspended(on); }
            }
            if m.is_halted() {
                prop_assert!(m.next_deadline().is_none() || m.is_running(), "arrêté : plus d'échéance");
            }
        }
    }
}
