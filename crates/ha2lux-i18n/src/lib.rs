//! Internationalization of ha2lux.
//!
//! User-visible texts live in [Fluent](https://projectfluent.org)
//! catalogs (`locales/<language>/*.ftl`) embedded in
//! the binary. The language is chosen once per process, based on
//! the environment (see [`detect`]); a key missing from a language falls back to
//! the fallback language [`FALLBACK_LANG`].
//!
//! ```
//! let texte = ha2lux_i18n::t!("i18n-self-test", name = "monde");
//! assert!(texte.contains("monde"));
//! ```
//!
//! Logs (`tracing`) and D-Bus identifiers are **not** translated.
//!
//! # Where the texts live
//!
//! - `locales/<language>/cli.ftl`: `ha2lux` CLI (output, errors, help);
//! - `locales/<language>/tray.ftl`: notification area icon;
//! - `locales/<language>/daemon.ftl`: daemon messages, sent to clients as
//!   key + arguments (see [`wire`]) and translated by the client;
//! - `locales/<language>/doctor.ftl`: `ha2lux doctor` report.
//!
//! # Adding a language (e.g. `en`)
//!
//! 1. Create `locales/en/` with the same `.ftl` files as `locales/fr/`
//!    (same keys, same `$variables`);
//! 2. declare them in `CATALOGS` (`src/lib.rs`);
//! 3. `cargo test -p ha2lux-i18n` checks that every file is embedded and that
//!    the new language defines exactly the keys of `fr`;
//! 4. try it: `HA2LUX_LANG=en ha2lux --help`.
//!
//! # Writing code that displays text
//!
//! Never hard-code a sentence: `t!("cli-list-empty")`, or with variables
//! `t!("cli-display-level", name = nom, number = n, level = niveau)`. No
//! trailing newline in messages (the code adds it), and numbers
//! whose format matters (`{:.0}`, `{:04X}`) are passed already formatted. A test
//! checks that every key cited in the code exists in the catalog.
#![forbid(unsafe_code)]

pub mod config;
pub mod wire;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub use fluent_bundle::FluentArgs;
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentResource, FluentValue};

use unic_langid::LanguageIdentifier;

/// Language used when the environment designates none that is available, and
/// for keys a translation does not have yet.
pub const FALLBACK_LANG: &str = "en";

/// Environment variable that forces the language (takes priority over `LANG`).
pub const LANG_ENV: &str = "HA2LUX_LANG";

/// An available language and its `.ftl` files.
struct Catalog {
    lang: &'static str,
    files: &'static [(&'static str, &'static str)],
}

/// Embedded catalogs. Any new language or file is added
/// here; the `catalogs_cover_locales_dir` test detects an omission.
const CATALOGS: &[Catalog] = &[
    Catalog {
        lang: "en",
        files: &[
            ("common.ftl", include_str!("../locales/en/common.ftl")),
            ("cli.ftl", include_str!("../locales/en/cli.ftl")),
            ("tray.ftl", include_str!("../locales/en/tray.ftl")),
            ("daemon.ftl", include_str!("../locales/en/daemon.ftl")),
            ("doctor.ftl", include_str!("../locales/en/doctor.ftl")),
        ],
    },
    Catalog {
        lang: "fr",
        files: &[
            ("common.ftl", include_str!("../locales/fr/common.ftl")),
            ("cli.ftl", include_str!("../locales/fr/cli.ftl")),
            ("tray.ftl", include_str!("../locales/fr/tray.ftl")),
            ("daemon.ftl", include_str!("../locales/fr/daemon.ftl")),
            ("doctor.ftl", include_str!("../locales/fr/doctor.ftl")),
        ],
    },
];

/// Available languages, in declaration order.
pub fn available() -> Vec<&'static str> {
    CATALOGS.iter().map(|c| c.lang).collect()
}

/// Reduces an environment value (`fr_FR.UTF-8@euro`, `en-GB`, `C`…) to the bare
/// language code (`fr`, `en`), or `None` for `C`/`POSIX`/empty.
fn language_code(value: &str) -> Option<String> {
    let base = value
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .split(['_', '-'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "" | "c" | "posix" => None,
        _ => Some(base),
    }
}

/// Picks the language from the environment provided by `var`.
///
/// Order: `HA2LUX_LANG`, `LANGUAGE` (`:`-separated list), `LC_ALL`,
/// `LC_MESSAGES`, `LANG`. The first code recognized among [`available`] wins;
/// otherwise [`FALLBACK_LANG`].
pub fn detect(var: impl Fn(&str) -> Option<String>) -> &'static str {
    detect_with(None, var)
}

/// Like [`detect`], with the configuration file's language (`preferred`):
/// it comes after `HA2LUX_LANG` but before the session variables. `auto`
/// (or `None`) is the same as having none. An unknown language is ignored.
pub fn detect_with(preferred: Option<&str>, var: impl Fn(&str) -> Option<String>) -> &'static str {
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(var(LANG_ENV));
    candidates.extend(
        preferred
            .filter(|lang| !lang.eq_ignore_ascii_case("auto"))
            .map(str::to_owned),
    );
    for name in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        candidates.extend(var(name));
    }
    for raw in candidates {
        for part in raw.split(':') {
            if let Some(code) = language_code(part)
                && let Some(found) = CATALOGS.iter().find(|c| c.lang == code)
            {
                return found.lang;
            }
        }
    }
    FALLBACK_LANG
}

/// Process language: the configuration file's (`preferred`) and the
/// resulting localizer, built on first use.
struct Language {
    preferred: Option<String>,
    current: Option<Arc<Localizer>>,
}

static LANGUAGE: Mutex<Language> = Mutex::new(Language {
    preferred: None,
    current: None,
});

fn language() -> MutexGuard<'static, Language> {
    // A poisoned lock does not make the data invalid: carry on.
    LANGUAGE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sets the configuration file's language (`auto`: the session's).
/// May be called again at any time (configuration reload): the
/// next texts are then translated into the new language. Returns `true` if
/// the effective language changed.
pub fn set_preferred(lang: &str) -> bool {
    let before = current().lang();
    let mut state = language();
    state.preferred = Some(lang.to_owned());
    state.current = None;
    drop(state);
    current().lang() != before
}

/// The messages of a language, with fallback to [`FALLBACK_LANG`].
pub struct Localizer {
    lang: &'static str,
    primary: FluentBundle<FluentResource>,
    fallback: Option<FluentBundle<FluentResource>>,
}

fn build_bundle(catalog: &Catalog) -> Result<FluentBundle<FluentResource>, String> {
    let id: LanguageIdentifier = catalog
        .lang
        .parse()
        .map_err(|e| format!("langue « {} » invalide : {e}", catalog.lang))?;
    let mut bundle = FluentBundle::new_concurrent(vec![id]);
    // No Unicode isolation marks: they pollute the terminal output.
    bundle.set_use_isolating(false);
    for (name, source) in catalog.files {
        let resource = FluentResource::try_new((*source).to_owned())
            .map_err(|(_, errors)| format!("{}/{name} : {errors:?}", catalog.lang))?;
        bundle
            .add_resource(resource)
            .map_err(|errors| format!("{}/{name} : {errors:?}", catalog.lang))?;
    }
    Ok(bundle)
}

impl Localizer {
    /// Loads `lang` (fallback: [`FALLBACK_LANG`]). Error if a catalog is
    /// malformed or contains a duplicate identifier — checked by the tests.
    pub fn try_new(lang: &str) -> Result<Self, String> {
        Self::with_catalogs(CATALOGS, lang)
    }

    fn with_catalogs(catalogs: &[Catalog], lang: &str) -> Result<Self, String> {
        let primary_catalog = catalogs
            .iter()
            .find(|c| c.lang == lang)
            .or_else(|| catalogs.iter().find(|c| c.lang == FALLBACK_LANG))
            .ok_or_else(|| "aucun catalogue embarqué".to_owned())?;
        let fallback = if primary_catalog.lang == FALLBACK_LANG {
            None
        } else {
            catalogs
                .iter()
                .find(|c| c.lang == FALLBACK_LANG)
                .map(build_bundle)
                .transpose()?
        };
        Ok(Self {
            lang: primary_catalog.lang,
            primary: build_bundle(primary_catalog)?,
            fallback,
        })
    }

    /// Language actually used.
    pub fn lang(&self) -> &'static str {
        self.lang
    }

    /// Text of `key`, or `key` itself if it exists in no language.
    pub fn translate(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        for bundle in std::iter::once(&self.primary).chain(self.fallback.as_ref()) {
            let Some(message) = bundle.get_message(key) else {
                continue;
            };
            let Some(pattern) = message.value() else {
                continue;
            };
            let mut errors = Vec::new();
            return bundle
                .format_pattern(pattern, args, &mut errors)
                .into_owned();
        }
        key.to_owned()
    }
}

/// The process's localizer, built on first call from the configuration
/// then the environment. The result stays valid even if the language changes later.
pub fn current() -> Arc<Localizer> {
    let mut state = language();
    if let Some(localizer) = &state.current {
        return Arc::clone(localizer);
    }
    let lang = detect_with(state.preferred.as_deref(), |name| std::env::var(name).ok());
    let localizer = Arc::new(match Localizer::try_new(lang) {
        Ok(localizer) => localizer,
        // Invalid embedded catalog: impossible in practice (tested).
        // We stay usable: keys are displayed as-is.
        Err(_) => Localizer {
            lang: FALLBACK_LANG,
            primary: FluentBundle::new_concurrent(Vec::new()),
            fallback: None,
        },
    });
    state.current = Some(Arc::clone(&localizer));
    localizer
}

/// Translates `key` into the process language (see also the [`t!`] macro).
pub fn translate(key: &str, args: Option<&FluentArgs<'_>>) -> String {
    current().translate(key, args)
}

/// Converts a value into a Fluent argument (re-export for the macro).
pub fn arg<'a>(value: impl Into<FluentValue<'a>>) -> FluentValue<'a> {
    value.into()
}

/// `t!("key")` or `t!("key", name = value, …)`: localized text.
///
/// Argument names match the catalog's `{ $name }` variables.
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        $crate::translate($key, ::core::option::Option::None)
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {{
        let mut args = $crate::FluentArgs::new();
        $( args.set(stringify!($name), $crate::arg($value)); )+
        $crate::translate($key, ::core::option::Option::Some(&args))
    }};
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

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
    fn language_code_normalizes_posix_locales() {
        assert_eq!(language_code("fr_FR.UTF-8"), Some("fr".to_owned()));
        assert_eq!(language_code("en-GB"), Some("en".to_owned()));
        assert_eq!(language_code("de_DE@euro"), Some("de".to_owned()));
        assert_eq!(language_code("C"), None);
        assert_eq!(language_code("POSIX"), None);
        assert_eq!(language_code(""), None);
    }

    #[test]
    fn detect_prefers_override_then_language_then_lc_then_lang() {
        assert_eq!(detect(env(&[])), FALLBACK_LANG);
        assert_eq!(detect(env(&[("LANG", "C.UTF-8")])), FALLBACK_LANG);
        assert_eq!(detect(env(&[("LANG", "zz_ZZ.UTF-8")])), FALLBACK_LANG);
        assert_eq!(detect(env(&[("LANG", "fr_FR.UTF-8")])), "fr");
        // An unknown value is ignored in favor of the next one.
        assert_eq!(detect(env(&[("LANGUAGE", "zz:fr"), ("LANG", "zz")])), "fr");
        assert_eq!(detect(env(&[(LANG_ENV, "fr"), ("LC_ALL", "zz")])), "fr");
    }

    #[test]
    fn detect_with_puts_the_config_between_the_override_and_the_session() {
        // The configuration file wins over the session...
        let session = [("LANG", "en_US.UTF-8")];
        assert_eq!(detect_with(Some("fr"), env(&session)), "fr");
        // ... but not over HA2LUX_LANG.
        let forced = [(LANG_ENV, "en"), ("LANG", "fr_FR.UTF-8")];
        assert_eq!(detect_with(Some("fr"), env(&forced)), "en");
        // `auto`, a missing value and an unknown language let the session decide.
        let fr_session = [("LANG", "fr_FR.UTF-8")];
        for ignored in [Some("auto"), Some("AUTO"), None, Some("zz")] {
            assert_eq!(detect_with(ignored, env(&fr_session)), "fr", "{ignored:?}");
        }
    }

    #[test]
    fn catalogs_load_without_errors() {
        for lang in available() {
            Localizer::try_new(lang).unwrap_or_else(|e| panic!("{lang} : {e}"));
        }
    }

    #[test]
    fn translate_substitutes_variables_without_isolation_marks() {
        let fr = Localizer::try_new("fr").expect("catalogue fr");
        let mut args = FluentArgs::new();
        args.set("name", arg("monde"));
        let text = fr.translate("i18n-self-test", Some(&args));
        assert_eq!(text, "bonjour monde");
    }

    #[test]
    fn missing_key_returns_the_key() {
        let fr = Localizer::try_new("fr").expect("catalogue fr");
        assert_eq!(fr.translate("cle-inexistante", None), "cle-inexistante");
    }

    /// Every `.ftl` in the `locales/` folder must be embedded, and vice versa.
    #[test]
    fn catalogs_cover_locales_dir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut on_disk = BTreeSet::new();
        for lang in std::fs::read_dir(&root).expect("dossier locales") {
            let lang = lang.expect("entrée").path();
            let lang_name = lang
                .file_name()
                .expect("nom")
                .to_string_lossy()
                .into_owned();
            for file in std::fs::read_dir(&lang).expect("dossier de langue") {
                let name = file
                    .expect("entrée")
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                on_disk.insert(format!("{lang_name}/{name}"));
            }
        }
        let embedded: BTreeSet<String> = CATALOGS
            .iter()
            .flat_map(|c| c.files.iter().map(move |(n, _)| format!("{}/{n}", c.lang)))
            .collect();
        assert_eq!(
            on_disk, embedded,
            "fichiers .ftl non embarqués (ou en trop)"
        );
    }

    /// A partial language: its keys win, the others fall back to the
    /// fallback language, and plurals follow the language's rules.
    #[test]
    fn a_partial_language_falls_back_key_by_key_and_handles_plurals() {
        const CATALOGS_TEST: &[Catalog] = &[
            Catalog {
                lang: "en",
                files: &[("t.ftl", "only-en = english only\nboth = english\n")],
            },
            Catalog {
                lang: "fr",
                files: &[(
                    "t.ftl",
                    "both = français\ndisplays = { $n ->\n    [one] { $n } écran\n   *[other] { $n } écrans\n }\n",
                )],
            },
        ];
        let fr = Localizer::with_catalogs(CATALOGS_TEST, "fr").expect("catalogue fr");
        assert_eq!(fr.lang(), "fr");
        assert_eq!(fr.translate("both", None), "français");
        assert_eq!(fr.translate("only-en", None), "english only");
        assert_eq!(fr.translate("absent", None), "absent");
        // In French, 0 and 1 are singular.
        for (n, expected) in [(0, "0 écran"), (1, "1 écran"), (3, "3 écrans")] {
            let mut args = FluentArgs::new();
            args.set("n", arg(n));
            assert_eq!(fr.translate("displays", Some(&args)), expected);
        }
        // Unknown language: falls back to the fallback language.
        let unknown = Localizer::with_catalogs(CATALOGS_TEST, "de").expect("repli");
        assert_eq!(unknown.lang(), FALLBACK_LANG);
        assert_eq!(unknown.translate("both", None), "english");
    }

    /// Every key cited in the code (`t!("…")`, `encode("…")`) exists in the
    /// fallback catalog: otherwise a typo would not go unnoticed.
    #[test]
    fn every_key_used_in_code_exists() {
        fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("dossier") {
                let path = entry.expect("entrée").path();
                if path.is_dir() {
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let reference = CATALOGS
            .iter()
            .find(|c| c.lang == FALLBACK_LANG)
            .expect("langue de repli");
        let mut known = BTreeSet::new();
        for (_, source) in reference.files {
            for line in source.lines() {
                if line.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                    && let Some((id, _)) = line.split_once('=')
                {
                    known.insert(id.trim().to_owned());
                }
            }
        }
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for krate in std::fs::read_dir(&crates).expect("crates") {
            let src = krate.expect("entrée").path().join("src");
            if src.is_dir() {
                rust_files(&src, &mut files);
            }
        }
        let mut unknown = Vec::new();
        let mut checked = 0_usize;
        for file in files {
            let text = std::fs::read_to_string(&file).expect("lecture");
            for marker in ["t!(", "encode("] {
                let mut rest = text.as_str();
                while let Some(at) = rest.find(marker) {
                    rest = &rest[at + marker.len()..];
                    let after = rest.trim_start();
                    if let Some(body) = after.strip_prefix('"')
                        && let Some(end) = body.find('"')
                    {
                        let key = &body[..end];
                        let looks_like_key = key.contains('-')
                            && key.chars().all(|c| c.is_ascii_lowercase() || c == '-');
                        checked += usize::from(looks_like_key);
                        if looks_like_key && !known.contains(key) {
                            unknown.push(format!("{} : {key}", file.display()));
                        }
                    }
                }
            }
        }
        assert!(
            unknown.is_empty(),
            "clés absentes du catalogue : {unknown:#?}"
        );
        // Safeguard against a scan that would no longer see anything (broken pattern).
        assert!(
            checked >= 150,
            "le balayage ne voit que {checked} clés : motif cassé ?"
        );
    }

    /// Every language must define exactly the same keys as the fallback language
    /// (otherwise some texts would remain untranslated unnoticed).
    #[test]
    fn every_language_has_the_fallback_keys() {
        fn keys(catalog: &Catalog) -> BTreeSet<String> {
            let mut set = BTreeSet::new();
            for (_, source) in catalog.files {
                for line in source.lines() {
                    let starts_ident = line.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
                    if starts_ident && let Some((id, _)) = line.split_once('=') {
                        set.insert(id.trim().to_owned());
                    }
                }
            }
            set
        }
        let reference = CATALOGS
            .iter()
            .find(|c| c.lang == FALLBACK_LANG)
            .expect("langue de repli");
        let expected = keys(reference);
        for catalog in CATALOGS {
            assert_eq!(
                keys(catalog),
                expected,
                "clés de {} ≠ {FALLBACK_LANG}",
                catalog.lang
            );
        }
    }
}
