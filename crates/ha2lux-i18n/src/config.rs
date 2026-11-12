//! Language chosen in `config.toml` (`[general] language`).
//!
//! The CLI and the tray do not always talk to the daemon (help, `doctor`, usage errors),
//! so they read this file themselves at startup. Any problem
//! (missing or unreadable file, invalid value) is ignored: we then fall back to
//! the session language, never preventing help from being displayed.

use std::path::{Path, PathBuf};

use ha2lux_core::config::{Config, LANGUAGE_AUTO};

/// Path of `config.toml` (same XDG rule as the daemon: an XDG variable is
/// only valid if it is an absolute path).
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

/// Language requested by a configuration text, `None` for `auto`,
/// a missing key or an unreadable configuration.
pub fn language_in(text: &str) -> Option<String> {
    let language = Config::parse(text).ok()?.config.general.language;
    (language != LANGUAGE_AUTO).then_some(language)
}

/// Reads `config.toml` and sets the requested language (see [`crate::set_preferred`]).
/// To be called at the start of `main`, before displaying anything.
pub fn init_from_config() {
    if let Some(path) = config_file(|name| std::env::var(name).ok()) {
        apply_file(&path);
    }
}

/// Rereads `path` and applies its language. A missing, unreadable or malformed file changes nothing;
/// `auto` or a missing key restore the session language. Returns `true` if the
/// effective language changed.
pub fn apply_file(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    // Malformed file: keep the current language rather than losing it.
    let Ok(parsed) = Config::parse(&text) else {
        return false;
    };
    crate::set_preferred(&parsed.config.general.language)
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
        // A relative XDG variable is ignored.
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
        // Invalid value: the configuration maps it to `auto`.
        assert_eq!(language_in("[general]\nlanguage = \"???\""), None);
        // Unreadable file: no language, never an error.
        assert_eq!(language_in("[general\nlanguage = "), None);
    }
}
