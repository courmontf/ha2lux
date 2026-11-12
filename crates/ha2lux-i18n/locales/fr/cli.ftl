# ha2lux (CLI) — textes visibles. Les messages ne contiennent pas de retour à
# la ligne final : le code l'ajoute.

cli-percent = { $value } %
cli-unknown = inconnu

## Types et états d'écran
cli-kind-internal = interne
cli-kind-external = externe
cli-state-controllable = contrôlable
cli-state-unavailable = indisponible
cli-state-not-controllable = non contrôlable
cli-state-absent = absent

## list / get
cli-display-line = { $number }  { $name }  [{ $id }]  { $kind }  { $state }  niveau { $level }  plage { $low }-{ $high }
cli-list-empty = aucun écran détecté
cli-get-master = maître : { $level }  (source : { $mode })
cli-get-schedule-resume = planning suspendu jusqu'à { $resume }
cli-get-dimmed = atténuation d'inactivité en cours
cli-display-level = { $name } (n°{ $number }) : { $level }

## auto status
cli-auto-enabled = activée
cli-auto-disabled = désactivée
cli-auto-configured = configurée
cli-auto-needs-url-entity = non configurée (URL et entité requises)
cli-auto-needs-url-entity-token = non configurée (URL, entité et jeton requis : ha2lux auto token)
cli-auto-summary = auto-luminosité : { $enabled }, { $configured }
cli-auto-state-line = état : { $state }
cli-auto-token-rejected = jeton refusé par Home Assistant : enregistrez-en un nouveau (ha2lux auto token)
cli-auto-raw = valeur brute du capteur : { $raw }
cli-auto-lux = éclairement lissé : { $lux } lx
cli-auto-lux-with-latest = éclairement lissé : { $lux } lx (dernière mesure : { $latest } lx)
cli-auto-target = niveau visé par la courbe : { $level }
cli-auto-applied = niveau appliqué : { $level }
cli-auto-curve = courbe ({ $kind }) : { $points }
cli-auto-curve-default = par défaut
cli-auto-curve-learned = apprise
cli-auto-curve-point = { $lux } lx → { $level }
cli-auto-source = source de pilotage : { $mode }
cli-auto-state-inactive = inactif
cli-auto-state-not-configured = non configuré
cli-auto-state-token-rejected = jeton refusé
cli-auto-state-offline = hors ligne
cli-auto-state-degraded = dégradé
cli-auto-state-waiting = en attente d'une mesure
cli-auto-state-adjusting = en ajustement
cli-auto-state-stale = pas de mesure récente
cli-auto-state-active = actif
cli-sensors-none = aucun capteur d'éclairement (device_class illuminance) trouvé

## Confirmations d'interrupteurs
cli-toggle-schedule-on = planning activé
cli-toggle-schedule-off = planning désactivé
cli-toggle-idle-on = atténuation d'inactivité activée : niveau { $level } %, après { $minutes } min d'inactivité
cli-toggle-idle-off = atténuation d'inactivité désactivée
cli-toggle-auto-on = auto-luminosité activée
cli-toggle-auto-off = auto-luminosité désactivée

## Retours des commandes
cli-token-cleared = jeton Home Assistant supprimé
cli-token-saved = jeton Home Assistant enregistré
cli-curve-reset = courbe apprise oubliée
cli-warn-schedule-empty = avertissement : le planning est activé mais ne contient aucun point (rien ne sera appliqué)

## Erreurs (préfixées par « erreur : » à l'affichage)
cli-error = erreur : { $message }
cli-err-json = sérialisation JSON impossible : { $error }
cli-err-no-reply = aucune réponse en { $seconds } s
cli-err-unexpected-reply = réponse inattendue du démon
cli-err-unexpected-reply-short = réponse inattendue
cli-err-daemon-absent = le démon ha2luxd est absent ({ $detail }) ; lancez-le : systemctl --user start ha2luxd
cli-err-daemon-no-reply = le démon ha2luxd ne répond pas ({ $detail })
cli-detail-no-owner = aucun propriétaire pour org.ha2lux.Daemon
cli-detail-timeout = délai dépassé
cli-err-display-unavailable = écran indisponible : { $name }
cli-err-level-unknown = niveau actuel de { $name } inconnu
cli-err-token-read = lecture du jeton impossible : { $error }
cli-err-token-empty = aucun jeton reçu : envoyez-le sur l'entrée standard (ex. : ha2lux auto token < fichier)
cli-displays-available = écrans disponibles :
cli-see-list = (voir `ha2lux list`)

## Sélection d'écran
cli-select-out-of-range = écran n°{ $given } introuvable ({ $count } écran(s))
cli-select-not-found = aucun écran ne correspond à « { $spec } »
cli-select-ambiguous = « { $spec } » désigne plusieurs écrans : { $candidates }
cli-select-candidate = n°{ $number }

## Analyse des arguments
cli-usage = Utilisation :
cli-heading-commands = Commandes
cli-heading-global = Options globales
cli-value-command = COMMANDE
cli-usage-hint = Essayez `ha2lux --help`, ou `ha2lux <commande> --help` pour le détail d'une commande.
cli-err-not-integer-between = « { $text } » n'est pas un entier entre { $low } et { $high }
cli-err-empty-value = la valeur ne peut pas être vide
cli-usage-unknown-command = commande inconnue : { $value }
cli-usage-unknown-argument = option ou argument inattendu : { $arg }
cli-usage-missing-argument = argument requis manquant : { $arg }
cli-usage-missing-command = commande manquante
cli-usage-reason = { " " }: { $reason }
cli-usage-possible-values = { " " }(valeurs possibles : { $values })
cli-usage-invalid-value = valeur invalide « { $value } » pour { $arg }{ $reason }
cli-usage-conflict = arguments incompatibles : { $arg }
cli-usage-too-many = trop de valeurs pour { $arg } (une seule attendue)
cli-usage-missing-value = valeur manquante pour { $arg }

## Aide : commandes (cli-cmd-<chemin>), arguments (cli-arg-<chemin>-<id>), noms de valeurs (cli-val-<chemin>-<id>)
cli-cmd-root = Règle la luminosité de tous les écrans en parlant au démon `ha2luxd`
cli-cmd-set = Définit le niveau (maître, ou celui d'un écran avec --display)
cli-cmd-up = Augmente le niveau d'un pas
cli-cmd-down = Diminue le niveau d'un pas
cli-cmd-get = Affiche le niveau maître et ceux des écrans
cli-cmd-list = Liste les écrans, numérotés
cli-cmd-preset = Applique un preset (insensible à la casse)
cli-cmd-schedule = Active ou désactive le planning horaire
cli-cmd-idle-dim = Active ou désactive l'atténuation à l'inactivité
cli-cmd-auto = Auto-luminosité (capteur Home Assistant)
cli-cmd-auto-on = Active l'auto-luminosité
cli-cmd-auto-off = Désactive l'auto-luminosité
cli-cmd-auto-status = Affiche l'état de l'auto-luminosité
cli-cmd-auto-reset-curve = Oublie la courbe apprise
cli-cmd-auto-token = Enregistre le jeton Home Assistant (lu sur l'entrée standard, jamais en argument)
cli-cmd-auto-sensors = Liste les capteurs d'éclairement de Home Assistant
cli-cmd-rescan = Relance l'énumération des écrans
cli-cmd-reload = Recharge la configuration
cli-cmd-doctor = Diagnostic de la chaîne DDC/CI et backlight (sans démon)

cli-arg-root-help = Affiche l'aide
cli-arg-root-version = Affiche la version
cli-arg-root-json = Sortie JSON (pour les scripts) ; les erreurs sont aussi renvoyées en JSON
cli-arg-set-level = Niveau de 0 à 100
cli-arg-set-display = Écran visé : numéro dans `ha2lux list`, nom exact ou sous-chaîne du nom (insensible à la casse). Sans cette option, la commande agit sur le niveau maître
cli-arg-up-step = Pas de 1 à 100 (défaut : le réglage `general.step` du démon, 5)
cli-arg-up-display = Écran visé : numéro dans `ha2lux list`, nom exact ou sous-chaîne du nom (insensible à la casse). Sans cette option, la commande agit sur le niveau maître
cli-arg-down-step = Pas de 1 à 100 (défaut : le réglage `general.step` du démon, 5)
cli-arg-down-display = Écran visé : numéro dans `ha2lux list`, nom exact ou sous-chaîne du nom (insensible à la casse). Sans cette option, la commande agit sur le niveau maître
cli-arg-get-display = Écran visé : numéro dans `ha2lux list`, nom exact ou sous-chaîne du nom (insensible à la casse). Sans cette option, la commande agit sur le niveau maître
cli-arg-preset-name = Nom du preset
cli-arg-schedule-state = `on` ou `off`
cli-arg-idle-dim-state = `on` ou `off`
cli-arg-auto-token-clear = Supprime le jeton enregistré au lieu d'en saisir un
cli-arg-doctor-probe = Envoie de VRAIES trames DDC/CI (lecture de la luminosité) aux écrans externes

cli-val-set-level = NIVEAU
cli-val-set-display = ÉCRAN
cli-val-up-step = PAS
cli-val-up-display = ÉCRAN
cli-val-down-step = PAS
cli-val-down-display = ÉCRAN
cli-val-get-display = ÉCRAN
cli-val-schedule-state = ÉTAT
cli-val-idle-dim-state = ÉTAT
