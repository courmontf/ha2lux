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

## Installation (manual — ha2lux never modifies your system)

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
   to `~/.config/autostart/`. On GNOME, install the "AppIndicator" extension.

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
auto-brightness / schedule / idle dimming, rescan, settings (opens `config.toml`), quit.
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

## Languages

The interface (CLI, icon, daemon messages, `ha2lux doctor`) is available in **English** and
**French**. The language follows your environment (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`);
`HA2LUX_LANG=en` or `HA2LUX_LANG=fr` forces it. Other languages fall back to English. To add a
language, see `crates/ha2lux-i18n/src/lib.rs`. Logs are always in English.

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
