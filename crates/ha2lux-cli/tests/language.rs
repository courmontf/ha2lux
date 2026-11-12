//! Language choice: `HA2LUX_LANG` > the file's `[general] language` > session.
//! We run the real binary with a throwaway `HOME` and an empty environment.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

const EN: &str = "Sets the brightness of all displays";
const FR: &str = "Règle la luminosité de tous les écrans";

/// First line of `ha2lux --help`, with `config` written (or not) to `config.toml`.
fn help(config: Option<&str>, envs: &[(&str, &str)]) -> String {
    let home = tempfile::tempdir().unwrap();
    if let Some(text) = config {
        let dir = home.path().join(".config/ha2lux");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_ha2lux"))
        .arg("--help")
        .env_clear()
        .env("HOME", home.path())
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned()
}

fn config(language: &str) -> String {
    format!("[general]\nlanguage = \"{language}\"\n")
}

#[test]
fn the_config_file_sets_the_language() {
    // The session (English) yields to the file (French), and vice versa.
    assert!(help(Some(&config("fr")), &[("LANG", "en_US.UTF-8")]).starts_with(FR));
    assert!(help(Some(&config("en")), &[("LANG", "fr_FR.UTF-8")]).starts_with(EN));
    // Even without any session variable.
    assert!(help(Some(&config("fr")), &[]).starts_with(FR));
}

#[test]
fn the_environment_override_wins_over_the_file() {
    let envs = [("HA2LUX_LANG", "fr"), ("LANG", "en_US.UTF-8")];
    assert!(help(Some(&config("en")), &envs).starts_with(FR));
}

#[test]
fn auto_or_no_file_follows_the_session() {
    for (language, session, expected) in [
        (Some("auto"), "fr_FR.UTF-8", FR),
        (Some("auto"), "en_US.UTF-8", EN),
        (None, "fr_FR.UTF-8", FR),
        (None, "de_DE.UTF-8", EN),
    ] {
        let text = language.map(config);
        let got = help(text.as_deref(), &[("LANG", session)]);
        assert!(got.starts_with(expected), "{language:?} {session} : {got}");
    }
}

#[test]
fn an_unreadable_or_invalid_config_never_breaks_the_help() {
    let session = [("LANG", "fr_FR.UTF-8")];
    assert!(help(Some("[general\nlanguage = "), &session).starts_with(FR));
    assert!(help(Some(&config("???")), &session).starts_with(FR));
    // A language without a catalog: we fall back to the session.
    assert!(help(Some(&config("de")), &session).starts_with(FR));
}
