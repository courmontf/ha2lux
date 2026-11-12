#!/usr/bin/env bash
# Détecte les fuites probables (secrets, données personnelles) dans les fichiers donnés.
# Usage : scripts/check-secrets.sh <fichier>...     (sans argument : tous les fichiers suivis par git)
# Code de sortie : 0 rien trouvé, 1 fuite probable.
# Pour ignorer une ligne précise (faux positif volontaire, ex. jeton factice de test) :
# ajouter « # secret-scan: ok » (ou le commentaire équivalent) sur la même ligne.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -gt 0 ]; then
    files=("$@")
else
    mapfile -t files < <(git ls-files 2>/dev/null)
fi

# Expressions à signaler (ERE). Chaque entrée : « nom|expression ».
patterns=(
    'clé privée|-----BEGIN [A-Z ]*PRIVATE KEY-----'
    'jeton JWT (Home Assistant, etc.)|eyJ[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{10,}'
    'jeton GitHub|gh[pousr]_[A-Za-z0-9]{30,}'
    'clé AWS|AKIA[0-9A-Z]{16}'
    'secret en clair|(token|secret|passw(or)?d|api[_-]?key)[a-z_]*[[:space:]]*[:=][[:space:]]*"[A-Za-z0-9_./+=-]{24,}"'
    'chemin personnel|/home/[a-z][a-z0-9_-]*/'
    'adresse e-mail|[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+\.[A-Za-z]{2,}'
)
# Valeurs tolérées : exemples génériques et adresses « noreply ».
allow='/home/(u|alice|user|name|example)/|user:pw@|noreply|@example\.(com|org|net)|@(localhost|users\.noreply)|name@host|@types/'

status=0
for entry in "${patterns[@]}"; do
    name=${entry%%|*}
    regex=${entry#*|}
    for f in "${files[@]}"; do
        [ -f "$f" ] || continue
        case "$f" in
            scripts/check-secrets.sh|Cargo.lock|*.png|*.jpg|*.svg|*.lock) continue ;;
        esac
        out=$(grep -InE -e "$regex" -- "$f" 2>/dev/null | grep -vE -e "$allow" | grep -v "secret-scan: ok" || true)
        if [ -n "$out" ]; then
            printf 'FUITE PROBABLE (%s) dans %s :\n%s\n\n' "$name" "$f" "$out" >&2
            status=1
        fi
    done
done
[ "$status" -eq 0 ] && echo "scan de fuites : rien trouvé" >&2
exit "$status"
