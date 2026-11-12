#!/usr/bin/env bash
# Builds the prebuilt-release archive from `target/release` (run `cargo build --release --locked`
# first): dist/ha2lux-<arch>-linux.tar.gz and dist/SHA256SUMS.
#
# The archive holds the three programs, the files of `packaging/` that the installer uses,
# the licence, the READMEs, the changelog and `install.sh` (the same script as
# `scripts/install-release.sh`, which installs from the files next to it).
# The archive name carries no version, so that
# `releases/latest/download/ha2lux-<arch>-linux.tar.gz` always points to the latest release;
# the version is the name of the top-level directory inside the archive.
#
# Usage: scripts/package.sh
set -euo pipefail
cd "$(dirname "$0")/.."

[ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ] && { sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

arch=$(uname -m)
[ "$arch" = "x86_64" ] || { echo "error: only x86_64 is packaged (this machine: $arch)" >&2; exit 1; }
version=$(awk -F'"' '/^version *=/ { print $2; exit }' Cargo.toml)
[ -n "$version" ] || { echo "error: version not found in Cargo.toml" >&2; exit 1; }
for b in ha2luxd ha2lux ha2lux-tray; do
    [ -x "target/release/$b" ] || { echo "error: target/release/$b missing (cargo build --release --locked)" >&2; exit 1; }
done

name="ha2lux-$arch-linux"
stage="dist/stage/ha2lux-v$version-$arch-linux"
rm -rf dist
mkdir -p "$stage/bin" dist

install -m755 target/release/ha2luxd target/release/ha2lux target/release/ha2lux-tray "$stage/bin/"
for f in systemd/ha2luxd.service dbus/org.ha2lux.Daemon.service xdg/autostart/ha2lux-tray.desktop \
         modules-load.d/ha2lux-i2c-dev.conf udev/60-ha2lux-i2c.rules; do
    install -Dm644 "packaging/$f" "$stage/packaging/$f"
done
install -m644 LICENSE README.md README.fr.md CHANGELOG.md "$stage/"
install -m755 scripts/install-release.sh "$stage/install.sh"

# Reproducible archive: sorted entries, fixed owner and dates (those of the last commit).
tar -C dist/stage --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="@$(git log -1 --format=%ct)" -cf - "ha2lux-v$version-$arch-linux" \
    | gzip -n -9 > "dist/$name.tar.gz"
rm -rf dist/stage
(cd dist && sha256sum "$name.tar.gz" > SHA256SUMS)
echo "dist/$name.tar.gz (v$version)" >&2
cat dist/SHA256SUMS >&2
