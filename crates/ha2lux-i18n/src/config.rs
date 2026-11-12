//! Langue choisie dans `config.toml` (`[general] language`).
//!
//! La CLI et le tray ne parlent pas toujours au démon (aide, `doctor`, erreurs
//! d'usage) : ils lisent donc eux-mêmes ce fichier, au démarrage. Toute difficulté
//! (fichier absent, illisible, valeur invalide) est ignorée : on retombe alors sur
//! la langue de la session, sans jamais empêcher d'afficher une aide.

use std::path::{Path, PathBuf};

use ha2lux_core::config::{Config, LANGUAGE_AUTO};

/// Chemin de `config.toml` (même règle XDG que le démon : une variable XDG n'est
/// valable que si elle est un chemin absolu).
pub fn config_file(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let absolute = |p: &PathBuf| p.is_absolute();
    get("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(absolute)
        .or_else(|| {
            get("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| Path::new(&home).join(".config"))
                .filter(absolute)
        })
        .map(|base| base.join("ha2lux").join("config.toml"))
}

/// Langue demandée par le texte d'une configuration, `None` pour `auto`,
/// l'absence de clé ou une configuration illisible.
pub fn language_in(text: &str) -> Option<String> {
    let language = Config::parse(text).ok()?.config.general.language;
    (language != LANGUAGE_AUTO).then_some(language)
}

/// Lit `config.toml` et fixe la langue demandée (voir [`crate::set_preferred`]).
/// À appeler au début de `main`, avant d'afficher quoi que ce soit.
pub fn init_from_config() {
    let Some(path) = config_file(|name| std::env::var(name).ok()) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    if let Some(language) = language_in(&text) {
        crate::set_preferred(&language);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn config_file_follows_xdg_then_home() {
        let both = [("XDG_CONFIG_HOME", "/x/cfg"), ("HOME", "/home/u")];
        assert_eq!(
            config_file(env(&both)),
            Some(PathBuf::from("/x/cfg/ha2lux/config.toml"))
        );
        let home = [("HOME", "/home/u")];
        assert_eq!(
            config_file(env(&home)),
            Some(PathBuf::from("/home/u/.config/ha2lux/config.toml"))
        );
        // Une variable XDG relative est ignorée.
        let relative = [("XDG_CONFIG_HOME", "cfg"), ("HOME", "/home/u")];
        assert_eq!(
            config_file(env(&relative)),
            Some(PathBuf::from("/home/u/.config/ha2lux/config.toml"))
        );
        assert_eq!(config_file(env(&[])), None);
        assert_eq!(config_file(env(&[("HOME", "relatif")])), None);
    }

    #[test]
    fn language_in_reads_the_general_section() {
        assert_eq!(
            language_in("[general]\nlanguage = \"en\""),
            Some("en".into())
        );
        assert_eq!(language_in("[general]\nlanguage = \"auto\""), None);
        assert_eq!(language_in(""), None);
        // Valeur invalide : la configuration la ramène à `auto`.
        assert_eq!(language_in("[general]\nlanguage = \"???\""), None);
        // Fichier illisible : aucune langue, jamais d'erreur.
        assert_eq!(language_in("[general\nlanguage = "), None);
    }
}
