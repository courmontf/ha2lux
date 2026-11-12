#!/usr/bin/env bash
# Prepares a release: computes the next version number from the commits (SemVer +
# Conventional Commits), updates the files, generates the CHANGELOG, commits and
# creates the annotated tag vX.Y.Z. NEVER pushes: the final command is printed.
#
# Rules (Conventional Commits, https://www.conventionalcommits.org) since the last version:
#   type!: …  or  "BREAKING CHANGE:" in the body      -> major   (minor while the version is 0.x)
#   feat: …                                            -> minor
#   fix: …  or  perf: …                                -> patch
#   docs, chore, refactor, test, ci, build, style, i18n… never trigger a version on their own.
# While the version is 0.x, a breaking change does not go to 1.0.0: it bumps the
# minor (Cargo convention). To go to 1.0.0, force it: --bump major.
#
# Usage : scripts/release.sh [--dry-run] [--yes] [--bump major|minor|patch]
#   --dry-run  prints the version and the changelog without changing anything
#   --yes      does not ask for confirmation
#   --bump X   forces the level (useful if the commits do not follow the convention)
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

dry=0 yes=0 force=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) dry=1 ;;
        --yes) yes=1 ;;
        --bump)
            shift
            force="${1:-}"
            case "$force" in major | minor | patch) ;; *) echo "--bump attend major, minor ou patch" >&2; exit 2 ;; esac
            ;;
        -h | --help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "option inconnue : $1 (voir --help)" >&2; exit 2 ;;
    esac
    shift
done

die() { echo "erreur : $*" >&2; exit 1; }

# --- Starting state -----------------------------------------------------------------------------
current=$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"$/\1/p}' Cargo.toml | head -1)
[[ "$current" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || die "version actuelle illisible dans Cargo.toml : « $current »"
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}

if [ "$dry" -eq 0 ] && [ -n "$(git status --porcelain)" ]; then
    die "l'arbre de travail n'est pas propre : commitez ou rangez vos changements d'abord"
fi

# Base: the tag of the current version, otherwise the last commit that changed the version line.
if git rev-parse -q --verify "refs/tags/v$current" >/dev/null; then
    base=$(git rev-list -n1 "v$current")
else
    base=$(git log -1 --format=%H -G'^version = "' -- Cargo.toml)
fi
[ -n "$base" ] || die "impossible de déterminer le point de départ"

# --- Commit analysis --------------------------------------------------------------------------
level=0 # 0 none, 1 patch, 2 minor, 3 major
breaking=() feats=() fixes=() perfs=() others=()
re='^([a-zA-Z]+)(\(([^)]+)\))?(!)?:[[:space:]]*(.+)$'
while IFS= read -r -d $'\x1e' record; do
    record=${record#$'\n'}
    [ -n "$record" ] || continue
    hash=${record%%$'\x1f'*}; rest=${record#*$'\x1f'}
    subject=${rest%%$'\x1f'*}; body=${rest#*$'\x1f'}
    short=${hash:0:7}
    type="" scope="" bang="" desc="$subject"
    if [[ "$subject" =~ $re ]]; then
        type=${BASH_REMATCH[1]} scope=${BASH_REMATCH[3]} bang=${BASH_REMATCH[4]} desc=${BASH_REMATCH[5]}
        type=$(echo "$type" | tr 'A-Z' 'a-z')
    fi
    [ "$type" = "chore" ] && [ "$scope" = "release" ] && continue
    line="$desc"
    [ -n "$scope" ] && line="**$scope** : $desc"
    line="$line ($short)"
    is_breaking=0
    [ -n "$bang" ] && is_breaking=1
    grep -qE '^BREAKING[ -]CHANGE:' <<<"$body" && is_breaking=1
    if [ "$is_breaking" -eq 1 ]; then
        breaking+=("$line"); [ "$level" -lt 3 ] && level=3
        continue
    fi
    case "$type" in
        feat) feats+=("$line"); [ "$level" -lt 2 ] && level=2 ;;
        fix) fixes+=("$line"); [ "$level" -lt 1 ] && level=1 ;;
        perf) perfs+=("$line"); [ "$level" -lt 1 ] && level=1 ;;
        *) others+=("$subject ($short)") ;;
    esac
done < <(git log --reverse --format='%H%x1f%s%x1f%b%x1e' "$base..HEAD")

total=$(( ${#breaking[@]} + ${#feats[@]} + ${#fixes[@]} + ${#perfs[@]} + ${#others[@]} ))
[ "$total" -gt 0 ] || die "aucun commit depuis la version $current : rien à publier"

if [ -n "$force" ]; then
    case "$force" in major) level=3 ;; minor) level=2 ;; patch) level=1 ;; esac
fi
[ "$level" -gt 0 ] || die "aucun commit « feat », « fix » ou « perf » depuis v$current (${#others[@]} autre(s)) : rien à publier automatiquement. Forcez avec --bump patch|minor|major."

# --- New version number ---------------------------------------------------------------------------------
if [ "$level" -eq 3 ] && [ "$major" -eq 0 ] && [ "$force" != "major" ]; then
    level=2 # 0.x: a breaking change bumps the minor
fi
case "$level" in
    3) new="$((major + 1)).0.0" ;;
    2) new="$major.$((minor + 1)).0" ;;
    1) new="$major.$minor.$((patch + 1))" ;;
esac
git rev-parse -q --verify "refs/tags/v$new" >/dev/null && die "le tag v$new existe déjà"

# --- Changelog --------------------------------------------------------------------------------------
section() { # title, items…
    local title=$1; shift
    [ $# -gt 0 ] || return 0
    printf '\n### %s\n\n' "$title"
    local item
    for item in "$@"; do printf -- '- %s\n' "$item"; done
}
entry=$(
    printf '## [%s] - %s\n' "$new" "$(date +%F)"
    section "Changements incompatibles" "${breaking[@]+"${breaking[@]}"}"
    section "Nouveautés" "${feats[@]+"${feats[@]}"}"
    section "Corrections" "${fixes[@]+"${fixes[@]}"}"
    section "Performances" "${perfs[@]+"${perfs[@]}"}"
    section "Autres changements" "${others[@]+"${others[@]}"}"
)

echo "Version : $current -> $new   (depuis $(git rev-parse --short "$base"), $total commit(s))"
echo
echo "$entry"
echo
[ "$dry" -eq 1 ] && { echo "(--dry-run : rien n'a été modifié)"; exit 0; }

if [ "$yes" -eq 0 ]; then
    read -r -p "Créer la version v$new (commit + tag, sans pousser) ? [o/N] " answer
    [[ "$answer" =~ ^[oOyY]$ ]] || { echo "annulé"; exit 1; }
fi

# --- File updates -----------------------------------------------------------------------
sed -i '/^\[workspace.package\]/,/^\[/{s/^version = ".*"$/version = "'"$new"'"/}' Cargo.toml
[ -f packaging/rpm/ha2lux.spec ] && sed -i 's/^Version:\( *\).*/Version:\1'"$new"'/' packaging/rpm/ha2lux.spec
[ -f packaging/arch/PKGBUILD ] && sed -i 's/^pkgver=.*/pkgver='"$new"'/' packaging/arch/PKGBUILD
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null
grep -q "version = \"$new\"" Cargo.lock || die "Cargo.lock n'a pas été mis à jour"

printf '%s\n' "$entry" >CHANGELOG.entry
if [ -f CHANGELOG.md ]; then
    # Inserts the new version before the first one already present (or at the end of the file).
    awk -v entry=CHANGELOG.entry '
        /^## \[/ && !done { while ((getline l < entry) > 0) print l; print ""; done = 1 }
        { print }
        END { if (!done) { print ""; while ((getline l < entry) > 0) print l } }
    ' CHANGELOG.md >CHANGELOG.md.new
else
    {
        printf '# Changelog\n\nFormat inspiré de [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/) ;\n'
        printf 'versions selon [SemVer](https://semver.org/lang/fr/).\n\n'
        cat CHANGELOG.entry
    } >CHANGELOG.md.new
fi
rm -f CHANGELOG.entry
mv CHANGELOG.md.new CHANGELOG.md

git add Cargo.toml Cargo.lock CHANGELOG.md packaging/rpm/ha2lux.spec packaging/arch/PKGBUILD
git commit -q -m "chore(release): v$new"
git tag -a "v$new" -m "ha2lux $new"

echo
echo "Version v$new créée (commit + tag). Pour la publier :"
echo "    git push --follow-tags"
