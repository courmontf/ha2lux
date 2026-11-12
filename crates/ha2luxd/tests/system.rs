//! Événements système et gestionnaire de rescan dans le démon (T-DDC-03..09), temps virtuel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Duration;

use ha2lux_core::DisplayKind;
use ha2lux_ipc::DisplayDto;
use ha2luxd::actor::SystemEvent;
use ha2luxd::api::{Reply, Request};
use ha2luxd::hardware::MockHardware;
use support::Rig;

async fn list(rig: &Rig) -> Vec<DisplayDto> {
    match rig.ask(Request::List).await.unwrap() {
        Reply::List(list) => list,
        other => panic!("{other:?}"),
    }
}

fn state(list: &[DisplayDto], id: &str) -> String {
    list.iter()
        .find(|d| d.id == id)
        .map_or("absent".to_owned(), |d| d.state.clone())
}

async fn two_screens() -> (
    Rig,
    std::sync::Arc<MockHardware>,
    ha2lux_backends::mock::MockControl,
    ha2lux_backends::mock::MockControl,
) {
    let hardware = MockHardware::new();
    let a = hardware.add("A", DisplayKind::External, 100, 50);
    let b = hardware.add("B", DisplayKind::External, 100, 50);
    let rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    (rig, hardware, a, b)
}

#[tokio::test(start_paused = true)]
async fn no_hardware_write_while_the_machine_is_suspending_and_restore_on_resume() {
    let (rig, _, a, _) = two_screens().await;
    rig.ok(Request::SetMaster(40)).await;
    assert_eq!(a.raw(), 40);
    rig.handle.send_system(SystemEvent::Suspend).await.unwrap();
    rig.settle().await;
    rig.ok(Request::SetMaster(70)).await;
    rig.advance(Duration::from_secs(3)).await;
    assert_eq!(
        a.raw(),
        40,
        "DDC en veille = blocage : aucune écriture pendant la préparation de veille"
    );
    a.set_raw(100); // l'écran se remet à 100 % au réveil
    rig.handle.send_system(SystemEvent::Resume).await.unwrap();
    rig.advance(Duration::from_millis(1_700)).await;
    assert_eq!(
        a.raw(),
        70,
        "le maître décidé pendant la veille est appliqué au réveil"
    );
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_resets_late_after_wake_is_corrected_by_the_next_scan() {
    // Constaté sur du vrai matériel : des moniteurs DisplayPort retrouvent leur luminosité
    // d'usine plusieurs secondes après la réapplication du réveil.
    let (rig, _, a, _) = two_screens().await;
    rig.ok(Request::SetMaster(40)).await;
    rig.handle.send_system(SystemEvent::Suspend).await.unwrap();
    rig.settle().await;
    rig.handle.send_system(SystemEvent::Resume).await.unwrap();
    rig.advance(Duration::from_millis(1_700)).await;
    assert_eq!(a.raw(), 40);
    rig.advance(Duration::from_secs(4)).await;
    a.set_raw(100); // réinitialisation tardive, sans aucun événement de branchement
    rig.advance(Duration::from_secs(12)).await;
    assert_eq!(
        a.raw(),
        40,
        "la dérive est corrigée par la vérification périodique, sans événement"
    );

    // Hors de la fenêtre, un réglage fait sur le moniteur est respecté.
    rig.advance(Duration::from_secs(90)).await;
    a.set_raw(55);
    rig.handle.send_system(SystemEvent::Hotplug).await.unwrap();
    rig.advance(Duration::from_secs(40)).await;
    assert_eq!(
        a.raw(),
        55,
        "pas de correction hors de la fenêtre de réveil"
    );
}

#[tokio::test(start_paused = true)]
async fn lock_does_nothing_and_unlock_reapplies() {
    let (rig, hardware, a, _) = two_screens().await;
    rig.ok(Request::SetMaster(45)).await;
    let before = hardware.discoveries();
    rig.handle.send_system(SystemEvent::Lock).await.unwrap();
    rig.advance(Duration::from_secs(5)).await;
    assert_eq!(
        hardware.discoveries(),
        before,
        "verrouillage : rien (comme l'extinction d'écran)"
    );
    a.set_raw(100);
    rig.handle.send_system(SystemEvent::Unlock).await.unwrap();
    rig.advance(Duration::from_millis(1_700)).await;
    assert_eq!(a.raw(), 45);
    assert!(
        hardware.discoveries() > before,
        "déverrouillage : rescan débouncé"
    );
}

#[tokio::test(start_paused = true)]
async fn session_becoming_inactive_pauses_control_and_active_resumes() {
    let (rig, _, a, _) = two_screens().await;
    rig.ok(Request::SetMaster(30)).await;
    rig.handle
        .send_system(SystemEvent::SessionActive(false))
        .await
        .unwrap();
    rig.settle().await;
    rig.ok(Request::SetMaster(80)).await;
    rig.advance(Duration::from_secs(2)).await;
    assert_eq!(
        a.raw(),
        30,
        "session inactive : on ne pilote plus les écrans"
    );
    rig.handle
        .send_system(SystemEvent::SessionActive(true))
        .await
        .unwrap();
    rig.advance(Duration::from_millis(1_700)).await;
    assert_eq!(a.raw(), 80);
}

#[tokio::test(start_paused = true)]
async fn t_ddc_05_a_burst_of_hotplug_events_gives_a_single_scan() {
    let (rig, hardware, _, _) = two_screens().await;
    let before = hardware.discoveries();
    for _ in 0..20 {
        rig.handle.send_system(SystemEvent::Hotplug).await.unwrap();
        rig.advance(Duration::from_millis(15)).await;
    }
    assert_eq!(
        hardware.discoveries(),
        before,
        "débounce : rien pendant la rafale"
    );
    rig.advance(Duration::from_secs(2)).await;
    assert_eq!(
        hardware.discoveries(),
        before + 1,
        "un seul scan pour 20 événements en 300 ms"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hotplug_adds_the_new_display() {
    let (rig, hardware, _, _) = two_screens().await;
    let c = hardware.add("C", DisplayKind::External, 100, 10);
    rig.handle.send_system(SystemEvent::Hotplug).await.unwrap();
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(state(&list(&rig).await, "C"), "controllable");
    assert_eq!(c.raw(), 50, "reçoit le maître");
}

#[tokio::test(start_paused = true)]
async fn t_ddc_06_noisy_hotplug_is_floored_to_30_seconds() {
    let (rig, hardware, _, _) = two_screens().await;
    let start = hardware.discoveries();
    let mut counts = Vec::new();
    for _ in 0..13 {
        rig.handle.send_system(SystemEvent::Hotplug).await.unwrap();
        rig.advance(Duration::from_secs(5)).await;
        counts.push(hardware.discoveries() - start);
    }
    // 65 s de hotplug toutes les 5 s : au plus un scan par fenêtre de 30 s (+ le premier).
    assert!(
        *counts.last().unwrap() <= 3,
        "plancher de 30 s : {counts:?}"
    );
    assert!(
        *counts.last().unwrap() >= 2,
        "les scans ont bien lieu : {counts:?}"
    );
    rig.advance(Duration::from_secs(40)).await;
    let settled = hardware.discoveries() - start;
    assert!(
        settled >= *counts.last().unwrap(),
        "le dernier événement n'est pas perdu"
    );
}

#[tokio::test(start_paused = true)]
async fn t_ddc_03_unavailable_display_is_retried_on_the_waiting_ladder_until_it_answers() {
    let (rig, hardware, _, _) = two_screens().await;
    hardware.set_responding("B", false);
    rig.ok(Request::Rescan).await;
    assert_eq!(
        state(&list(&rig).await, "B"),
        "unavailable",
        "connu mais muet : en attente"
    );
    let base = hardware.discoveries();
    let mut seen = Vec::new();
    for _ in 0..140 {
        rig.advance(Duration::from_secs(1)).await;
        seen.push(hardware.discoveries() - base);
    }
    // Relances à +2 s, +5 s, +10 s, +20 s, puis toutes les 60 s : 4 scans en 37 s, un de plus à ~97 s.
    let at = |s: usize| seen[s - 1];
    assert_eq!(at(1), 0);
    assert_eq!(at(3), 1, "{seen:?}");
    assert_eq!(at(9), 2, "{seen:?}");
    assert_eq!(at(20), 3, "{seen:?}");
    assert_eq!(at(41), 4, "{seen:?}");
    assert_eq!(at(100), 5, "{seen:?}");
    hardware.set_responding("B", true);
    rig.advance(Duration::from_secs(61)).await;
    assert_eq!(
        state(&list(&rig).await, "B"),
        "controllable",
        "il répond : relancé tout seul"
    );
    let calm = hardware.discoveries();
    rig.advance(Duration::from_secs(600)).await;
    assert_eq!(hardware.discoveries(), calm, "revenu : plus aucune relance");
}

#[tokio::test(start_paused = true)]
async fn t_ddc_04_a_display_that_never_answered_is_not_retried() {
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let dead = hardware.add("DEAD", DisplayKind::External, 100, 50);
    drop(dead);
    hardware.set_responding("DEAD", false);
    let rig = Rig::start_with(hardware.clone(), None, None).await.unwrap();
    assert_eq!(state(&list(&rig).await, "DEAD"), "not-controllable");
    let calm = hardware.discoveries();
    rig.advance(Duration::from_secs(600)).await;
    assert_eq!(
        hardware.discoveries(),
        calm,
        "aucune relance périodique : seulement sur événement"
    );
    rig.handle.send_system(SystemEvent::Hotplug).await.unwrap();
    rig.advance(Duration::from_secs(1)).await;
    assert_eq!(
        hardware.discoveries(),
        calm + 1,
        "un événement relance bien"
    );
}

#[tokio::test(start_paused = true)]
async fn t_ddc_09_retries_wait_while_the_interface_is_open() {
    let (rig, hardware, _, _) = two_screens().await;
    hardware.set_responding("B", false);
    rig.ok(Request::Rescan).await;
    rig.ok(Request::SetUiOpen(true)).await;
    let base = hardware.discoveries();
    rig.advance(Duration::from_secs(300)).await;
    assert_eq!(
        hardware.discoveries(),
        base,
        "interface ouverte : aucune relance (une re-création fermerait la fenêtre)"
    );
    rig.ok(Request::Rescan).await; // la demande de l'utilisateur, elle, n'est pas reportée
    assert_eq!(hardware.discoveries(), base + 1);
    rig.ok(Request::SetUiOpen(false)).await;
    rig.advance(Duration::from_secs(70)).await;
    assert!(
        hardware.discoveries() > base + 1,
        "relance dès la fermeture"
    );
}

#[tokio::test(start_paused = true)]
async fn t_ddc_07_a_hung_enumeration_is_disavowed_then_halted_until_a_new_event() {
    let (rig, hardware, a, _) = two_screens().await;
    rig.ok(Request::SetMaster(60)).await;
    let before = list(&rig).await;
    hardware.set_discover_delay(Duration::from_secs(3_600)); // l'énumération ne finit jamais
    let base = hardware.discoveries();
    rig.ok(Request::Rescan).await;
    rig.advance(Duration::from_secs(120)).await;
    assert_eq!(
        hardware.discoveries() - base,
        3,
        "3 scans désavoués (10 s chacun) puis arrêt"
    );
    rig.advance(Duration::from_secs(600)).await;
    assert_eq!(
        hardware.discoveries() - base,
        3,
        "arrêt : plus aucun scan sans nouvel événement"
    );
    assert_eq!(
        list(&rig).await,
        before,
        "les anciens écrans sont conservés"
    );
    assert_eq!(a.raw(), 60, "le matériel n'a pas été perturbé");
    hardware.set_discover_delay(Duration::ZERO);
    rig.handle.send_system(SystemEvent::Hotplug).await.unwrap();
    rig.advance(Duration::from_secs(2)).await;
    assert_eq!(
        hardware.discoveries() - base,
        4,
        "un nouvel événement relance les scans"
    );
    assert_eq!(state(&list(&rig).await, "A"), "controllable");
}
