# ha2lux

*English · [Français](README.fr.md)*

Sets the brightness of **all your displays** on Linux/Wayland — external monitors (DDC/CI over
`/dev/i2c-*`) and a laptop's internal panel (backlight) — with a **master level**, **per-display
ranges**, **presets**, a **time-of-day schedule**, **idle dimming** and **auto-brightness** driven
by a [Home Assistant](https://www.home-assistant.io/) illuminance sensor (it learns your curve).

Three programs:

| Binary | Role |
|---|---|
| `ha2luxd` | user daemon (session D-Bus `org.ha2lux.Daemon`): the only one that touches the hardware |
| `ha2lux` | command line (bind it to your keyboard shortcuts) |
| `ha2lux-tray` | notification-area icon (StatusNotifierItem): menu, mouse wheel, on-screen display |

> **Status**: the whole software is tested without hardware (fake displays, fake Home Assistant,
> fake Wayland compositor, private D-Bus buses). Validation **on your own displays** is still to
> be done: see `specs/VALIDATION_MATERIEL.md` (in French).

## Building

```sh
cargo build --release          # binaries in target/release/
./scripts/ci.sh                # fmt + clippy + tests
```

Rust 1.85 or newer (see `rust-toolchain.toml`).

## Installation

### Quick install (from source)

You need a Rust toolchain 1.85 or newer ([rustup](https://rustup.rs)) and a C linker
(`build-essential` on Debian/Ubuntu, `base-devel` on Arch, `gcc` on Fedora). Then, from the
repository root, copy and paste:

```sh
# 1. Build
cargo build --release

# 2. Install the programs for your user (no sudo)
mkdir -p ~/.local/bin
install -Dm755 target/release/ha2luxd target/release/ha2lux target/release/ha2lux-tray -t ~/.local/bin/
sed "s|/usr/bin/ha2luxd|$HOME/.local/bin/ha2luxd|" packaging/systemd/ha2luxd.service \
    | install -Dm644 /dev/stdin ~/.config/systemd/user/ha2luxd.service
install -Dm644 packaging/dbus/org.ha2lux.Daemon.service ~/.local/share/dbus-1/services/org.ha2lux.Daemon.service
sed "s|/usr/bin/ha2lux-tray|$HOME/.local/bin/ha2lux-tray|" packaging/xdg/autostart/ha2lux-tray.desktop \
    | install -Dm644 /dev/stdin ~/.config/autostart/ha2lux-tray.desktop

# 3. Allow access to the displays' I²C buses (needs sudo; external monitors only)
sudo install -m644 packaging/modules-load.d/ha2lux-i2c-dev.conf /etc/modules-load.d/ && sudo modprobe i2c-dev
sudo install -m644 packaging/udev/60-ha2lux-i2c.rules /etc/udev/rules.d/ \
    && sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev

# 4. Start the daemon and the icon
systemctl --user daemon-reload && systemctl --user enable --now ha2luxd
setsid -f ~/.local/bin/ha2lux-tray >/dev/null 2>&1

# 5. Check that everything works
ha2lux doctor && ha2lux get
```

Notes:

- `~/.local/bin` must be in your `PATH` (it is by default on most distributions; otherwise add
  `export PATH="$HOME/.local/bin:$PATH"` to your shell profile).
- Step 3 is only needed for **external monitors**; a laptop's internal panel works without it.
  If `ha2lux doctor` still reports "access denied" after step 3, log out and back in.
- The icon starts by itself at your next login. On GNOME, install the "AppIndicator" extension
  first, or no icon will appear.
- Nothing outside the files listed above is touched: ha2lux never edits your system on its own.

### Update

```sh
git pull && cargo build --release \
&& install -Dm755 target/release/ha2luxd target/release/ha2lux target/release/ha2lux-tray -t ~/.local/bin/ \
&& systemctl --user restart ha2luxd \
&& pkill -x ha2lux-tray; setsid -f ~/.local/bin/ha2lux-tray >/dev/null 2>&1
```

Always restart the daemon after updating: the daemon and the other programs must be the same
version. The icon's **About** submenu shows both versions and warns if they differ.

### Uninstall

```sh
systemctl --user disable --now ha2luxd; pkill -x ha2lux-tray
rm -f ~/.local/bin/{ha2luxd,ha2lux,ha2lux-tray} ~/.config/systemd/user/ha2luxd.service \
      ~/.local/share/dbus-1/services/org.ha2lux.Daemon.service ~/.config/autostart/ha2lux-tray.desktop
sudo rm -f /etc/udev/rules.d/60-ha2lux-i2c.rules /etc/modules-load.d/ha2lux-i2c-dev.conf
```

Your settings stay in `~/.config/ha2lux/` and `~/.local/state/ha2lux/` (delete them if you want).

### Manual installation (details)

1. **Binaries**: copy `ha2luxd`, `ha2lux`, `ha2lux-tray` to `~/.local/bin/` (or `/usr/bin/`).
2. **I²C bus permissions** (external displays only):
   - kernel module: `sudo install -m644 packaging/modules-load.d/ha2lux-i2c-dev.conf /etc/modules-load.d/ && sudo modprobe i2c-dev`
   - access: `sudo install -m644 packaging/udev/60-ha2lux-i2c.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev`
     (alternative: an `i2c` group, described in the rules file).
   - check: `ha2lux doctor` (then `ha2lux doctor --probe` to test a real read).
3. **Daemon**: see the header of `packaging/systemd/ha2luxd.service`, then
   `systemctl --user enable --now ha2luxd`. The file `packaging/dbus/org.ha2lux.Daemon.service`
   also allows on-demand startup.
4. **Icon** (optional): run `ha2lux-tray`, or copy `packaging/xdg/autostart/ha2lux-tray.desktop`
   to `~/.config/autostart/` (adjust its `Exec=` path). On GNOME, install the "AppIndicator" extension.

Package skeletons (Arch, RPM, Debian) are in `packaging/` — **untested**.

## Usage

```sh
ha2lux set 40            # master level 40%
ha2lux up / ha2lux down  # one step (general.step setting, 5 by default)
ha2lux up 10
ha2lux get               # state and level of each display
ha2lux list              # known displays
ha2lux set 30 --display DELL     # a single display (number, name or substring)
ha2lux preset Night
ha2lux schedule on|off   ha2lux idle-dim on|off   ha2lux auto on|off
ha2lux rescan            ha2lux reload            ha2lux doctor
```

`--json` gives structured output. Exit codes: 0 success · 1 usage · 2 daemon absent ·
3 command refused · 4 daemon not responding.

### Keyboard shortcuts (Wayland)

On Wayland an application cannot capture global keys: bind them to the CLI in your desktop.

- **KDE Plasma**: System Settings → Shortcuts → Add Command: `ha2lux up` / `ha2lux down`.
- **GNOME**: Settings → Keyboard → Custom Shortcuts: `ha2lux up` / `ha2lux down`.
- **Hyprland** (`hyprland.conf`): `binde = , XF86MonBrightnessUp, exec, ha2lux up`
  and `binde = , XF86MonBrightnessDown, exec, ha2lux down` (`binde` repeats while the key is held).
- **sway** (`config`): `bindsym --locked XF86MonBrightnessUp exec ha2lux up` (same for `down`).

### Notification-area icon

Menu: current level, "All displays" and a submenu per display (levels in 10% steps), presets,
auto-brightness / schedule / idle dimming, rescan, settings (opens `config.toml`), About (tray and daemon versions), quit.
**Mouse wheel** on the icon: ± one step (fast notches are merged) with a progress notification.
There is no slider popup (documented deviation: `specs/DECISIONS.md` no. 84).

## Configuration

`~/.config/ha2lux/config.toml` (created on first run, tolerant: an invalid key is reported in the
log and ignored; an unreadable file is never rewritten). Example:

```toml
version = 1

[general]
step = 5
edid_bus_matching = false      # true: EDID fallback for NVIDIA cards
language = "auto"              # "auto" (follow the session), "en" or "fr"

[presets]
Night = 15
Day = 80

[schedule]
enabled = true
points = [{ time = "07:30", level = 70 }, { time = "22:00", level = 15 }]

[idle]
enabled = true
level = 5            # level while idle
minutes = 5

[auto]
enabled = false
url = "http://homeassistant.local:8123"
entity = "sensor.living_room_illuminance"
```

After editing: `ha2lux reload`. Learned levels and state are in
`~/.local/state/ha2lux/state.toml`.

### Auto-brightness (Home Assistant)

1. Create a **long-lived access token** in your Home Assistant profile.
2. Set `[auto] url` and `entity` (`ha2lux auto sensors` lists the illuminance sensors once the
   token is stored), then `ha2lux reload`.
3. Store the token **through standard input** (never as a command-line argument):
   `ha2lux auto token < token.txt`. It is kept in the desktop keyring (Secret Service), or
   otherwise in `~/.config/ha2lux/secrets.json` (permissions 0600).
4. `ha2lux auto on`, then `ha2lux auto status` to follow the state.

The token is bound to the host:port: changing the address means entering it again. Your manual
adjustments (shortcut, wheel, preset…) are **learned**: the lux → level curve adapts (undoable:
`ha2lux auto reset-curve`). If Home Assistant is unreachable, the schedule takes over.

## Releases and versioning

Versions follow [SemVer](https://semver.org) and commits follow
[Conventional Commits](https://www.conventionalcommits.org) (`type(scope): description`):

| Commit | Bump |
|---|---|
| `feat: …` | minor (0.5.0 → 0.6.0) |
| `fix: …`, `perf: …` | patch (0.5.0 → 0.5.1) |
| `type!: …` or a `BREAKING CHANGE:` line in the body | major (minor while the version is 0.x) |
| `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `style`, `i18n` | none on their own |

`scripts/release.sh` reads the commits since the last version, picks the next number, updates
`Cargo.toml`, `Cargo.lock`, the RPM and Arch packaging and `CHANGELOG.md`, then commits
(`chore(release): vX.Y.Z`) and tags it. It never pushes:

```sh
scripts/release.sh --dry-run     # show the next version and the changelog, change nothing
scripts/release.sh               # create the release (asks for confirmation)
git push --follow-tags           # publish it
```

`--bump major|minor|patch` forces the level (for example to reach 1.0.0). A `commit-msg` hook
(enabled by `scripts/install-hooks.sh`) warns when a message does not follow the convention.

## Languages

The interface (CLI, icon, daemon messages, `ha2lux doctor`) is available in **English** and
**French**. To choose, set `language` in `config.toml` (`[general]` section):

```toml
[general]
language = "en"      # "auto" (default: follow the session), "en" or "fr"
```

Priority: the `HA2LUX_LANG=en|fr` environment variable (handy for a one-off test), then the
`language` setting, then your session's language (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`).
Other languages fall back to English. The icon reads the setting when it starts: restart it
(`pkill -x ha2lux-tray; ha2lux-tray &`) after changing it. To add a language, see
`crates/ha2lux-i18n/src/lib.rs`. Logs are always in English.

## Troubleshooting

- `ha2lux doctor`: state of modules, `/dev/i2c-*` permissions, backlight, detected displays.
- Daemon log: `journalctl --user -u ha2luxd -f` (`RUST_LOG=debug` for more detail).
- A display that answers DDC/CI poorly may need a few seconds after wake-up: the daemon retries
  with growing delays, then waits (cooldown) without ever blocking the other displays.
- The daemon replaces an already running instance; it refuses to start if the old one does not
  release the D-Bus name within 5 s (never two daemons on a DDC bus).

## Security and limits

See `specs/MENACES.md` (in French). In short: no hardware write apart from the brightness DDC/CI
commands (VCP 0x10) and the backlight through logind; no `unsafe` beyond documented need; the
token appears neither in logs, nor in the configuration, nor as an argument; strict TLS (no
option to ignore a certificate); HTTP redirects are never followed. Known limits: full-screen is
not detected for idle dimming; no slider popup.

## Architecture

`crates/`: `ha2lux-core` (pure logic, no I/O), `ha2lux-backends` (DDC/CI, backlight, discovery),
`ha2lux-ha` (Home Assistant client), `ha2lux-secrets` (keyring), `ha2lux-ipc` (D-Bus interface),
`ha2lux-i18n` (translations), `ha2luxd`, `ha2lux-cli`, `ha2lux-tray`. Details:
`specs/ARCHITECTURE.md` (in French). Decisions and deviations: `specs/DECISIONS.md`. Fuzzing:
`fuzz/README.md`.

## License

[MIT](LICENSE) — Copyright (c) 2026 courmontf.
