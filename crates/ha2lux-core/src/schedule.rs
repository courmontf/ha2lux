//! Planning horaire : points, interpolation, suspension (spec §6).

use std::fmt;

use thiserror::Error;

/// Nombre maximal de points d'un planning.
pub const MAX_POINTS: usize = 24;

const DAY: i64 = 1440;

/// Erreurs de construction ou de validation d'un planning.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ScheduleError {
    /// Heure mal formée : seul `HH:MM` strict (24 h) est accepté.
    #[error("heure invalide « {0} » : format attendu HH:MM (00:00 à 23:59)")]
    InvalidTime(String),
    /// Minute hors de `0..1440`.
    #[error("minute du jour invalide : {0} (attendu 0 à 1439)")]
    MinuteOutOfRange(u16),
    /// Niveau hors de `0..=100`.
    #[error("niveau invalide : {0} (attendu 0 à 100)")]
    LevelOutOfRange(u8),
    /// Deux points à la même minute.
    #[error("deux points à la même heure : {0}")]
    DuplicateTime(MinuteOfDay),
    /// Trop de points.
    #[error("trop de points : {0} (maximum {MAX_POINTS})")]
    TooManyPoints(usize),
}

/// Minute du jour, `0..=1439`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    /// Construit une minute du jour ; refuse `>= 1440`.
    pub fn new(minute: u16) -> Result<Self, ScheduleError> {
        if i64::from(minute) < DAY {
            Ok(Self(minute))
        } else {
            Err(ScheduleError::MinuteOutOfRange(minute))
        }
    }

    /// Analyse stricte de `HH:MM` : exactement 5 caractères, ni espace ni
    /// signe, `HH ≤ 23`, `MM ≤ 59`.
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

    /// Valeur en minutes depuis minuit.
    pub fn minutes(self) -> u16 {
        self.0
    }
}

impl fmt::Display for MinuteOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.0 / 60, self.0 % 60)
    }
}

/// Point de planning : une heure et un niveau maître.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    /// Heure du point.
    pub time: MinuteOfDay,
    /// Niveau maître 0-100.
    pub level: u8,
}

/// Planning validé : trié, sans doublon d'heure, au plus [`MAX_POINTS`] points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    points: Vec<Point>,
}

impl Schedule {
    /// Valide et trie les points.
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

    /// Points triés par heure.
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// Vrai si le planning n'a aucun point (rien à appliquer).
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Niveau maître à une minute donnée, ou `None` sans point.
    ///
    /// Interpolation linéaire entre points consécutifs, avec bouclage du
    /// dernier au premier point à travers minuit. Arrondi au plus proche ;
    /// sur une égalité exacte, l'écart au point de départ s'éloigne de zéro.
    pub fn value_at(&self, now: MinuteOfDay) -> Option<u8> {
        let first = *self.points.first()?;
        if self.points.len() == 1 {
            return Some(first.level);
        }
        let now = i64::from(now.0);
        // Dernier point dont l'heure est <= now ; sinon, le dernier point de la veille.
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

    /// Premier point d'ancrage **strictement postérieur** à `after`, en
    /// bouclant sur le lendemain. Avec un seul point à `after`, renvoie ce
    /// même point (le lendemain).
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

/// Suspension du planning après une action manuelle (spec §6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suspension {
    start: MinuteOfDay,
    resume: MinuteOfDay,
}

impl Suspension {
    /// Suspend à `start` ; reprise au prochain point strictement postérieur.
    /// `None` si le planning est vide (rien à suspendre).
    pub fn begin(schedule: &Schedule, start: MinuteOfDay) -> Option<Self> {
        Some(Self {
            start,
            resume: schedule.next_point_after(start)?,
        })
    }

    /// Construit une suspension à partir de ses deux instants (rechargement
    /// depuis le fichier d'état).
    pub fn from_parts(start: MinuteOfDay, resume: MinuteOfDay) -> Self {
        Self { start, resume }
    }

    /// Instant de la suspension.
    pub fn start(&self) -> MinuteOfDay {
        self.start
    }

    /// Instant de reprise prévu.
    pub fn resume(&self) -> MinuteOfDay {
        self.resume
    }

    /// Vrai si le planning doit reprendre à `now` : l'arc orienté
    /// `start → now` atteint ou dépasse l'arc `start → resume` sur le cercle de
    /// 24 h. Cas dégénéré `resume == start` : reprise immédiate.
    ///
    /// L'arc est calculé modulo 24 h : l'appelant évalue au moins une fois par
    /// minute, donc ne saute jamais une journée entière.
    pub fn should_resume(&self, now: MinuteOfDay) -> bool {
        let arc = |from: MinuteOfDay, to: MinuteOfDay| {
            (i64::from(to.0) - i64::from(from.0)).rem_euclid(DAY)
        };
        let to_resume = arc(self.start, self.resume);
        to_resume == 0 || arc(self.start, now) >= to_resume
    }
}
