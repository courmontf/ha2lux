#!/usr/bin/env bash
# Rebuilds ha2lux from the sources and reinstalls it for the current user (no sudo):
# release build, binaries copied to ~/.local/bin, daemon restarted, tray icon relaunched.
# Stops at the first error, so a failed build never restarts anything.
#
# Usage: scripts/update.sh [--pull]
#   --pull   runs `git pull --ff-only` first (fetch the latest sources)
#
# Only for the "quick install" of the README (systemd --user unit already installed);
# for a first installation, follow the README.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

case "${1:-}" in
    "") ;;
    --pull) git pull --ff-only ;;
    -h | --help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "option inconnue : $1 (voir --help)" >&2; exit 2 ;;
esac

bin="$HOME/.local/bin"

echo "==> compilation (release)" >&2
cargo build --release

echo "==> installation dans $bin" >&2
install -Dm755 target/release/ha2luxd target/release/ha2lux target/release/ha2lux-tray -t "$bin/"

echo "==> redémarrage du démon" >&2
systemctl --user restart ha2luxd

echo "==> relance de l'icône" >&2
pkill -x ha2lux-tray || true
setsid -f "$bin/ha2lux-tray" >/dev/null 2>&1

echo "ha2lux $("$bin/ha2lux" --version | awk '{print $NF}') installé." >&2
"$bin/ha2lux" doctor >/dev/null 2>&1 || echo "attention : « ha2lux doctor » signale un problème (lancez-le pour le détail)." >&2
