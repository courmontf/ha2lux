# Changelog

Format inspiré de [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) ;
versions selon [SemVer](https://semver.org/lang/fr/).

## [0.7.1] - 2026-11-12

### Autres changements

- chore(deny): retire les licences autorisées devenues inutiles (da3fb19)
- docs: traduit en anglais les commentaires du code et des fichiers de configuration (a5af3ab)

## [0.7.0] - 2026-11-12

### Nouveautés

- **i18n** : langue réglable dans config.toml ([general] language) (ae9fdc3)
- **tray** : « Recharger la configuration » dans le menu, langue changée à chaud (2ada2c4)

### Autres changements

- chore(build): réduit les informations de débogage du profil dev (a97392e)

## [0.6.0] - 2026-11-12

### Nouveautés

- **Internationalisation** : l'interface existe en français et en anglais (CLI et son aide,
  icône, messages du démon, `ha2lux doctor`). La langue suit l'environnement (`LANGUAGE`,
  `LC_ALL`, `LC_MESSAGES`, `LANG`) ; `HA2LUX_LANG` la force ; les autres langues retombent sur
  l'anglais. Ajouter une langue = ajouter des fichiers Fluent (voir `crates/ha2lux-i18n`).
- **Icône** : sous-menu « À propos » avec les versions de l'icône et du démon, et un
  avertissement si elles diffèrent (démon non redémarré après une mise à jour).
- **README** en anglais (version française dans `README.fr.md`), avec installation rapide,
  mise à jour et désinstallation.
- **Publication** : `scripts/release.sh` calcule la version d'après les commits (SemVer +
  Conventional Commits), met à jour les fichiers, génère ce changelog et crée le tag ; un hook
  `commit-msg` avertit si un message ne suit pas la convention.

### Modifications

- Les journaux et messages techniques sont désormais en anglais, quelle que soit la langue.
- Les erreurs du démon voyagent sous forme de clé de message et d'arguments : chaque client les
  traduit dans sa propre langue.
- Packaging : description systemd en anglais, `Comment[fr]` dans le fichier `.desktop`.
