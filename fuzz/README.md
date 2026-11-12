# Fuzzing

Cibles `cargo-fuzz` pour les analyseurs qui lisent des données non fiables : URL saisie,
EDID d'un écran, `config.toml`, trames DDC, réponses Home Assistant.

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cargo +nightly fuzz run url        # ou edid, config, ddc, ha_state
```

Elles ne sont **pas** exécutées par `scripts/ci.sh` (elles exigent la chaîne nightly).
Les mêmes analyseurs sont couverts en continu par des tests `proptest` (`tests/robust.rs`).
