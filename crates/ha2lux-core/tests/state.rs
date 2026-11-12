//! State file: tolerant reading, round trip, nothing ephemeral.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::config::ConfigError;
use ha2lux_core::{Curve, LearnPolicy, MinuteOfDay, State, Suspension};

fn t(s: &str) -> MinuteOfDay {
    MinuteOfDay::parse(s).unwrap()
}

#[test]
fn empty_file_gives_empty_state() {
    let (state, issues) = State::parse("").unwrap();
    assert_eq!(state, State::default());
    assert!(issues.is_empty());
}

#[test]
fn round_trip_keeps_everything() {
    let mut curve = Curve::new();
    curve.learn(5.0, 30, None, &LearnPolicy::UNGUARDED);
    let state = State {
        master: Some(42),
        suspension: Some(Suspension::from_parts(t("14:00"), t("20:00"))),
        curve,
        last_display: Some("DEL-A0B1-12345678".to_owned()),
    };
    let (back, issues) = State::parse(&state.to_toml().unwrap()).unwrap();
    assert_eq!(back, state);
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn faulty_fields_are_ignored_individually() {
    let (state, issues) = State::parse(
        "master = 300\nsuspension = { start = \"25:00\", resume = \"08:00\" }\ncurve = 5\nlast_display = \"\"\n",
    )
    .unwrap();
    assert_eq!(state, State::default());
    assert_eq!(issues.len(), 4, "{issues:?}");
    let (state, _) = State::parse("master = -1").unwrap();
    assert_eq!(state.master, None);
    let (state, issues) = State::parse("master = \"x\"\ncurve = \"10:20,garbage,30:40\"").unwrap();
    assert_eq!(state.master, None);
    assert_eq!(state.curve.points().len(), 2);
    assert_eq!(issues.len(), 1);
}

#[test]
fn newer_version_and_syntax_errors_are_fatal() {
    assert_eq!(
        State::parse("version = 9\nmaster = 5"),
        Err(ConfigError::UnsupportedVersion {
            found: 9,
            supported: 1
        })
    );
    assert!(matches!(
        State::parse("master = ["),
        Err(ConfigError::Syntax { .. })
    ));
}

#[test]
fn nothing_ephemeral_is_persisted() {
    let text = State {
        master: Some(1),
        ..State::default()
    }
    .to_toml()
    .unwrap();
    for forbidden in ["dim", "ramp", "health", "lux", "token"] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} ne doit pas être persisté : {text}"
        );
    }
}

#[test]
fn master_boundaries() {
    for (raw, want) in [(0, Some(0)), (100, Some(100)), (101, None)] {
        assert_eq!(
            State::parse(&format!("master = {raw}")).unwrap().0.master,
            want
        );
    }
}
