# UNTESTED: RPM package skeleton (Fedora/openSUSE). Validate before any use.
Name:           ha2lux
Version:        0.8.0
Release:        1%{?dist}
Summary:        Brightness control for all displays (DDC/CI, backlight)
License:        MIT
URL:            https://example.invalid/ha2lux
Source0:        %{name}-%{version}.tar.gz
BuildRequires:  cargo
Requires:       dbus
Recommends:     i2c-tools

%description
User daemon, CLI and notification-area icon that set the brightness of all displays
(DDC/CI through /dev/i2c-*, laptop backlight) with a master level, presets, a schedule,
idle dimming and auto-brightness from a Home Assistant sensor.

%prep
%autosetup

%build
cargo build --release --locked

%install
install -Dm755 target/release/ha2luxd %{buildroot}%{_bindir}/ha2luxd
install -Dm755 target/release/ha2lux %{buildroot}%{_bindir}/ha2lux
install -Dm755 target/release/ha2lux-tray %{buildroot}%{_bindir}/ha2lux-tray
install -Dm644 packaging/systemd/ha2luxd.service %{buildroot}%{_userunitdir}/ha2luxd.service
install -Dm644 packaging/dbus/org.ha2lux.Daemon.service %{buildroot}%{_datadir}/dbus-1/services/org.ha2lux.Daemon.service
install -Dm644 packaging/udev/60-ha2lux-i2c.rules %{buildroot}%{_udevrulesdir}/60-ha2lux-i2c.rules
install -Dm644 packaging/modules-load.d/ha2lux-i2c-dev.conf %{buildroot}%{_modulesloaddir}/ha2lux-i2c-dev.conf
install -Dm644 packaging/xdg/autostart/ha2lux-tray.desktop %{buildroot}%{_sysconfdir}/xdg/autostart/ha2lux-tray.desktop

%files
%license LICENSE
%{_bindir}/ha2luxd
%{_bindir}/ha2lux
%{_bindir}/ha2lux-tray
%{_userunitdir}/ha2luxd.service
%{_datadir}/dbus-1/services/org.ha2lux.Daemon.service
%{_udevrulesdir}/60-ha2lux-i2c.rules
%{_modulesloaddir}/ha2lux-i2c-dev.conf
%{_sysconfdir}/xdg/autostart/ha2lux-tray.desktop
