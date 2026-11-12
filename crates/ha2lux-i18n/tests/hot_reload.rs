//! Hot language switching (configuration reload from the tray).
//!
//! Language state is process-global, and `.cargo/config.toml` sets `HA2LUX_LANG` for
//! all tests: the test therefore relaunches itself with an empty environment.
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

    // Session with no language: English fallback.
    assert_eq!(quit(), "Quit");
    let before = current();

    // The file switches to French: the language changes immediately.
    write(&file, "[general]\nlanguage = \"fr\"\n");
    assert!(apply_file(&file));
    assert_eq!(quit(), "Quitter");
    // A localizer already obtained keeps its language (no half-translated text).
    assert_eq!(before.lang(), "en");

    // Reapplying the same language reports no change.
    assert!(!apply_file(&file));

    // Malformed or missing file: the current language is kept.
    write(&file, "[general\nlanguage = ");
    assert!(!apply_file(&file));
    assert!(!apply_file(&dir.path().join("absent.toml")));
    assert_eq!(quit(), "Quitter");

    // `auto` (or no key anymore) restores the session language.
    write(&file, "[general]\nlanguage = \"auto\"\n");
    assert!(apply_file(&file));
    assert_eq!(quit(), "Quit");
}
