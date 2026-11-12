# ha2lux-tray — notification-area icon: menu, tooltip, on-screen brightness display.

tray-percent = { $value }%

## Menu
tray-daemon-unreachable = The ha2luxd daemon is not responding
tray-quit = Quit
tray-brightness = Brightness: { $level }
tray-brightness-unknown = Brightness: unknown
tray-all-displays = All displays
tray-display-with-level = { $name } ({ $level })
tray-display-unavailable = { $name } (unavailable)
tray-preset = { $name } — { $level }
tray-auto-configured = Auto-brightness (Home Assistant sensor)
tray-auto-unconfigured = Auto-brightness (not configured)
tray-sensor-ok = sensor: up to date
tray-sensor-stale = sensor: stale reading
tray-sensor-degraded = sensor: degraded
tray-sensor-offline = sensor: offline
tray-sensor-unknown = sensor: unknown
tray-schedule-fallback = Schedule (used when Home Assistant is offline)
tray-schedule = Time-of-day schedule
tray-schedule-resume = Schedule paused until { $resume }
tray-idle-dim = Dim when idle ({ $level }% after { $minutes } min)
tray-rescan = Rescan displays
tray-settings = Settings…
tray-about = About
tray-about-tray = Tray icon ha2lux-tray { $version }
tray-about-daemon = Daemon ha2luxd { $version }
tray-about-daemon-unreachable = Daemon ha2luxd: unreachable
tray-about-daemon-unknown = Daemon ha2luxd: unknown version
tray-about-mismatch = Versions differ: restart the daemon

## Tooltip
tray-tooltip-unreachable = daemon unreachable
tray-tooltip = { $level } · { $mode }
tray-tooltip-dimmed = { $level } · { $mode } · dimmed

## On-screen display (notification)
tray-osd = Brightness { $level }%

## Command line and errors
tray-usage =
    usage: ha2lux-tray [--help | --version]

    Notification-area icon (StatusNotifierItem) for ha2lux: preset menu,
    mouse wheel to adjust brightness, schedule / auto / idle-dimming toggles.
    The ha2luxd daemon must be running; the icon queries it over D-Bus.

    exit codes: 0 normal exit, 1 usage, 3 no notification area or bus
tray-unexpected-argument = unexpected argument: { $arg }
tray-error = error: { $error }
tray-err-bus = session bus unavailable: { $error }
tray-err-no-watcher = no notification area (StatusNotifierWatcher): { $error }. On GNOME, install the "AppIndicator" extension.
tray-detail-no-host = no icon host
