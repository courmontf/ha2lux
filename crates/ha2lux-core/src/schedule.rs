//! Hourly schedule: points, interpolation, suspension (spec §6).

use std::fmt;

use thiserror::Error;

/// Maximum number of points in a schedule.
pub const MAX_POINTS: usize = 24;

const DAY: i64 = 1440;

/// Schedule construction or validation errors.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ScheduleError {
    /// Malformed time: only strict `HH:MM` (24 h) is accepted.
    #[error("invalid time \"{0}\": expected format HH:MM (00:00 to 23:59)")]
    InvalidTime(String),
    /// Minute outside `0..1440`.
    #[error("invalid minute of the day: {0} (expected 0 to 1439)")]
    MinuteOutOfRange(u16),
    /// Level outside `0..=100`.
    #[error("invalid level: {0} (expected 0 to 100)")]
    LevelOutOfRange(u8),
    /// Two points at the same minute.
    #[error("two points at the same time: {0}")]
    DuplicateTime(MinuteOfDay),
    /// Too many points.
    #[error("trop de points : {0} (maximum {MAX_POINTS})")]
    TooManyPoints(usize),
}

/// Minute of the day, `0..=1439`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    /// Builds a minute of the day; rejects `>= 1440`.
    pub fn new(minute: u16) -> Result<Self, ScheduleError> {
        if i64::from(minute) < DAY {
            Ok(Self(minute))
        } else {
            Err(ScheduleError::MinuteOutOfRange(minute))
        }
    }

    /// Strict parsing of `HH:MM`: exactly 5 characters, no space or
    /// sign, `HH ≤ 23`, `MM ≤ 59`.
    pub fn parse(text: &str) -> Result<Self, ScheduleError> {
        let invalid = || ScheduleError::InvalidTime(text.to_owned());
        let bytes = text.as_bytes();
        if bytes.len() != 5 || bytes[2] != b':' {
            return Err(invalid());
        }
        let digit = |b: u8| -> Result<u16, ScheduleError> {
            if b.is_ascii_digit() {
                Ok(u16::from(b - b'0'))
            } else {
                Err(invalid())
            }
        };
        let hours = digit(bytes[0])? * 10 + digit(bytes[1])?;
        let minutes = digit(bytes[3])? * 10 + digit(bytes[4])?;
        if hours > 23 || minutes > 59 {
            return Err(invalid());
        }
        Ok(Self(hours * 60 + minutes))
    }

    /// Value in minutes since midnight.
    pub fn minutes(self) -> u16 {
        self.0
    }
}

impl fmt::Display for MinuteOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.0 / 60, self.0 % 60)
    }
}

/// Schedule point: a time and a master level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// Time of the point.
    pub time: MinuteOfDay,
    /// Master level 0-100.
    pub level: u8,
}

/// Validated schedule: sorted, no duplicate times, at most [`MAX_POINTS`] points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    points: Vec<Point>,
}

impl Schedule {
    /// Validates and sorts the points.
    pub fn new(mut points: Vec<Point>) -> Result<Self, ScheduleError> {
        if points.len() > MAX_POINTS {
            return Err(ScheduleError::TooManyPoints(points.len()));
        }
        if let Some(bad) = points.iter().find(|p| p.level > 100) {
            return Err(ScheduleError::LevelOutOfRange(bad.level));
        }
        points.sort_by_key(|p| p.time);
        if let Some(pair) = points.windows(2).find(|w| w[0].time == w[1].time) {
            return Err(ScheduleError::DuplicateTime(pair[0].time));
        }
        Ok(Self { points })
    }

    /// Points sorted by time.
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// True if the schedule has no points (nothing to apply).
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Master level at a given minute, or `None` with no points.
    ///
    /// Linear interpolation between consecutive points, wrapping from the
    /// last to the first point across midnight. Rounded to nearest;
    /// on an exact tie, the gap from the start point is rounded away from zero.
    pub fn value_at(&self, now: MinuteOfDay) -> Option<u8> {
        let first = *self.points.first()?;
        if self.points.len() == 1 {
            return Some(first.level);
        }
        let now = i64::from(now.0);
        // Last point whose time is <= now; otherwise, the last point of the previous day.
        let (start_minute, start) =
            match self.points.iter().rposition(|p| i64::from(p.time.0) <= now) {
                Some(i) => (i64::from(self.points[i].time.0), self.points[i]),
                None => {
                    let last = self.points[self.points.len() - 1];
                    (i64::from(last.time.0) - DAY, last)
                }
            };
        let next = self
            .points
            .iter()
            .find(|p| i64::from(p.time.0) > start_minute)
            .copied()
            .map(|p| (i64::from(p.time.0), p))
            .unwrap_or((i64::from(first.time.0) + DAY, first));
        let (end_minute, end) = next;
        let duration = end_minute - start_minute;
        let elapsed = now - start_minute;
        let delta = i64::from(end.level) - i64::from(start.level);
        let scaled = delta.abs() * elapsed;
        let magnitude = (2 * scaled + duration) / (2 * duration);
        let value = i64::from(start.level) + delta.signum() * magnitude;
        Some(crate::range::clamp_percent(value))
    }

    /// First anchor point **strictly after** `after`, wrapping
    /// to the next day. With a single point at `after`, returns that
    /// same point (the next day).
    pub fn next_point_after(&self, after: MinuteOfDay) -> Option<MinuteOfDay> {
        let first = self.points.first()?;
        Some(
            self.points
                .iter()
                .find(|p| p.time > after)
                .map_or(first.time, |p| p.time),
        )
    }
}

/// Schedule suspension after a manual action (spec §6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suspension {
    start: MinuteOfDay,
    resume: MinuteOfDay,
}

impl Suspension {
    /// Suspends at `start`; resumes at the next point strictly after.
    /// `None` if the schedule is empty (nothing to suspend).
    pub fn begin(schedule: &Schedule, start: MinuteOfDay) -> Option<Self> {
        Some(Self {
            start,
            resume: schedule.next_point_after(start)?,
        })
    }

    /// Builds a suspension from its two instants (reload
    /// from the state file).
    pub fn from_parts(start: MinuteOfDay, resume: MinuteOfDay) -> Self {
        Self { start, resume }
    }

    /// Instant of the suspension.
    pub fn start(&self) -> MinuteOfDay {
        self.start
    }

    /// Planned resume instant.
    pub fn resume(&self) -> MinuteOfDay {
        self.resume
    }

    /// True if the schedule must resume at `now`: the oriented arc
    /// `start → now` reaches or exceeds the arc `start → resume` on the
    /// 24 h circle. Degenerate case `resume == start`: immediate resume.
    ///
    /// The arc is computed modulo 24 h: the caller evaluates at least once per
    /// minute, so it never skips an entire day.
    pub fn should_resume(&self, now: MinuteOfDay) -> bool {
        let arc = |from: MinuteOfDay, to: MinuteOfDay| {
            (i64::from(to.0) - i64::from(from.0)).rem_euclid(DAY)
        };
        let to_resume = arc(self.start, self.resume);
        to_resume == 0 || arc(self.start, now) >= to_resume
    }
}
