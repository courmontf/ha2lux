//! Menu, molette et OSD : logique pure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ha2lux_ipc::DisplayDto;
use ha2lux_tray::model::{Action, Entry, TrayState, build_menu, icon_name, tooltip};
use ha2lux_tray::osd;
use ha2lux_tray::wheel::Wheel;

fn display(id: &str, name: &str, state: &str, level: i32) -> DisplayDto {
    DisplayDto {
        id: id.into(),
        name: name.into(),
        kind: "external".into(),
        state: state.into(),
        level,
        low: 0,
        high: 100,
        max_raw: 100,
    }
}

fn state() -> TrayState {
    TrayState {
        reachable: true,
        master: Some(55),
        mode: "static".into(),
        step: 5,
        schedule_points: 2,
        idle_level: 10,
        idle_minutes: 3,
        displays: vec![
            display("A", "DELL", "controllable", 40),
            display("B", "ASUS", "unavailable", -1),
        ],
        presets: vec![("Nuit".into(), 20), ("Jour".into(), 90)],
        ..TrayState::default()
    }
}

fn flat(entries: &[Entry], out: &mut Vec<Entry>) {
    for entry in entries {
        out.push(entry.clone());
        if let Entry::Sub { entries, .. } = entry {
            flat(entries, out);
        }
    }
}

fn labels(menu: &[Entry]) -> Vec<String> {
    let mut all = Vec::new();
    flat(menu, &mut all);
    all.into_iter()
        .filter_map(|e| match e {
            Entry::Item { label, .. } | Entry::Check { label, .. } | Entry::Sub { label, .. } => {
                Some(label)
            }
            Entry::Separator => None,
        })
        .collect()
}

fn find<'a>(menu: &'a [Entry], needle: &str) -> &'a Entry {
    menu.iter()
        .find(|e| match e {
            Entry::Item { label, .. } | Entry::Check { label, .. } | Entry::Sub { label, .. } => {
                label.contains(needle)
            }
            Entry::Separator => false,
        })
        .unwrap_or_else(|| panic!("{needle} absent de {:?}", labels(menu)))
}

#[test]
fn menu_lists_presets_with_values_and_displays() {
    let menu = build_menu(&state());
    let text = labels(&menu);
    assert!(text.contains(&"Nuit — 20 %".to_owned()));
    assert!(text.contains(&"Jour — 90 %".to_owned()));
    assert!(text.contains(&"DELL (40 %)".to_owned()));
    assert!(text.contains(&"ASUS (indisponible)".to_owned()));
    assert!(text.contains(&"Luminosité : 55 %".to_owned()));
    assert!(
        text.iter()
            .any(|l| l.contains("Atténuation") && l.contains("10 % après 3 min"))
    );
    assert!(matches!(
        find(&menu, "Nuit"),
        Entry::Item { action: Some(Action::Preset(name)), enabled: true, .. } if name == "Nuit"
    ));
}

#[test]
fn display_submenu_sets_that_displays_own_level() {
    let menu = build_menu(&state());
    let Entry::Sub { entries, .. } = find(&menu, "DELL") else {
        panic!("sous-menu attendu")
    };
    assert!(entries.iter().any(|e| matches!(
        e,
        Entry::Check { label, checked: true, action: Action::SetDisplay { id, level: 40 }, .. }
            if label == "40 %" && id == "A"
    )));
    let mut s = state();
    s.master = Some(52);
    let menu = build_menu(&s);
    let Entry::Sub { entries, .. } = find(&menu, "Tous les écrans") else {
        panic!("sous-menu attendu")
    };
    assert!(entries.iter().any(|e| matches!(
        e,
        Entry::Check {
            action: Action::SetMaster(50),
            checked: true,
            ..
        }
    )));
}

#[test]
fn schedule_is_greyed_while_auto_drives() {
    let mut s = state();
    s.auto_enabled = true;
    s.auto_configured = true;
    s.ha_health = "ok".into();
    s.schedule_enabled = true;
    let menu = build_menu(&s);
    let Entry::Check { label, enabled, .. } = find(&menu, "Planning") else {
        panic!()
    };
    assert!(!enabled);
    assert!(label.contains("sert si Home Assistant est hors ligne"));
    assert!(labels(&menu).contains(&"capteur : à jour".to_owned()));
}

#[test]
fn auto_is_disabled_when_not_configured_and_schedule_needs_points() {
    let mut s = state();
    s.schedule_points = 0;
    let menu = build_menu(&s);
    assert!(matches!(
        find(&menu, "Auto-luminosité"),
        Entry::Check { enabled: false, .. }
    ));
    assert!(matches!(
        find(&menu, "Planning"),
        Entry::Check { enabled: false, .. }
    ));
    s.schedule_points = 2;
    assert!(matches!(
        find(&build_menu(&s), "Planning"),
        Entry::Check {
            enabled: true,
            action: Action::Toggle("schedule", true),
            ..
        }
    ));
}

#[test]
fn suspended_schedule_shows_its_resume_time() {
    let mut s = state();
    s.schedule_enabled = true;
    s.schedule_resume = Some("20:00".into());
    assert!(labels(&build_menu(&s)).contains(&"Planning suspendu jusqu'à 20:00".to_owned()));
}

/// Lignes du sous-menu « À propos » (la dernière entrée avant « Quitter »).
fn about_lines(s: &TrayState) -> Vec<String> {
    let menu = build_menu(s);
    match find(&menu, "À propos") {
        Entry::Sub { entries, .. } => entries
            .iter()
            .filter_map(|e| match e {
                Entry::Item { label, enabled, .. } => {
                    assert!(!enabled, "les lignes d'information sont grisées");
                    Some(label.clone())
                }
                _ => None,
            })
            .collect(),
        other => panic!("« À propos » devrait être un sous-menu : {other:?}"),
    }
}

#[test]
fn about_shows_the_tray_and_daemon_versions() {
    let version = env!("CARGO_PKG_VERSION");
    let mut s = state();
    s.daemon_version = version.to_owned();
    let lines = about_lines(&s);
    assert_eq!(
        lines,
        [
            format!("Icône ha2lux-tray {version}"),
            format!("Démon ha2luxd {version}")
        ]
    );
}

#[test]
fn about_warns_when_the_daemon_runs_another_version() {
    let mut s = state();
    s.daemon_version = "0.0.1".to_owned();
    let lines = about_lines(&s);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[1].contains("0.0.1"));
    assert!(lines[2].contains("redémarrez le démon"));
}

#[test]
fn about_handles_an_unknown_daemon_version() {
    let lines = about_lines(&state());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains("version inconnue"));
}

#[test]
fn menu_offers_to_reload_the_configuration() {
    let menu = build_menu(&state());
    let Entry::Item {
        action, enabled, ..
    } = find(&menu, "Recharger la configuration")
    else {
        panic!("entrée de rechargement attendue");
    };
    assert!(*enabled);
    assert_eq!(action, &Some(Action::ReloadConfig));
}

#[test]
fn unreachable_daemon_reduces_the_menu_to_a_message_and_quit() {
    let s = TrayState::default();
    let menu = build_menu(&s);
    let text = labels(&menu);
    // Message, « À propos » (version de l'icône + démon injoignable), puis Quitter.
    assert_eq!(text.len(), 5, "{text:?}");
    assert!(text[0].contains("ne répond pas") && text[1] == "À propos");
    assert!(text[2].contains(env!("CARGO_PKG_VERSION")), "{text:?}");
    assert_eq!(text[4], "Quitter");
    assert_eq!(icon_name(&s), "dialog-warning-symbolic");
    assert_eq!(tooltip(&s), "démon injoignable");
}

#[test]
fn tooltip_mentions_level_mode_and_dimming() {
    let mut s = state();
    s.dimmed = true;
    assert_eq!(tooltip(&s), "55 % · static · atténué");
}

#[test]
fn wheel_notches_are_summed_into_a_single_step() {
    let mut wheel = Wheel::default();
    assert!(!wheel.pending());
    for _ in 0..4 {
        wheel.push(1);
    }
    wheel.push(-1);
    assert!(wheel.pending());
    assert_eq!(wheel.take(5), Some(15));
    assert!(!wheel.pending());
    assert_eq!(wheel.take(5), None);
    wheel.push(1);
    wheel.push(-1);
    assert_eq!(
        wheel.take(5),
        None,
        "des crans qui s'annulent n'envoient rien"
    );
    wheel.push(1000);
    assert_eq!(wheel.take(5), Some(100), "variation bornée à 100");
    wheel.push(-1000);
    assert_eq!(wheel.take(100), Some(-100));
}

#[test]
fn osd_notification_is_replaceable_and_shows_a_progress_bar() {
    let hints = osd::hints(70);
    assert_eq!(
        hints.get("value").map(|v| format!("{v:?}")),
        Some("I32(70)".to_owned())
    );
    assert!(hints.contains_key("x-canonical-private-synchronous"));
    assert_eq!(
        osd::hints(250).get("value").map(|v| format!("{v:?}")),
        Some("I32(100)".to_owned())
    );
    assert_eq!(osd::summary(70), "Luminosité 70 %");
}

#[test]
fn the_icon_is_drawn_by_the_program_and_never_empty() {
    use ha2lux_tray::icon::sun_argb;
    for size in [22_u32, 32, 48, 64] {
        let data = sun_argb(size, true);
        assert_eq!(data.len(), (size * size * 4) as usize);
        let opaque = data.chunks(4).filter(|p| p[0] > 200).count();
        let clear = data.chunks(4).filter(|p| p[0] == 0).count();
        assert!(opaque > 10, "{size} : disque visible");
        assert!(clear > 10, "{size} : fond transparent");
        // Le centre est plein, le coin est transparent.
        let at = |x: u32, y: u32| data[((y * size + x) * 4) as usize];
        assert_eq!(at(size / 2, size / 2), 255);
        assert_eq!(at(0, 0), 0);
    }
    assert_ne!(
        sun_argb(22, true),
        sun_argb(22, false),
        "grisé si le démon est injoignable"
    );
}
