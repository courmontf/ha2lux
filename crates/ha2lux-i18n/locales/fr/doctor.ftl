# ha2lux doctor — rapport de diagnostic (rendu texte et résumés des vérifications).
# Les étiquettes de gravité (OK, INFO, WARN, FAIL) et les identifiants de
# vérification (module-i2c-dev, acces-i2c…) restent fixes : ils servent aux scripts.

doctor-title = ha2lux doctor
doctor-check-line = [{ $tag }] { $id } : { $summary }

## Sections du rapport
doctor-heading-connectors = Connecteurs DRM
doctor-heading-buses = Bus I²C
doctor-heading-backlights = Rétroéclairages
doctor-heading-probes = Sondes DDC/CI (VCP 0x10)
doctor-kind-internal = interne
doctor-kind-external = externe
doctor-state-connected = connecté
doctor-state-free = libre
doctor-access-ok = accès ok
doctor-access-denied = accès refusé
doctor-bus-none = pas de bus DDC
doctor-bus-with-access = i2c-{ $bus } ({ $access })
doctor-connector-line = { $name } : { $state }, { $kind }, { $bus }
doctor-edid-line = EDID : { $manufacturer } { $product } série { $serial } « { $name } » → id { $id }
doctor-edid-line-bad-checksum = EDID : { $manufacturer } { $product } série { $serial } « { $name } » → id { $id } (somme de contrôle fausse)
doctor-bus-line = i2c-{ $bus } : { $name } ({ $access })
doctor-bus-line-gpu = i2c-{ $bus } : { $name } ({ $access }) [bus DDC de carte graphique]
doctor-backlight-line = { $name } : type { $kind }, max { $max }, courant { $current }
doctor-probe-line-ok = { $connector } ({ $id }) : max { $max }, courant { $current } = { $level } % [bus i2c-{ $bus } via { $source }]
doctor-probe-line-fail = { $connector } : ÉCHEC — { $reason }
doctor-probe-unknown-reason = raison inconnue

## Notes sur les connecteurs
doctor-note-no-ddc-bus = aucun bus DDC exposé : écran derrière un dock/MST/adaptateur, ou pilote NVIDIA propriétaire
doctor-note-edid-missing = EDID absent
doctor-note-edid-checksum = somme de contrôle EDID fausse (fréquent, sans gravité)
doctor-note-internal-panel = panneau interne : piloté par le backlight, jamais en DDC

## Vérifications
doctor-module-i2c-dev-ok = module i2c-dev chargé (ou intégré au noyau)
doctor-module-i2c-dev-missing = module i2c-dev absent : aucun /dev/i2c-* n'est créé
doctor-module-i2c-dev-missing-hint =
    à lancer vous-même : sudo modprobe i2c-dev
    au démarrage : copier packaging/modules-load.d/ha2lux-i2c-dev.conf dans /etc/modules-load.d/

doctor-nodes-none = aucun nœud /dev/i2c-*
doctor-nodes-none-hint = charger le module i2c-dev, puis relancer ha2lux doctor
doctor-nodes-ok = { $count } nœud(s) /dev/i2c-*

doctor-access-denied-buses = accès refusé aux bus des écrans : { $list }
doctor-access-denied-item = { $name } (i2c-{ $bus }) : { $error }
doctor-access-denied-absent = { $name } (i2c-{ $bus }) : nœud /dev/i2c-{ $bus } absent
doctor-access-hint =
    installer packaging/udev/60-ha2lux-i2c.rules dans /etc/udev/rules.d/, puis : sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=i2c-dev
    ou ajouter l'utilisateur au groupe i2c et se reconnecter
doctor-access-none-accessible = aucun nœud /dev/i2c-* n'est accessible par cet utilisateur
doctor-access-not-evaluated = non évalué : aucun nœud
doctor-access-summary = { $accessible }/{ $total } nœud(s) accessible(s)

doctor-udev-ok = règle udev trouvée : { $path }
doctor-udev-missing = aucune règle udev donnant accès à i2c-dev n'a été trouvée
doctor-udev-missing-hint = fichier fourni : packaging/udev/60-ha2lux-i2c.rules (à installer dans /etc/udev/rules.d/)

doctor-group-member = groupe i2c présent (gid { $gid }), utilisateur membre
doctor-group-not-member = groupe i2c présent (gid { $gid }), utilisateur non membre
doctor-group-none = pas de groupe i2c (l'accès repose sur l'ACL udev « uaccess »)

doctor-ddcci = le pilote noyau ddcci est chargé : il peut occuper l'adresse 0x37 et gêner l'accès direct
doctor-ddcci-hint = décharger ddcci si l'écriture échoue avec « device busy »

doctor-nvidia = pilote NVIDIA propriétaire détecté : les bus DDC des connecteurs peuvent ne pas être exposés dans sysfs
doctor-nvidia-hint = si un écran externe n'a pas de bus DDC, utiliser les nœuds i2c du pilote NVIDIA

doctor-drm-none = aucun connecteur DRM lisible dans /sys/class/drm
doctor-drm-ok = { $connectors } connecteur(s), { $externals } écran(s) externe(s) connecté(s)

doctor-ddc-connectors = écran(s) externe(s) sans bus DDC exposé : { $list }
doctor-ddc-connectors-hint-gpu = pilote NVIDIA : les bus DDC existent (voir « Bus I²C ») mais ne sont pas liés aux connecteurs ; ha2lux doctor --probe lit l'EDID de chaque bus GPU pour les associer
doctor-ddc-connectors-hint-dock = dock, MST, adaptateur DP→HDMI ou DisplayLink : le DDC y est souvent absent ; surcharge possible par écran (ddc = false)

doctor-edid = écran(s) connecté(s) sans EDID lisible : { $list }
doctor-edid-hint = sans EDID, l'écran n'a pas d'identité stable et n'est pas piloté

doctor-backlight-ok = { $count } rétroéclairage(s) détecté(s)
doctor-backlight-missing = panneau interne connecté mais aucun rétroéclairage dans /sys/class/backlight
doctor-backlight-missing-hint = certains portables n'en exposent pas (pilote hybride, firmware) : voir acpi_backlight= dans la ligne de commande du noyau
doctor-backlight-no-internal = aucun panneau interne (poste fixe)

doctor-session-ok = session logind { $id } (écriture du backlight via SetBrightness)
doctor-session-missing = pas de XDG_SESSION_ID : le backlight ne sera réglable que depuis une session graphique
doctor-session-missing-hint = lancer depuis la session de bureau (pas via SSH) ; un service systemd --user retombe sur la session graphique de l'utilisateur

doctor-probe-node-inaccessible = nœud inaccessible, sonde ignorée
doctor-probe-timeout = aucune réponse en { $seconds } s
doctor-probe-none = aucun écran externe à sonder
doctor-probe-ok = { $count } écran(s) répondent en DDC/CI
doctor-probe-failed = pas de réponse DDC/CI : { $list }
doctor-probe-failed-hint = vérifier que DDC/CI est activé dans le menu OSD de l'écran, le câble, et éviter dock/KVM ; un écran qui dort répond « null message »
doctor-probe-not-run = sonde DDC non exécutée (elle envoie de vraies trames aux écrans)
doctor-probe-not-run-hint = ajouter --probe pour lire la luminosité (VCP 0x10) de chaque écran externe
