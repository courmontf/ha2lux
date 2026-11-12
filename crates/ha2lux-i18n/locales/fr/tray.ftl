# ha2lux-tray — icône de zone de notification : menu, infobulle, affichage de luminosité.

tray-percent = { $value } %

## Menu
tray-daemon-unreachable = Le démon ha2luxd ne répond pas
tray-quit = Quitter
tray-brightness = Luminosité : { $level }
tray-brightness-unknown = Luminosité : inconnue
tray-all-displays = Tous les écrans
tray-display-with-level = { $name } ({ $level })
tray-display-unavailable = { $name } (indisponible)
tray-preset = { $name } — { $level }
tray-auto-configured = Auto-luminosité (capteur Home Assistant)
tray-auto-unconfigured = Auto-luminosité (non configurée)
tray-sensor-ok = capteur : à jour
tray-sensor-stale = capteur : mesure périmée
tray-sensor-degraded = capteur : dégradé
tray-sensor-offline = capteur : hors ligne
tray-sensor-unknown = capteur : inconnu
tray-schedule-fallback = Planning (sert si Home Assistant est hors ligne)
tray-schedule = Planning horaire
tray-schedule-resume = Planning suspendu jusqu'à { $resume }
tray-idle-dim = Atténuation à l'inactivité ({ $level } % après { $minutes } min)
tray-rescan = Re-scanner les écrans
tray-settings = Réglages…
tray-about = À propos
tray-about-tray = Icône ha2lux-tray { $version }
tray-about-daemon = Démon ha2luxd { $version }
tray-about-daemon-unreachable = Démon ha2luxd : injoignable
tray-about-daemon-unknown = Démon ha2luxd : version inconnue
tray-about-mismatch = Versions différentes : redémarrez le démon

## Infobulle
tray-tooltip-unreachable = démon injoignable
tray-tooltip = { $level } · { $mode }
tray-tooltip-dimmed = { $level } · { $mode } · atténué

## Affichage à l'écran (notification)
tray-osd = Luminosité { $level } %

## Ligne de commande et erreurs
tray-usage =
    usage : ha2lux-tray [--help | --version]

    Icône de zone de notification (StatusNotifierItem) pour ha2lux : menu des presets,
    molette pour ajuster la luminosité, bascules planning / auto / atténuation.
    Le démon ha2luxd doit tourner ; l'icône l'interroge par D-Bus.

    codes de sortie : 0 arrêt normal, 1 usage, 3 pas de zone de notification ou de bus
tray-unexpected-argument = argument inattendu : { $arg }
tray-error = erreur : { $error }
tray-err-bus = bus de session indisponible : { $error }
tray-err-no-watcher = aucune zone de notification (StatusNotifierWatcher) : { $error }. Sous GNOME, installez l'extension « AppIndicator ».
tray-detail-no-host = aucun hôte d'icônes
