# ha2lux doctor — diagnostic report (text rendering and check summaries).
# Severity tags (OK, INFO, WARN, FAIL) and check identifiers (module-i2c-dev,
# acces-i2c…) stay fixed: scripts rely on them.

doctor-title = ha2lux doctor
doctor-check-line = [{ $tag }] { $id }: { $summary }

## Report sections
doctor-heading-connectors = DRM connectors
doctor-heading-buses = I²C buses
doctor-heading-backlights = Backlights
doctor-heading-probes = DDC/CI probes (VCP 0x10)
doctor-kind-internal = internal
doctor-kind-external = external
doctor-state-connected = connected
doctor-state-free = free
doctor-access-ok = access ok
doctor-access-denied = access denied
doctor-bus-none = no DDC bus
doctor-bus-with-access = i2c-{ $bus } ({ $access })
doctor-connector-line = { $name }: { $state }, { $kind }, { $bus }
doctor-edid-line = EDID: { $manufacturer } { $product } serial { $serial } "{ $name }" → id { $id }
doctor-edid-line-bad-checksum = EDID: { $manufacturer } { $product } serial { $serial } "{ $name }" → id { $id } (bad checksum)
doctor-bus-line = i2c-{ $bus }: { $name } ({ $access })
doctor-bus-line-gpu = i2c-{ $bus }: { $name } ({ $access }) [graphics card DDC bus]
doctor-backlight-line = { $name }: type { $kind }, max { $max }, current { $current }
doctor-probe-line-ok = { $connector } ({ $id }): max { $max }, current { $current } = { $level }% [bus i2c-{ $bus } via { $source }]
doctor-probe-line-fail = { $connector }: FAILED — { $reason }
doctor-probe-unknown-reason = unknown reason

## Connector notes
doctor-note-no-ddc-bus = no DDC bus exposed: display behind a dock/MST/adapter, or proprietary NVIDIA driver
doctor-note-edid-missing = EDID missing
doctor-note-edid-checksum = bad EDID checksum (common, harmless)
doctor-note-internal-panel = internal panel: driven by the backlight, never over DDC

## Checks
doctor-module-i2c-dev-ok = i2c-dev module loaded (or built into the kernel)
doctor-module-i2c-dev-missing = i2c-dev module missing: no /dev/i2c-* is created
doctor-module-i2c-dev-missing-hint =
    run it yourself: sudo modprobe i2c-dev
    at boot: copy packaging/modules-load.d/ha2lux-i2c-dev.conf to /etc/modules-load.d/

doctor-nodes-none = no /dev/i2c-* node
doctor-nodes-none-hint = load the i2c-dev module, then run ha2lux doctor again
doctor-nodes-ok = { $count } /dev/i2c-* node(s)

doctor-access-denied-buses = access denied to the displays' buses: { $list }
doctor-access-denied-item = { $name } (i2c-{ $bus }): { $error }
doctor-access-denied-absent = { $name } (i2c-{ $bus }): node /dev/i2c-{ $bus } missing
doctor-access-hint =
    install packaging/udev/60-ha2lux-i2c.rules in /etc/udev/rules.d/, then: sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev
    or add your user to the i2c group and log in again
doctor-access-none-accessible = no /dev/i2c-* node is accessible by this user
doctor-access-not-evaluated = not evaluated: no node
doctor-access-summary = { $accessible }/{ $total } node(s) accessible

doctor-udev-ok = udev rule found: { $path }
doctor-udev-missing = no udev rule granting access to i2c-dev was found
doctor-udev-missing-hint = provided file: packaging/udev/60-ha2lux-i2c.rules (to install in /etc/udev/rules.d/)

doctor-group-member = i2c group present (gid { $gid }), user is a member
doctor-group-not-member = i2c group present (gid { $gid }), user is not a member
doctor-group-none = no i2c group (access relies on the udev "uaccess" ACL)

doctor-ddcci = the ddcci kernel driver is loaded: it may hold address 0x37 and get in the way of direct access
doctor-ddcci-hint = unload ddcci if writes fail with "device busy"

doctor-nvidia = proprietary NVIDIA driver detected: connectors' DDC buses may not be exposed in sysfs
doctor-nvidia-hint = if an external display has no DDC bus, use the NVIDIA driver's i2c nodes

doctor-drm-none = no readable DRM connector in /sys/class/drm
doctor-drm-ok = { $connectors } connector(s), { $externals } external display(s) connected

doctor-ddc-connectors = external display(s) without an exposed DDC bus: { $list }
doctor-ddc-connectors-hint-gpu = NVIDIA driver: the DDC buses exist (see "I²C buses") but are not linked to connectors; ha2lux doctor --probe reads the EDID of each GPU bus to match them
doctor-ddc-connectors-hint-dock = dock, MST, DP→HDMI adapter or DisplayLink: DDC is often missing there; per-display override possible (ddc = false)

doctor-edid = connected display(s) without a readable EDID: { $list }
doctor-edid-hint = without an EDID the display has no stable identity and is not controlled

doctor-backlight-ok = { $count } backlight(s) detected
doctor-backlight-missing = internal panel connected but no backlight in /sys/class/backlight
doctor-backlight-missing-hint = some laptops do not expose one (hybrid driver, firmware): see acpi_backlight= on the kernel command line
doctor-backlight-no-internal = no internal panel (desktop machine)

doctor-session-ok = logind session { $id } (backlight written through SetBrightness)
doctor-session-missing = no XDG_SESSION_ID: the backlight can only be adjusted from a graphical session
doctor-session-missing-hint = run from the desktop session (not over SSH); a systemd --user service falls back to the user's graphical session

doctor-probe-node-inaccessible = node inaccessible, probe skipped
doctor-probe-timeout = no reply within { $seconds } s
doctor-probe-none = no external display to probe
doctor-probe-ok = { $count } display(s) answer over DDC/CI
doctor-probe-failed = no DDC/CI reply: { $list }
doctor-probe-failed-hint = check that DDC/CI is enabled in the display's OSD menu, check the cable, and avoid docks/KVMs; a sleeping display answers "null message"
doctor-probe-not-run = DDC probe not run (it sends real frames to the displays)
doctor-probe-not-run-hint = add --probe to read the brightness (VCP 0x10) of each external display
