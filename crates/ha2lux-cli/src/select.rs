//! Sélection d'un écran par numéro, nom exact ou sous-chaîne (spec §11.1).

use std::fmt;

use ha2lux_i18n::t;
use ha2lux_ipc::DisplayDto;

/// Échec de sélection ; porte de quoi expliquer à l'utilisateur.
#[derive(Debug, PartialEq, Eq)]
pub enum SelectError {
    /// Numéro hors de la liste (ou nul).
    OutOfRange {
        /// Numéro demandé.
        given: String,
        /// Nombre d'écrans.
        count: usize,
    },
    /// Aucun écran ne correspond.
    NotFound(String),
    /// Plusieurs écrans correspondent ; numéros (à partir de 1).
    Ambiguous {
        /// Texte saisi.
        spec: String,
        /// Numéros des candidats.
        numbers: Vec<usize>,
    },
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { given, count } => f.write_str(&t!(
                "cli-select-out-of-range",
                given = given.as_str(),
                count = count.to_string()
            )),
            Self::NotFound(spec) => f.write_str(&t!("cli-select-not-found", spec = spec.as_str())),
            Self::Ambiguous { spec, numbers } => f.write_str(&t!(
                "cli-select-ambiguous",
                spec = spec.as_str(),
                candidates = candidates(numbers)
            )),
        }
    }
}

impl std::error::Error for SelectError {}

fn candidates(list: &[usize]) -> String {
    list.iter()
        .map(|n| t!("cli-select-candidate", number = n.to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn lower(text: &str) -> String {
    text.to_lowercase()
}

/// Choisit un écran : un nombre est une position dans `list` (à partir de 1) ;
/// sinon nom exact unique, puis sous-chaîne unique, insensibles à la casse.
/// Renvoie la position (à partir de 1) et l'écran.
pub fn select<'a>(
    list: &'a [DisplayDto],
    spec: &str,
) -> Result<(usize, &'a DisplayDto), SelectError> {
    let spec = spec.trim();
    if !spec.is_empty() && spec.bytes().all(|b| b.is_ascii_digit()) {
        let position = spec
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=list.len()).contains(n));
        return position
            .map(|n| (n, &list[n - 1]))
            .ok_or_else(|| SelectError::OutOfRange {
                given: spec.to_owned(),
                count: list.len(),
            });
    }
    let wanted = lower(spec);
    let pick = |matches: &dyn Fn(&DisplayDto) -> bool| -> Result<Option<(usize, &'a DisplayDto)>, SelectError> {
        let found: Vec<(usize, &DisplayDto)> =
            list.iter().enumerate().filter(|(_, d)| matches(d)).map(|(i, d)| (i + 1, d)).collect();
        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            many => Err(SelectError::Ambiguous { spec: spec.to_owned(), numbers: many.iter().map(|(n, _)| *n).collect() }),
        }
    };
    if let Some(found) = pick(&|d| lower(&d.name) == wanted || lower(&d.id) == wanted)? {
        return Ok(found);
    }
    pick(&|d| lower(&d.name).contains(&wanted) || lower(&d.id).contains(&wanted))?
        .ok_or_else(|| SelectError::NotFound(spec.to_owned()))
}
