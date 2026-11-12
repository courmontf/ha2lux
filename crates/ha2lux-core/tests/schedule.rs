//! T-SCH: schedule (spec §6).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::{MAX_POINTS, MinuteOfDay, Point, Schedule, ScheduleError, Suspension};

fn t(s: &str) -> MinuteOfDay {
    MinuteOfDay::parse(s).unwrap()
}

fn sched(points: &[(&str, u8)]) -> Schedule {
    Schedule::new(
        points
            .iter()
            .map(|&(h, level)| Point { time: t(h), level })
            .collect(),
    )
    .unwrap()
}

fn set_a() -> Schedule {
    sched(&[("08:00", 80), ("20:00", 20)])
}

fn set_b() -> Schedule {
    sched(&[("08:00", 70)])
}

#[test]
fn t_sch_01_single_point_constant() {
    assert_eq!(set_b().value_at(t("00:00")), Some(70));
    assert_eq!(set_b().value_at(t("23:00")), Some(70));
}

#[test]
fn t_sch_02_to_06() {
    let a = set_a();
    assert_eq!(a.value_at(t("08:00")), Some(80));
    assert_eq!(a.value_at(t("20:00")), Some(20));
    assert_eq!(a.value_at(t("14:00")), Some(50));
    assert_eq!(a.value_at(t("02:00")), Some(50));
    assert_eq!(a.value_at(t("23:00")), Some(35));
    assert_eq!(a.value_at(t("09:00")), Some(75));
    assert_eq!(a.value_at(t("12:00")), Some(60));
}

#[test]
fn t_sch_07_empty() {
    let s = Schedule::new(vec![]).unwrap();
    assert!(s.is_empty());
    assert_eq!(s.value_at(t("12:00")), None);
    assert_eq!(s.next_point_after(t("12:00")), None);
    assert_eq!(Suspension::begin(&s, t("12:00")), None);
}

#[test]
fn t_sch_08_09_rounding_ties_away_from_start() {
    assert_eq!(
        sched(&[("08:00", 10), ("08:02", 11)]).value_at(t("08:01")),
        Some(11)
    );
    assert_eq!(
        sched(&[("08:00", 10), ("08:02", 9)]).value_at(t("08:01")),
        Some(9)
    );
}

#[test]
fn t_sch_10_11_next_point() {
    let a = set_a();
    assert_eq!(a.next_point_after(t("10:00")), Some(t("20:00")));
    assert_eq!(a.next_point_after(t("22:00")), Some(t("08:00")));
    assert_eq!(a.next_point_after(t("08:00")), Some(t("20:00")));
}

#[test]
fn t_sch_12_resume() {
    let s = Suspension::from_parts(t("14:00"), t("20:00"));
    assert!(!s.should_resume(t("14:00")));
    assert!(!s.should_resume(t("19:00")));
    assert!(s.should_resume(t("20:00")));
}

#[test]
fn t_sch_13_resume_wraps_midnight() {
    let s = Suspension::from_parts(t("22:00"), t("08:00"));
    assert!(!s.should_resume(t("23:00")));
    assert!(s.should_resume(t("08:00")));
}

#[test]
fn t_sch_14_degenerate_immediate() {
    assert!(Suspension::from_parts(t("08:00"), t("08:00")).should_resume(t("08:00")));
    let b = set_b();
    let s = Suspension::begin(&b, t("08:00")).unwrap();
    assert_eq!(s.resume(), t("08:00"));
    assert!(s.should_resume(t("08:00")));
}

#[test]
fn t_sch_15_not_before_resume() {
    assert!(!Suspension::from_parts(t("22:00"), t("08:00")).should_resume(t("07:59")));
}

#[test]
fn suspension_begin_uses_next_point() {
    let s = Suspension::begin(&set_a(), t("14:00")).unwrap();
    assert_eq!((s.start(), s.resume()), (t("14:00"), t("20:00")));
}

#[test]
fn t_sch_16_parse() {
    assert_eq!(t("08:00").minutes(), 480);
    assert_eq!(t("23:30").minutes(), 1410);
    assert_eq!(t("00:00").minutes(), 0);
    assert_eq!(t("23:30").to_string(), "23:30");
}

#[test]
fn t_sch_17_rejects() {
    for bad in [
        "24:00", "8-00", "08:99", "8:00", "08:00 ", "", "ab:cd", "+8:00", "08:0é", "-1:00",
    ] {
        assert!(
            MinuteOfDay::parse(bad).is_err(),
            "{bad:?} aurait dû être rejeté"
        );
    }
    assert_eq!(
        MinuteOfDay::new(1440),
        Err(ScheduleError::MinuteOutOfRange(1440))
    );
}

#[test]
fn t_sch_18_sorted() {
    let s = Schedule::new(vec![
        Point {
            time: MinuteOfDay::new(1200).unwrap(),
            level: 1,
        },
        Point {
            time: MinuteOfDay::new(300).unwrap(),
            level: 2,
        },
        Point {
            time: MinuteOfDay::new(800).unwrap(),
            level: 3,
        },
    ])
    .unwrap();
    let minutes: Vec<u16> = s.points().iter().map(|p| p.time.minutes()).collect();
    assert_eq!(minutes, [300, 800, 1200]);
}

#[test]
fn t_sch_19_duplicate_rejected() {
    let p = |level| Point {
        time: t("08:00"),
        level,
    };
    assert_eq!(
        Schedule::new(vec![p(1), p(2)]),
        Err(ScheduleError::DuplicateTime(t("08:00")))
    );
}

#[test]
fn t_sch_20_too_many() {
    let make = |n: usize| {
        (0..n)
            .map(|i| Point {
                time: MinuteOfDay::new(i as u16 * 10).unwrap(),
                level: 50,
            })
            .collect()
    };
    assert!(Schedule::new(make(MAX_POINTS)).is_ok());
    assert_eq!(
        Schedule::new(make(MAX_POINTS + 1)),
        Err(ScheduleError::TooManyPoints(25))
    );
}

#[test]
fn level_over_100_rejected() {
    assert_eq!(
        Schedule::new(vec![Point {
            time: t("08:00"),
            level: 101
        }]),
        Err(ScheduleError::LevelOutOfRange(101))
    );
}

#[test]
fn interpolation_stays_within_bounds_and_hits_points() {
    let s = sched(&[("00:00", 0), ("12:00", 100), ("18:00", 40)]);
    let mut previous = 0u8;
    for m in 0..1440u16 {
        let v = s.value_at(MinuteOfDay::new(m).unwrap()).unwrap();
        assert!(v <= 100);
        if m <= 720 {
            assert!(v >= previous, "croissant jusqu'à midi");
            previous = v;
        }
    }
    assert_eq!(s.value_at(t("12:00")), Some(100));
    assert_eq!(s.value_at(t("18:00")), Some(40));
}
