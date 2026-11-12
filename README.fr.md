# ha2lux

*[English](README.md) · Français*

Règle la luminosité de **tous vos écrans** sous Linux/Wayland — moniteurs externes (DDC/CI
par `/dev/i2c-*`) et écran interne de portable (backlight) — avec un **niveau maître**, des
**plages par écran**, des **presets**, un **planning horaire**, une **atténuation à
l'inactivité** et une **auto-luminosité** pilotée par un capteur d'éclairement
[Home Assistant](https://www.home-assistant.io/) (avec apprentissage de votre courbe).

Trois programmes :

| Binaire | Rôle |
|---|---|
| `ha2luxd` | démon utilisateur (D-Bus de session `org.ha2lux.Daemon`) : seul à toucher au matériel |
| `ha2lux` | ligne de commande (à lier à vos raccourcis clavier) |
| `ha2lux-tray` | icône de zone de notification (StatusNotifierItem) : menu, molette, OSD |

> **État** : tout le logiciel est testé sans matériel (écrans factices, faux Home Assistant,
> faux compositeur Wayland, bus D-Bus privés). La validation **sur vos écrans** reste à faire :
> voir `specs/VALIDATION_MATERIEL.md`.

## Compilation

```sh
cargo build --release          # binaires dans target/release/
./scripts/ci.sh                # fmt + clippy + tests
```

Rust 1.85 ou plus récent (voir `rust-toolchain.toml`).

## Installation

### Installation rapide (depuis les sources)

Il faut une chaîne Rust 1.85 ou plus récente ([rustup](https://rustup.rs)) et un éditeur de liens
C (`build-essential` sous Debian/Ubuntu, `base-devel` sous Arch, `gcc` sous Fedora). Puis, à la
racine du dépôt, copiez-collez :

```sh
# 1. Compiler
cargo build --release

# 2. Installer les programmes pour votre utilisateur (sans sudo)
mkdir -p ~/.local/bin
install -Dm755 target/release/ha2luxd target/release/ha2lux target/release/ha2lux-tray -t ~/.local/bin/
sed "s|/usr/bin/ha2luxd|$HOME/.local/bin/ha2luxd|" packaging/systemd/ha2luxd.service \
    | install -Dm644 /dev/stdin ~/.config/systemd/user/ha2luxd.service
install -Dm644 packaging/dbus/org.ha2lux.Daemon.service ~/.local/share/dbus-1/services/org.ha2lux.Daemon.service
sed "s|/usr/bin/ha2lux-tray|$HOME/.local/bin/ha2lux-tray|" packaging/xdg/autostart/ha2lux-tray.desktop \
    | install -Dm644 /dev/stdin ~/.config/autostart/ha2lux-tray.desktop

# 3. Autoriser l'accès aux bus I²C des écrans (sudo requis ; moniteurs externes seulement)
sudo install -m644 packaging/modules-load.d/ha2lux-i2c-dev.conf /etc/modules-load.d/ && sudo modprobe i2c-dev
sudo install -m644 packaging/udev/60-ha2lux-i2c.rules /etc/udev/rules.d/ \
    && sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev

# 4. Démarrer le démon et l'icône
systemctl --user daemon-reload && systemctl --user enable --now ha2luxd
setsid -f ~/.local/bin/ha2lux-tray >/dev/null 2>&1

# 5. Vérifier que tout fonctionne
ha2lux doctor && ha2lux get
```

Remarques :

- `~/.local/bin` doit être dans votre `PATH` (c'est le cas par défaut sur la plupart des
  distributions ; sinon ajoutez `export PATH="$HOME/.local/bin:$PATH"` à votre profil de shell).
- L'étape 3 ne concerne que les **moniteurs externes** ; l'écran interne d'un portable fonctionne
  sans. Si `ha2lux doctor` signale encore « accès refusé » après l'étape 3, fermez puis rouvrez
  votre session.
- L'icône démarrera toute seule à votre prochaine connexion. Sous GNOME, installez d'abord
  l'extension « AppIndicator », sinon aucune icône n'apparaît.
- Rien d'autre que les fichiers ci-dessus n'est touché : ha2lux ne modifie jamais votre système
  de lui-même.

### Mise à jour

```sh
git pull && cargo build --release \
&& install -Dm755 target/release/ha2luxd target/release/ha2lux target/release/ha2lux-tray -t ~/.local/bin/ \
&& systemctl --user restart ha2luxd \
&& pkill -x ha2lux-tray; setsid -f ~/.local/bin/ha2lux-tray >/dev/null 2>&1
```

Redémarrez toujours le démon après une mise à jour : le démon et les autres programmes doivent
avoir la même version. Le sous-menu **À propos** de l'icône affiche les deux versions et avertit
si elles diffèrent.

### Désinstallation

```sh
systemctl --user disable --now ha2luxd; pkill -x ha2lux-tray
rm -f ~/.local/bin/{ha2luxd,ha2lux,ha2lux-tray} ~/.config/systemd/user/ha2luxd.service \
      ~/.local/share/dbus-1/services/org.ha2lux.Daemon.service ~/.config/autostart/ha2lux-tray.desktop
sudo rm -f /etc/udev/rules.d/60-ha2lux-i2c.rules /etc/modules-load.d/ha2lux-i2c-dev.conf
```

Vos réglages restent dans `~/.config/ha2lux/` et `~/.local/state/ha2lux/` (supprimez-les si vous
le souhaitez).

### Installation manuelle (détails)

1. **Binaires** : copiez `ha2luxd`, `ha2lux`, `ha2lux-tray` dans `~/.local/bin/` (ou `/usr/bin/`).
2. **Droits sur les bus I²C** (écrans externes seulement) :
   - module noyau : `sudo install -m644 packaging/modules-load.d/ha2lux-i2c-dev.conf /etc/modules-load.d/ && sudo modprobe i2c-dev`
   - accès : `sudo install -m644 packaging/udev/60-ha2lux-i2c.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev`
     (alternative : un groupe `i2c`, détaillé dans le fichier de règles).
   - vérifiez : `ha2lux doctor` (puis `ha2lux doctor --probe` pour tester la lecture réelle).
3. **Démon** : voir l'en-tête de `packaging/systemd/ha2luxd.service`, puis
   `systemctl --user enable --now ha2luxd`. Le fichier `packaging/dbus/org.ha2lux.Daemon.service`
   permet aussi un démarrage à la demande.
4. **Icône** (facultatif) : lancez `ha2lux-tray`, ou copiez `packaging/xdg/autostart/ha2lux-tray.desktop`
   dans `~/.config/autostart/` (adaptez son chemin `Exec=`). Sous GNOME, installez l'extension « AppIndicator ».

Des squelettes de paquets (Arch, RPM, Debian) sont dans `packaging/` — **non testés**.

## Utilisation

```sh
ha2lux set 40            # niveau maître 40 %
ha2lux up / ha2lux down  # un pas (réglage general.step, 5 par défaut)
ha2lux up 10
ha2lux get               # état et niveau de chaque écran
ha2lux list              # écrans connus
ha2lux set 30 --display DELL     # un seul écran (numéro, nom ou sous-chaîne)
ha2lux preset Nuit
ha2lux schedule on|off   ha2lux idle-dim on|off   ha2lux auto on|off
ha2lux rescan            ha2lux reload            ha2lux doctor
```

`--json` donne une sortie structurée. Codes de sortie : 0 succès · 1 usage · 2 démon absent ·
3 commande refusée · 4 démon sans réponse.

### Raccourcis clavier (Wayland)

Sous Wayland, une application ne capture pas les touches globales : liez-les à la CLI dans
votre bureau.

- **KDE Plasma** : Paramètres système → Raccourcis → Ajouter une commande : `ha2lux up` / `ha2lux down`.
- **GNOME** : Paramètres → Clavier → Raccourcis personnalisés : `ha2lux up` / `ha2lux down`.
- **Hyprland** (`hyprland.conf`) : `binde = , XF86MonBrightnessUp, exec, ha2lux up`
  et `binde = , XF86MonBrightnessDown, exec, ha2lux down` (`binde` répète la touche maintenue).
- **sway** (`config`) : `bindsym --locked XF86MonBrightnessUp exec ha2lux up` (idem `down`).

### Icône de zone de notification

Menu : niveau courant, « Tous les écrans » et un sous-menu par écran (niveaux de 10 en 10 %),
presets, auto-luminosité / planning / atténuation, re-scan, réglages (ouvre `config.toml`), à propos (versions de l'icône et du démon),
quitter. **Molette** sur l'icône : ± un pas (les crans rapides sont regroupés) avec une notification
de progression. Il n'y a pas de popup à glisseurs (écart documenté : `specs/DECISIONS.md` n° 84).

## Configuration

`~/.config/ha2lux/config.toml` (créé au premier lancement, tolérant : une clé invalide est signalée
dans le journal et ignorée ; un fichier illisible n'est jamais réécrit). Exemple :

```toml
version = 1

[general]
step = 5
edid_bus_matching = false      # true : repli par EDID pour les cartes NVIDIA

[presets]
Nuit = 15
Jour = 80

[schedule]
enabled = true
points = [{ time = "07:30", level = 70 }, { time = "22:00", level = 15 }]

[idle]
enabled = true
level = 5            # niveau pendant l'inactivité
minutes = 5

[auto]
enabled = false
url = "http://homeassistant.local:8123"
entity = "sensor.salon_illuminance"
```

Après modification : `ha2lux reload`. Les niveaux appris et l'état sont dans
`~/.local/state/ha2lux/state.toml`.

### Auto-luminosité (Home Assistant)

1. Créez un **jeton d'accès longue durée** dans votre profil Home Assistant.
2. Renseignez `[auto] url` et `entity` (`ha2lux auto sensors` liste les capteurs d'éclairement
   une fois le jeton enregistré), puis `ha2lux reload`.
3. Enregistrez le jeton **par l'entrée standard** (jamais en argument de ligne de commande) :
   `ha2lux auto token < jeton.txt`. Il est rangé dans le trousseau du bureau (Secret Service),
   à défaut dans `~/.config/ha2lux/secrets.json` (permissions 0600).
4. `ha2lux auto on`, puis `ha2lux auto status` pour suivre l'état.

Le jeton est lié à l'hôte:port : changer d'adresse oblige à le ressaisir. Vos réglages manuels
(raccourci, molette, preset…) sont **appris** : la courbe lux → niveau s'adapte (annulable :
`ha2lux auto reset-curve`). Si Home Assistant est injoignable, le planning prend le relais.

## Langues

L'interface (CLI, icône, messages du démon, `ha2lux doctor`) existe en **français** et en
**anglais**. La langue suit votre environnement (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`) ;
`HA2LUX_LANG=fr` ou `HA2LUX_LANG=en` la force. Les autres langues retombent sur l'anglais.
Ajouter une langue : voir `crates/ha2lux-i18n/src/lib.rs`. Les journaux restent en anglais.

## Dépannage

- `ha2lux doctor` : état des modules, des droits `/dev/i2c-*`, du backlight, des écrans détectés.
- Journal du démon : `journalctl --user -u ha2luxd -f` (`RUST_LOG=debug` pour plus de détails).
- Un écran qui répond mal au DDC/CI peut demander quelques secondes après un réveil : le démon
  réessaie avec un délai croissant puis attend (cooldown) sans jamais bloquer les autres écrans.
- Le démon remplace une instance déjà en cours ; il refuse de démarrer si l'ancienne ne libère
  pas le nom D-Bus en 5 s (jamais deux démons sur un bus DDC).

## Sécurité et limites

Voir `specs/MENACES.md`. En bref : aucune écriture matérielle hors des commandes DDC/CI de
luminosité (VCP 0x10) et du backlight via logind ; aucun `unsafe` hors besoin documenté ; le jeton
n'apparaît ni dans les journaux, ni dans la configuration, ni en argument ; TLS strict (aucune
option pour ignorer un certificat) ; les redirections HTTP ne sont jamais suivies. Limites connues :
le plein écran n'est pas détecté pour l'atténuation d'inactivité ; pas de popup à glisseurs.

## Architecture

`crates/` : `ha2lux-core` (logique pure, sans E/S), `ha2lux-backends` (DDC/CI, backlight, découverte),
`ha2lux-ha` (client Home Assistant), `ha2lux-secrets` (trousseau), `ha2lux-ipc` (interface D-Bus),
`ha2lux-i18n` (traductions), `ha2luxd`, `ha2lux-cli`, `ha2lux-tray`. Détails : `specs/ARCHITECTURE.md`. Décisions et écarts :
`specs/DECISIONS.md`. Fuzzing : `fuzz/README.md`.

## Licence

[MIT](LICENSE) — Copyright (c) 2026 courmontf.
