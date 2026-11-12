//! Horloge virtuelle : le temps n'avance que sur demande, minuit boucle.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{Clock, MinuteOfDay, VirtualClock};

fn t(s: &str) -> MinuteOfDay {
    MinuteOfDay::parse(s).unwrap()
}

#[test]
fn starts_at_given_time_and_stays_put() {
    let clock = VirtualClock::new(t("14:00"));
    assert_eq!(clock.now().minute, t("14:00"));
    assert_eq!(clock.now().ms, 0);
    assert_eq!(
        clock.now(),
        clock.now(),
        "aucune dérive sans avance explicite"
    );
}

#[test]
fn advances_and_wraps_past_midnight() {
    let mut clock = VirtualClock::new(t("23:30"));
    clock.advance_ms(29 * 60_000 + 59_999);
    assert_eq!(clock.now().minute, t("23:59"));
    clock.advance_ms(1);
    assert_eq!(clock.now().minute, t("00:00"));
    clock.advance_ms(24 * 60 * 60_000);
    assert_eq!(clock.now().minute, t("00:00"));
}

#[test]
fn advance_to_never_goes_back() {
    let mut clock = VirtualClock::new(t("00:00"));
    clock.advance_to_ms(5_000);
    clock.advance_to_ms(1_000);
    assert_eq!(clock.now().ms, 5_000);
}
