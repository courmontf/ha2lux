# NON TESTÉ : squelette de paquet RPM (Fedora/openSUSE). À valider avant tout usage.
Name:           ha2lux
Version:        0.7.0
Release:        1%{?dist}
Summary:        Réglage de la luminosité de tous les écrans (DDC/CI, backlight)
License:        MIT
URL:            https://example.invalid/ha2lux
Source0:        %{name}-%{version}.tar.gz
BuildRequires:  cargo
Requires:       dbus
Recommends:     i2c-tools

%description
Démon utilisateur, CLI et icône de zone de notification qui règlent la luminosité de
tous les écrans (DDC/CI par /dev/i2c-*, backlight du portable) avec niveau maître,
presets, planning, atténuation à l'inactivité et auto-luminosité par capteur Home Assistant.

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
