//! Changement de langue à chaud (rechargement de la configuration depuis le tray).
//!
//! L'état de langue est global au processus, et `.cargo/config.toml` fixe `HA2LUX_LANG` pour
//! tous les tests : le test se relance donc lui-même avec un environnement vide.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::path::Path;
use std::process::Command;

use ha2lux_i18n::config::apply_file;
use ha2lux_i18n::{current, translate};

const CHILD: &str = "HA2LUX_TEST_HOT_RELOAD_CHILD";

fn quit() -> String {
    translate("tray-quit", None)
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[test]
fn the_language_follows_the_configuration_file_without_restart() {
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "the_language_follows_the_configuration_file_without_restart",
            ])
            .env_clear()
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");

    // Session sans langue : repli anglais.
    assert_eq!(quit(), "Quit");
    let before = current();

    // Le fichier passe en français : la langue change tout de suite.
    write(&file, "[general]\nlanguage = \"fr\"\n");
    assert!(apply_file(&file));
    assert_eq!(quit(), "Quitter");
    // Un localisateur déjà obtenu garde sa langue (pas de texte à moitié traduit).
    assert_eq!(before.lang(), "en");

    // Réappliquer la même langue ne signale aucun changement.
    assert!(!apply_file(&file));

    // Fichier mal formé ou absent : la langue actuelle est conservée.
    write(&file, "[general\nlanguage = ");
    assert!(!apply_file(&file));
    assert!(!apply_file(&dir.path().join("absent.toml")));
    assert_eq!(quit(), "Quitter");

    // `auto` (ou plus de clé) redonne la langue de la session.
    write(&file, "[general]\nlanguage = \"auto\"\n");
    assert!(apply_file(&file));
    assert_eq!(quit(), "Quit");
}
