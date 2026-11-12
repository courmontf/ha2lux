#!/usr/bin/env bash
# Installs (or updates) ha2lux from a published release, for the current user and without
# sudo: no Rust toolchain needed. Downloads the archive, checks its SHA-256, installs the
# programs in ~/.local/bin and the user-level service files, then starts the daemon and the icon.
# ha2lux never modifies your system by itself: the two steps that need root (I²C access for
# external monitors) are only printed, for you to run.
#
# Usage: install-release.sh [--version vX.Y.Z] [--archive FILE] [--no-start]
#   --version vX.Y.Z   installs that release instead of the latest
#   --archive FILE     installs from a downloaded .tar.gz (no network)
#   --no-start         installs the files but does not start or restart anything
# Run from an unpacked archive (as `./install.sh`), it installs the files next to it.
# Environment: HA2LUX_REPO=owner/name to use another repository (default: courmontf/ha2lux).
set -euo pipefail

repo="${HA2LUX_REPO:-courmontf/ha2lux}"
version="" archive="" start=1
while [ $# -gt 0 ]; do
    case "$1" in
        --version) shift; version="${1:-}"; [ -n "$version" ] || { echo "--version needs a value (e.g. v0.8.0)" >&2; exit 2; } ;;
        --archive) shift; archive="${1:-}"; [ -f "$archive" ] || { echo "archive not found: ${archive:-}" >&2; exit 2; } ;;
        --no-start) start=0 ;;
        -h | --help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done

bin="$HOME/.local/bin"
config="${XDG_CONFIG_HOME:-$HOME/.config}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"

# Installs the content of an unpacked archive (bin/ and packaging/ side by side).
install_from() {
    local src="$1"
    install -Dm755 "$src/bin/ha2luxd" "$src/bin/ha2lux" "$src/bin/ha2lux-tray" -t "$bin/"
    sed "s|/usr/bin/ha2luxd|$bin/ha2luxd|" "$src/packaging/systemd/ha2luxd.service" \
        | install -Dm644 /dev/stdin "$config/systemd/user/ha2luxd.service"
    install -Dm644 "$src/packaging/dbus/org.ha2lux.Daemon.service" \
        "$data/dbus-1/services/org.ha2lux.Daemon.service"
    sed "s|/usr/bin/ha2lux-tray|$bin/ha2lux-tray|" "$src/packaging/xdg/autostart/ha2lux-tray.desktop" \
        | install -Dm644 /dev/stdin "$config/autostart/ha2lux-tray.desktop"
    # Kept for the root-only steps printed below (the unpacked archive is temporary).
    install -Dm644 "$src/packaging/modules-load.d/ha2lux-i2c-dev.conf" "$data/ha2lux/packaging/ha2lux-i2c-dev.conf"
    install -Dm644 "$src/packaging/udev/60-ha2lux-i2c.rules" "$data/ha2lux/packaging/60-ha2lux-i2c.rules"
}

here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"
if [ -z "$archive" ] && [ -d "$here/bin" ] && [ -d "$here/packaging" ]; then
    src="$here"
else
    arch=$(uname -m)
    [ "$arch" = "x86_64" ] || { echo "no prebuilt binary for $arch: build from the sources (see the README)." >&2; exit 1; }
    for tool in tar sha256sum install; do
        command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 1; }
    done
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    name="ha2lux-$arch-linux.tar.gz"
    if [ -n "$archive" ]; then
        cp "$archive" "$tmp/$name"
    else
        command -v curl >/dev/null || { echo "missing tool: curl" >&2; exit 1; }
        if [ -n "$version" ]; then
            base="https://github.com/$repo/releases/download/$version"
        else
            base="https://github.com/$repo/releases/latest/download"
        fi
        echo "==> downloading $name (${version:-latest})" >&2
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$name" "$base/$name" \
            || { echo "download failed: $base/$name (is the release published and the repository public?)" >&2; exit 1; }
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" \
            || { echo "download failed: $base/SHA256SUMS" >&2; exit 1; }
        echo "==> checking the SHA-256" >&2
        (cd "$tmp" && sha256sum --check --strict SHA256SUMS >/dev/null) \
            || { echo "checksum mismatch: nothing was installed." >&2; exit 1; }
    fi
    tar -xzf "$tmp/$name" -C "$tmp" --no-same-owner
    src=$(find "$tmp" -mindepth 1 -maxdepth 1 -type d -name 'ha2lux-v*' | head -n1)
    [ -n "$src" ] && [ -x "$src/bin/ha2lux" ] || { echo "unexpected archive layout." >&2; exit 1; }
fi

echo "==> installing in $bin" >&2
install_from "$src"
echo "ha2lux $("$bin/ha2lux" --version | awk '{print $NF}') installed." >&2

case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "note: $bin is not in your PATH; add: export PATH=\"\$HOME/.local/bin:\$PATH\"" >&2 ;;
esac

if [ "$start" -eq 1 ]; then
    echo "==> starting the daemon and the icon" >&2
    systemctl --user daemon-reload
    systemctl --user enable ha2luxd
    systemctl --user restart ha2luxd
    pkill -x ha2lux-tray || true
    if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
        setsid -f "$bin/ha2lux-tray" >/dev/null 2>&1
    else
        echo "note: no graphical session detected; the icon will start at your next login." >&2
    fi
fi

if [ ! -e /etc/udev/rules.d/60-ha2lux-i2c.rules ]; then
    cat >&2 <<MSG

External monitors need access to the I²C buses (one-time step, needs root; skip it if you
only use a laptop's internal panel):
  sudo install -m644 $data/ha2lux/packaging/ha2lux-i2c-dev.conf /etc/modules-load.d/ && sudo modprobe i2c-dev
  sudo install -m644 $data/ha2lux/packaging/60-ha2lux-i2c.rules /etc/udev/rules.d/ \\
      && sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev
MSG
fi
echo "Check with: ha2lux doctor && ha2lux get" >&2
