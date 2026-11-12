#!/usr/bin/env bash
# CI locale : formatage, clippy, tests, et cargo-deny s'il est installé.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PWD/target/tools/bin:$PATH"   # outils locaux : cargo install --root target/tools cargo-deny cargo-audit
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
if cargo deny --version >/dev/null 2>&1; then
    cargo deny check
else
    echo "cargo-deny absent : vérification des licences/avis ignorée (cargo install cargo-deny)" >&2
fi
