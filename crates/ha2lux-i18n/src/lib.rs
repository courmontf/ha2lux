//! Internationalisation de ha2lux.
//!
//! Les textes visibles par l'utilisateur vivent dans des catalogues
//! [Fluent](https://projectfluent.org) (`locales/<langue>/*.ftl`) embarqués dans
//! le binaire. La langue est choisie une fois par processus, d'après
//! l'environnement (voir [`detect`]) ; une clé absente d'une langue retombe sur
//! la langue de repli [`FALLBACK_LANG`].
//!
//! ```
//! let texte = ha2lux_i18n::t!("i18n-self-test", name = "monde");
//! assert!(texte.contains("monde"));
//! ```
//!
//! Les journaux (`tracing`) et les identifiants D-Bus ne sont **pas** traduits.
//!
//! # Où vivent les textes
//!
//! - `locales/<langue>/cli.ftl` : CLI `ha2lux` (sortie, erreurs, aide) ;
//! - `locales/<langue>/tray.ftl` : icône de zone de notification ;
//! - `locales/<langue>/daemon.ftl` : messages du démon, envoyés aux clients sous
//!   forme de clé + arguments (voir [`wire`]) et traduits par le client ;
//! - `locales/<langue>/doctor.ftl` : rapport `ha2lux doctor`.
//!
//! # Ajouter une langue (ex. `en`)
//!
//! 1. Créer `locales/en/` avec les mêmes fichiers `.ftl` que `locales/fr/`
//!    (mêmes clés, mêmes `$variables`) ;
//! 2. les déclarer dans `CATALOGS` (`src/lib.rs`) ;
//! 3. `cargo test -p ha2lux-i18n` vérifie que chaque fichier est embarqué et que
//!    la nouvelle langue définit exactement les clés de `fr` ;
//! 4. essayer : `HA2LUX_LANG=en ha2lux --help`.
//!
//! # Écrire du code qui affiche du texte
//!
//! Jamais de phrase en dur : `t!("cli-list-empty")`, ou avec des variables
//! `t!("cli-display-level", name = nom, number = n, level = niveau)`. Pas de
//! retour à la ligne final dans les messages (le code l'ajoute), et les nombres
//! dont le format compte (`{:.0}`, `{:04X}`) se passent déjà formatés. Un test
//! vérifie que toute clé citée dans le code existe dans le catalogue.
#![forbid(unsafe_code)]

pub mod wire;

use std::sync::OnceLock;

pub use fluent_bundle::FluentArgs;
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentResource, FluentValue};

use unic_langid::LanguageIdentifier;

/// Langue utilisée quand l'environnement n'en désigne aucune de disponible, et
/// pour les clés qu'une traduction n'a pas encore.
pub const FALLBACK_LANG: &str = "en";

/// Variable d'environnement qui impose la langue (prioritaire sur `LANG`).
pub const LANG_ENV: &str = "HA2LUX_LANG";

/// Une langue disponible et ses fichiers `.ftl`.
struct Catalog {
    lang: &'static str,
    files: &'static [(&'static str, &'static str)],
}

/// Catalogues embarqués. Toute nouvelle langue ou tout nouveau fichier s'ajoute
/// ici ; le test `catalogs_cover_locales_dir` détecte un oubli.
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

/// Langues disponibles, dans l'ordre de déclaration.
pub fn available() -> Vec<&'static str> {
    CATALOGS.iter().map(|c| c.lang).collect()
}

/// Ramène une valeur d'environnement (`fr_FR.UTF-8@euro`, `en-GB`, `C`…) au code
/// de langue seul (`fr`, `en`), ou `None` pour `C`/`POSIX`/vide.
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

/// Choisit la langue d'après l'environnement fourni par `var`.
///
/// Ordre : `HA2LUX_LANG`, `LANGUAGE` (liste séparée par `:`), `LC_ALL`,
/// `LC_MESSAGES`, `LANG`. Le premier code reconnu parmi [`available`] gagne ;
/// sinon [`FALLBACK_LANG`].
pub fn detect(var: impl Fn(&str) -> Option<String>) -> &'static str {
    let candidates = [LANG_ENV, "LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"];
    for name in candidates {
        let Some(raw) = var(name) else { continue };
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

/// Les messages d'une langue, avec repli sur [`FALLBACK_LANG`].
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
    // Pas de marques d'isolation Unicode : elles polluent la sortie du terminal.
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
    /// Charge `lang` (repli : [`FALLBACK_LANG`]). Erreur si un catalogue est
    /// mal formé ou contient un identifiant en double — vérifié par les tests.
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

    /// Langue effectivement utilisée.
    pub fn lang(&self) -> &'static str {
        self.lang
    }

    /// Texte de `key`, ou `key` lui-même s'il n'existe dans aucune langue.
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

static CURRENT: OnceLock<Localizer> = OnceLock::new();

/// Le localisateur du processus, initialisé au premier appel d'après l'environnement.
pub fn current() -> &'static Localizer {
    CURRENT.get_or_init(|| {
        let lang = detect(|name| std::env::var(name).ok());
        match Localizer::try_new(lang) {
            Ok(localizer) => localizer,
            // Catalogue embarqué invalide : impossible en pratique (testé).
            // On reste utilisable : les clés s'affichent telles quelles.
            Err(_) => Localizer {
                lang: FALLBACK_LANG,
                primary: FluentBundle::new_concurrent(Vec::new()),
                fallback: None,
            },
        }
    })
}

/// Traduit `key` dans la langue du processus (voir aussi la macro [`t!`]).
pub fn translate(key: &str, args: Option<&FluentArgs<'_>>) -> String {
    current().translate(key, args)
}

/// Convertit une valeur en argument Fluent (réexport pour la macro).
pub fn arg<'a>(value: impl Into<FluentValue<'a>>) -> FluentValue<'a> {
    value.into()
}

/// `t!("clé")` ou `t!("clé", nom = valeur, …)` : texte localisé.
///
/// Les noms d'arguments correspondent aux variables `{ $nom }` du catalogue.
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
        // Une valeur inconnue est ignorée au profit de la suivante.
        assert_eq!(detect(env(&[("LANGUAGE", "zz:fr"), ("LANG", "zz")])), "fr");
        assert_eq!(detect(env(&[(LANG_ENV, "fr"), ("LC_ALL", "zz")])), "fr");
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

    /// Chaque `.ftl` du dossier `locales/` doit être embarqué, et inversement.
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

    /// Une langue partielle : ses clés l'emportent, les autres retombent sur la
    /// langue de repli, et les pluriels suivent les règles de la langue.
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
        // En français, 0 et 1 sont au singulier.
        for (n, expected) in [(0, "0 écran"), (1, "1 écran"), (3, "3 écrans")] {
            let mut args = FluentArgs::new();
            args.set("n", arg(n));
            assert_eq!(fr.translate("displays", Some(&args)), expected);
        }
        // Langue inconnue : retombe sur la langue de repli.
        let unknown = Localizer::with_catalogs(CATALOGS_TEST, "de").expect("repli");
        assert_eq!(unknown.lang(), FALLBACK_LANG);
        assert_eq!(unknown.translate("both", None), "english");
    }

    /// Chaque clé citée dans le code (`t!("…")`, `encode("…")`) existe dans le
    /// catalogue de repli : une faute de frappe ne passerait sinon pas inaperçue.
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
        // Garde-fou contre un balayage qui ne verrait plus rien (motif cassé).
        assert!(
            checked >= 150,
            "le balayage ne voit que {checked} clés : motif cassé ?"
        );
    }

    /// Toute langue doit définir exactement les mêmes clés que la langue de repli
    /// (sinon des textes resteraient non traduits sans qu'on le voie).
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
