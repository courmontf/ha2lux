# ha2luxd — messages sent to clients (CLI, tray) in D-Bus errors, and the daemon's
# command line. These keys travel encoded (see ha2lux_i18n::wire): the client
# translates them into its own language.

## Request validation
# $what: master-level, display-level, step or preset-level.
err-out-of-range = { $what ->
    [master-level] master level
    [display-level] display level
    [step] step
    [preset-level] preset
   *[other] value
 } out of range: { $value }
err-percent-out-of-range = { $what ->
    [master-level] master level
    [display-level] display level
    [step] step
    [preset-level] preset
   *[other] value
 } out of range: { $value } (expected 0 to 100)
err-display-id-invalid = invalid display identifier (1 to { $max } characters, no control characters)
err-preset-name-invalid = invalid preset name (1 to { $max } characters, no control characters)
err-token-empty = empty token
err-step-out-of-range = step out of range: { $delta } (expected 1 to 100, positive or negative)
err-range-out-of-range = range out of range: ({ $low }, { $high }) (expected 0 to 100)
err-unknown-preset = unknown preset: { $name }; available: { $available }
err-unknown-feature = unknown feature: { $feature } (expected: { $first }, { $second } or { $third })

## Displays
err-unknown-display = unknown display: { $id }
err-display-unavailable = display unavailable: { $id }
err-no-controllable-display = no controllable display

## Auto-brightness and Home Assistant token
err-auto-needs-config = auto-brightness requires the Home Assistant URL, the sensor entity and a token
err-no-ha-url = no valid Home Assistant URL in the configuration ([auto] url)
err-no-token = no token stored for this address (ha2lux auto token)

## Daemon state
err-daemon-stopped = the daemon is stopped

## Keyring (token)
err-keyring-unavailable = keyring unavailable: { $detail }
err-keyring-locked = keyring locked: unlock refused or timed out
err-secret-file-too-open = { $path }: permissions too open (0600 expected), file ignored

## Home Assistant
err-ha-config-url = invalid Home Assistant address: { $detail }
err-ha-config-entity = invalid entity identifier (expected: domain.object)
err-ha-config-no-token = access token missing
err-ha-auth = authentication refused: token invalid or revoked
err-ha-not-found = entity not found
err-ha-redirect = redirect refused: the token is never sent to another address
err-ha-too-large = response too large (over 256 KiB)
err-ha-tls = TLS error: { $detail }
err-ha-timeout = timed out
err-ha-connection = connection failed: { $detail }
err-ha-protocol = unexpected response: { $detail }

## Daemon command line
daemon-usage =
    usage: ha2luxd [--help | --version]

    ha2lux user daemon: sets the brightness of displays (DDC/CI and backlight).
    It replaces an already running instance, then serves the D-Bus interface org.ha2lux.Daemon1
    on the session bus. Log level: RUST_LOG variable (default: info).

    exit codes: 0 normal exit, 1 usage, 3 startup failure
daemon-unexpected-argument = unexpected argument: { $arg }
daemon-error = error: { $error }
