//! Fichiers : écriture atomique, chargement tolérant, chemins XDG (T-CFG-01/08/09).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io;
use std::sync::Arc;

use ha2lux_core::config::{Config, ConfigError};
use ha2lux_core::{Curve, LearnPolicy, State};
use ha2luxd::paths::Paths;
use ha2luxd::store::{
    ConfigLoad, StoreError, backup, cleanup_stale_temps, load_or_create_config, load_state,
    read_config, write_atomic, write_atomic_with,
};
use tempfile::TempDir;

fn names(dir: &TempDir) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

#[test]
fn write_atomic_creates_parents_and_replaces_without_leftovers() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("a/b/state.toml");
    write_atomic(&file, b"one").unwrap();
    write_atomic(&file, b"two").unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"two");
    assert_eq!(names(&TempDir::new().unwrap()), Vec::<String>::new());
    let siblings: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(siblings.len(), 1, "aucun fichier temporaire ne subsiste");
}

#[test]
fn t_cfg_09_interrupted_write_keeps_the_old_file_intact() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    write_atomic(&file, b"ancien contenu").unwrap();
    let result = write_atomic_with(&file, b"nouveau contenu", || {
        Err(io::Error::other("coupure simulée"))
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(&file).unwrap(),
        b"ancien contenu",
        "l'original n'a pas bougé"
    );
    assert_eq!(
        names(&dir),
        ["config.toml"],
        "le fichier temporaire est nettoyé"
    );
}

#[test]
fn t_cfg_09_hard_kill_leaves_only_a_stale_temp_that_is_ignored_then_cleaned() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    write_atomic(&file, Config::default().to_toml().unwrap().as_bytes()).unwrap();
    // `kill -9` après l'écriture du temporaire et avant le renommage : il reste un résidu tronqué.
    fs::write(dir.path().join(".config.toml.tmp.4242.0"), b"tronq").unwrap();
    assert_eq!(
        read_config(&file).unwrap().config,
        Config::default(),
        "le résidu n'est jamais lu"
    );
    assert!(matches!(
        load_or_create_config(&file).unwrap(),
        ConfigLoad::Loaded(_)
    ));
    assert_eq!(
        names(&dir),
        ["config.toml"],
        "résidu supprimé au chargement"
    );
    assert_eq!(cleanup_stale_temps(&file), 0);
}

#[test]
fn concurrent_writers_never_expose_a_partial_file() {
    let dir = TempDir::new().unwrap();
    let file = Arc::new(dir.path().join("state.toml"));
    let payloads: Vec<Vec<u8>> = (0..4u8).map(|i| vec![b'a' + i; 64 * 1024]).collect();
    write_atomic(&file, &payloads[0]).unwrap();
    let writers: Vec<_> = payloads
        .iter()
        .cloned()
        .map(|payload| {
            let file = Arc::clone(&file);
            std::thread::spawn(move || {
                for _ in 0..20 {
                    write_atomic(&file, &payload).unwrap();
                }
            })
        })
        .collect();
    let reader = {
        let (file, payloads) = (Arc::clone(&file), payloads.clone());
        std::thread::spawn(move || {
            for _ in 0..200 {
                let content = fs::read(&*file).unwrap();
                assert!(
                    payloads.contains(&content),
                    "lecture d'un fichier partiel ({} octets)",
                    content.len()
                );
            }
        })
    };
    for writer in writers {
        writer.join().unwrap();
    }
    reader.join().unwrap();
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "aucun temporaire restant"
    );
}

#[test]
fn t_cfg_01_missing_config_is_created_with_defaults() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("ha2lux/config.toml");
    let ConfigLoad::Created(loaded) = load_or_create_config(&file).unwrap() else {
        panic!("la configuration aurait dû être créée");
    };
    assert_eq!(loaded.config, Config::default());
    assert_eq!(
        read_config(&file).unwrap().config,
        Config::default(),
        "le fichier créé est relisible"
    );
}

#[test]
fn invalid_syntax_gives_defaults_and_never_touches_the_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "[general\nstep = ").unwrap();
    let before = fs::read(&file).unwrap();
    assert!(matches!(
        load_or_create_config(&file).unwrap(),
        ConfigLoad::Invalid {
            error: ConfigError::Syntax { .. }
        }
    ));
    assert_eq!(fs::read(&file).unwrap(), before);
}

#[test]
fn t_cfg_08_newer_schema_is_refused_and_the_file_is_intact() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "version = 99\n[general]\nstep = 7\n").unwrap();
    let before = fs::read(&file).unwrap();
    let error = load_or_create_config(&file).unwrap_err();
    assert!(matches!(
        error,
        StoreError::Config {
            error: ConfigError::UnsupportedVersion { found: 99, .. },
            ..
        }
    ));
    assert!(error.to_string().contains("99"));
    assert_eq!(fs::read(&file).unwrap(), before);
}

#[test]
fn read_config_reports_missing_file_and_bad_content() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        read_config(&dir.path().join("absent.toml")),
        Err(StoreError::Io { .. })
    ));
    let file = dir.path().join("c.toml");
    fs::write(&file, "x = [").unwrap();
    assert!(matches!(read_config(&file), Err(StoreError::Config { .. })));
}

#[test]
fn state_round_trip_and_missing_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("state.toml");
    assert_eq!(load_state(&file).state, State::default());
    let mut curve = Curve::new();
    curve.learn(5.0, 30, None, &LearnPolicy::UNGUARDED);
    let state = State {
        master: Some(33),
        curve,
        ..State::default()
    };
    write_atomic(&file, state.to_toml().unwrap().as_bytes()).unwrap();
    assert_eq!(load_state(&file).state, state);
}

#[test]
fn corrupt_state_is_set_aside_not_silently_overwritten() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("state.toml");
    fs::write(&file, "master = [[[").unwrap();
    let loaded = load_state(&file);
    assert!(loaded.set_aside);
    assert_eq!(loaded.state, State::default());
    assert_eq!(
        fs::read_to_string(dir.path().join("state.toml.corrupt")).unwrap(),
        "master = [[["
    );
    assert!(!file.exists());
    // Une version plus récente est traitée de la même façon.
    fs::write(&file, "version = 9\nmaster = 5\n").unwrap();
    assert!(load_state(&file).set_aside);
}

#[test]
fn backup_copies_the_original_once_it_exists() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    backup(&file).unwrap(); // absent : rien à faire
    assert!(!dir.path().join("config.toml.bak").exists());
    fs::write(&file, "# mon commentaire\n").unwrap();
    backup(&file).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(),
        "# mon commentaire\n"
    );
}

#[test]
fn xdg_paths() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    let paths = Paths::from_env(env(&[
        ("XDG_CONFIG_HOME", "/cfg"),
        ("XDG_STATE_HOME", "/st"),
    ]))
    .unwrap();
    assert_eq!(paths.config_file.to_str(), Some("/cfg/ha2lux/config.toml"));
    assert_eq!(paths.state_file.to_str(), Some("/st/ha2lux/state.toml"));
    let paths = Paths::from_env(env(&[("HOME", "/home/u")])).unwrap();
    assert_eq!(
        paths.config_file.to_str(),
        Some("/home/u/.config/ha2lux/config.toml")
    );
    assert_eq!(
        paths.state_file.to_str(),
        Some("/home/u/.local/state/ha2lux/state.toml")
    );
    // Un chemin XDG relatif est ignoré, comme l'exige la convention.
    let paths =
        Paths::from_env(env(&[("XDG_CONFIG_HOME", "relatif"), ("HOME", "/home/u")])).unwrap();
    assert_eq!(
        paths.config_file.to_str(),
        Some("/home/u/.config/ha2lux/config.toml")
    );
    assert!(Paths::from_env(env(&[])).is_err());
}
