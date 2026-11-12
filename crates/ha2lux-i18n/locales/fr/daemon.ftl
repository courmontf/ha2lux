# ha2luxd — messages envoyés aux clients (CLI, tray) dans les erreurs D-Bus, et
# ligne de commande du démon. Ces clés voyagent encodées (voir ha2lux_i18n::wire) :
# le client les traduit dans sa propre langue.

## Validation des requêtes
# $what : master-level, display-level, step ou preset-level.
err-out-of-range = { $what ->
    [master-level] niveau maître
    [display-level] niveau écran
    [step] pas
    [preset-level] preset
   *[other] valeur
 } hors bornes : { $value }
err-percent-out-of-range = { $what ->
    [master-level] niveau maître
    [display-level] niveau écran
    [step] pas
    [preset-level] preset
   *[other] valeur
 } hors bornes : { $value } (attendu 0 à 100)
err-display-id-invalid = identifiant d'écran invalide (1 à { $max } caractères, sans caractère de contrôle)
err-preset-name-invalid = nom de preset invalide (1 à { $max } caractères, sans caractère de contrôle)
err-token-empty = jeton vide
err-step-out-of-range = pas hors bornes : { $delta } (attendu 1 à 100, positif ou négatif)
err-range-out-of-range = plage hors bornes : ({ $low }, { $high }) (attendu 0 à 100)
err-unknown-preset = preset inconnu : { $name } ; disponibles : { $available }
err-unknown-feature = fonction inconnue : { $feature } (attendu : { $first }, { $second } ou { $third })

## Écrans
err-unknown-display = écran inconnu : { $id }
err-display-unavailable = écran indisponible : { $id }
err-no-controllable-display = aucun écran contrôlable

## Auto-luminosité et jeton Home Assistant
err-auto-needs-config = l'auto-luminosité exige l'URL de Home Assistant, l'entité du capteur et un jeton
err-no-ha-url = aucune URL Home Assistant valide dans la configuration ([auto] url)
err-no-token = aucun jeton enregistré pour cette adresse (ha2lux auto token)

## État du démon
err-daemon-stopped = le démon est arrêté

## Trousseau (jeton)
err-keyring-unavailable = trousseau indisponible : { $detail }
err-keyring-locked = trousseau verrouillé : déverrouillage refusé ou expiré
err-secret-file-too-open = { $path } : permissions trop larges (attendu 0600), fichier ignoré

## Home Assistant
err-ha-config-url = adresse de Home Assistant invalide : { $detail }
err-ha-config-entity = identifiant d'entité invalide (attendu : domaine.objet)
err-ha-config-no-token = jeton d'accès absent
err-ha-auth = authentification refusée : jeton invalide ou révoqué
err-ha-not-found = entité introuvable
err-ha-redirect = redirection refusée : le jeton n'est jamais renvoyé vers une autre adresse
err-ha-too-large = réponse trop volumineuse (plus de 256 Kio)
err-ha-tls = erreur TLS : { $detail }
err-ha-timeout = délai dépassé
err-ha-connection = connexion impossible : { $detail }
err-ha-protocol = réponse inattendue : { $detail }

## Ligne de commande du démon
daemon-usage =
    usage : ha2luxd [--help | --version]

    Démon utilisateur ha2lux : règle la luminosité des écrans (DDC/CI et backlight).
    Il remplace une instance déjà en cours, puis sert l'interface D-Bus org.ha2lux.Daemon1
    sur le bus de session. Niveau de journalisation : variable RUST_LOG (défaut : info).

    codes de sortie : 0 arrêt normal, 1 usage, 3 démarrage impossible
daemon-unexpected-argument = argument inattendu : { $arg }
daemon-error = erreur : { $error }
