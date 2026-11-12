# ha2lux (CLI) — user-visible text. Messages have no trailing newline: the code adds it.

cli-percent = { $value }%
cli-unknown = unknown

## Display kinds and states
cli-kind-internal = internal
cli-kind-external = external
cli-state-controllable = controllable
cli-state-unavailable = unavailable
cli-state-not-controllable = not controllable
cli-state-absent = absent

## list / get
cli-display-line = { $number }  { $name }  [{ $id }]  { $kind }  { $state }  level { $level }  range { $low }-{ $high }
cli-list-empty = no display detected
cli-get-master = master: { $level }  (source: { $mode })
cli-get-schedule-resume = schedule paused until { $resume }
cli-get-dimmed = idle dimming in effect
cli-display-level = { $name } (#{ $number }): { $level }

## auto status
cli-auto-enabled = enabled
cli-auto-disabled = disabled
cli-auto-configured = configured
cli-auto-needs-url-entity = not configured (URL and entity required)
cli-auto-needs-url-entity-token = not configured (URL, entity and token required: ha2lux auto token)
cli-auto-summary = auto-brightness: { $enabled }, { $configured }
cli-auto-state-line = state: { $state }
cli-auto-token-rejected = token rejected by Home Assistant: store a new one (ha2lux auto token)
cli-auto-raw = raw sensor value: { $raw }
cli-auto-lux = smoothed illuminance: { $lux } lx
cli-auto-lux-with-latest = smoothed illuminance: { $lux } lx (latest reading: { $latest } lx)
cli-auto-target = level targeted by the curve: { $level }
cli-auto-applied = applied level: { $level }
cli-auto-curve = curve ({ $kind }): { $points }
cli-auto-curve-default = default
cli-auto-curve-learned = learned
cli-auto-curve-point = { $lux } lx → { $level }
cli-auto-source = control source: { $mode }
cli-auto-state-inactive = inactive
cli-auto-state-not-configured = not configured
cli-auto-state-token-rejected = token rejected
cli-auto-state-offline = offline
cli-auto-state-degraded = degraded
cli-auto-state-waiting = waiting for a reading
cli-auto-state-adjusting = adjusting
cli-auto-state-stale = no recent reading
cli-auto-state-active = active
cli-sensors-none = no illuminance sensor (device_class illuminance) found

## Toggle confirmations
cli-toggle-schedule-on = schedule enabled
cli-toggle-schedule-off = schedule disabled
cli-toggle-idle-on = idle dimming enabled: level { $level }%, after { $minutes } min of inactivity
cli-toggle-idle-off = idle dimming disabled
cli-toggle-auto-on = auto-brightness enabled
cli-toggle-auto-off = auto-brightness disabled

## Command results
cli-token-cleared = Home Assistant token removed
cli-token-saved = Home Assistant token saved
cli-curve-reset = learned curve forgotten
cli-warn-schedule-empty = warning: the schedule is enabled but has no points (nothing will be applied)

## Errors (prefixed with "error:" when displayed)
cli-error = error: { $message }
cli-err-json = could not serialize JSON: { $error }
cli-err-no-reply = no reply within { $seconds } s
cli-err-unexpected-reply = unexpected reply from the daemon
cli-err-unexpected-reply-short = unexpected reply
cli-err-daemon-absent = the ha2luxd daemon is not running ({ $detail }); start it: systemctl --user start ha2luxd
cli-err-daemon-no-reply = the ha2luxd daemon is not responding ({ $detail })
cli-detail-no-owner = no owner for org.ha2lux.Daemon
cli-detail-timeout = timed out
cli-err-display-unavailable = display unavailable: { $name }
cli-err-level-unknown = current level of { $name } is unknown
cli-err-token-read = could not read the token: { $error }
cli-err-token-empty = no token received: send it on standard input (e.g. ha2lux auto token < file)
cli-displays-available = available displays:
cli-see-list = (see `ha2lux list`)

## Display selection
cli-select-out-of-range = display #{ $given } not found ({ $count } display(s))
cli-select-not-found = no display matches "{ $spec }"
cli-select-ambiguous = "{ $spec }" matches several displays: { $candidates }
cli-select-candidate = #{ $number }

## Argument parsing
cli-usage = Usage:
cli-heading-commands = Commands
cli-heading-global = Global options
cli-value-command = COMMAND
cli-usage-hint = Try `ha2lux --help`, or `ha2lux <command> --help` for details on a command.
cli-err-not-integer-between = "{ $text }" is not an integer between { $low } and { $high }
cli-err-empty-value = the value must not be empty
cli-usage-unknown-command = unknown command: { $value }
cli-usage-unknown-argument = unexpected option or argument: { $arg }
cli-usage-missing-argument = missing required argument: { $arg }
cli-usage-missing-command = missing command
cli-usage-reason = { "" }: { $reason }
cli-usage-possible-values = { " " }(possible values: { $values })
cli-usage-invalid-value = invalid value "{ $value }" for { $arg }{ $reason }
cli-usage-conflict = conflicting arguments: { $arg }
cli-usage-too-many = too many values for { $arg } (only one expected)
cli-usage-missing-value = missing value for { $arg }

## Help: commands (cli-cmd-<path>), arguments (cli-arg-<path>-<id>), value names (cli-val-<path>-<id>)
cli-cmd-root = Sets the brightness of all displays by talking to the `ha2luxd` daemon
cli-cmd-set = Sets the level (master, or a single display with --display)
cli-cmd-up = Raises the level by one step
cli-cmd-down = Lowers the level by one step
cli-cmd-get = Shows the master level and each display's level
cli-cmd-list = Lists the displays, numbered
cli-cmd-preset = Applies a preset (case-insensitive)
cli-cmd-schedule = Enables or disables the time-of-day schedule
cli-cmd-idle-dim = Enables or disables dimming when idle
cli-cmd-auto = Auto-brightness (Home Assistant sensor)
cli-cmd-auto-on = Enables auto-brightness
cli-cmd-auto-off = Disables auto-brightness
cli-cmd-auto-status = Shows the auto-brightness state
cli-cmd-auto-reset-curve = Forgets the learned curve
cli-cmd-auto-token = Stores the Home Assistant token (read from standard input, never as an argument)
cli-cmd-auto-sensors = Lists Home Assistant's illuminance sensors
cli-cmd-rescan = Re-enumerates the displays
cli-cmd-reload = Reloads the configuration
cli-cmd-doctor = Diagnoses the DDC/CI and backlight chain (no daemon needed)

cli-arg-root-help = Shows help
cli-arg-root-version = Shows the version
cli-arg-root-json = JSON output (for scripts); errors are also returned as JSON
cli-arg-set-level = Level from 0 to 100
cli-arg-set-display = Target display: number from `ha2lux list`, exact name or substring of the name (case-insensitive). Without this option, the command acts on the master level
cli-arg-up-step = Step from 1 to 100 (default: the daemon's `general.step` setting, 5)
cli-arg-up-display = Target display: number from `ha2lux list`, exact name or substring of the name (case-insensitive). Without this option, the command acts on the master level
cli-arg-down-step = Step from 1 to 100 (default: the daemon's `general.step` setting, 5)
cli-arg-down-display = Target display: number from `ha2lux list`, exact name or substring of the name (case-insensitive). Without this option, the command acts on the master level
cli-arg-get-display = Target display: number from `ha2lux list`, exact name or substring of the name (case-insensitive). Without this option, the command acts on the master level
cli-arg-preset-name = Preset name
cli-arg-schedule-state = `on` or `off`
cli-arg-idle-dim-state = `on` or `off`
cli-arg-auto-token-clear = Removes the stored token instead of entering one
cli-arg-doctor-probe = Sends REAL DDC/CI frames (brightness read) to external displays

cli-val-set-level = LEVEL
cli-val-set-display = DISPLAY
cli-val-up-step = STEP
cli-val-up-display = DISPLAY
cli-val-down-step = STEP
cli-val-down-display = DISPLAY
cli-val-get-display = DISPLAY
cli-val-schedule-state = STATE
cli-val-idle-dim-state = STATE
