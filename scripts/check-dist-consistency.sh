#!/bin/sh
set -eu

# Assert that everything claiming to know the release target list agrees with
# scripts/release-targets.sh.
#
# AGENTS.md and docs/releasing.md both tell readers that release-targets.sh is
# the definition of what a release contains. That is only true if nothing else
# quietly holds a second copy, and two things have to:
#
#   deny.toml     scopes the dependency policy to the triples afi ships. A
#                 target in the matrix but missing here has its dependencies
#                 checked by nothing, and the run still reports green.
#   install.sh    maps a user's machine to a target. It is fetched over curl and
#                 run with no repository around it, so it cannot read the list at
#                 run time and has to carry its own.
#
# Neither can be deduplicated away. This is the check that keeps them honest
# instead, so a documented single source of truth is one in fact.
#
# A third copy lives in another repository: the formula template in
# smykla-skalski/homebrew-tap names four of these archives. This script cannot
# read it and does not try. That copy is kept honest by failure instead -- the
# tap asks for the archives by name with `gh release download --pattern`, so a
# rename here turns its next run red rather than pinning a file that no longer
# exists, and the release waits on that run.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
targets_sh="$repo_root/scripts/release-targets.sh"

failures=0
fail() {
    printf 'DRIFT: %s\n' "$1" >&2
    failures=$((failures + 1))
}

# The authority. One triple per line.
known=$(
    "$targets_sh" --matrix \
        | tr ',' '\n' \
        | sed -n 's/.*"target":"\([^"]*\)".*/\1/p' \
        | sort
)

if [ -z "$known" ]; then
    printf 'could not read any target from %s\n' "$targets_sh" >&2
    exit 1
fi

printf 'release targets:\n'
printf '%s\n' "$known" | sed 's/^/  /'

# --- deny.toml has to cover exactly the same set -----------------------------

denied=$(
    awk '
      /^targets[[:space:]]*=[[:space:]]*\[/ { inside = 1; next }
      inside && /\]/ { exit }
      inside { print }
    ' "$repo_root/deny.toml" \
        | sed -n 's/.*"\([^"]*\)".*/\1/p' \
        | sort
)

printf '\ndeny.toml [graph] targets:\n'
for target in $known; do
    if printf '%s\n' "$denied" | grep -qxF "$target"; then
        printf '  ok       %s\n' "$target"
    else
        fail "deny.toml does not cover $target, so its dependencies go unchecked"
    fi
done
for target in $denied; do
    printf '%s\n' "$known" | grep -qxF "$target" || \
        fail "deny.toml lists $target, which is not a release target"
done

# --- install.sh may serve a subset, never something that does not exist ------

# The triples on the right-hand side of the case arms that pick a build.
served=$(
    sed -n 's/^[[:space:]]*[A-Za-z]*:[A-Za-z0-9_]*)[[:space:]]*target=\([A-Za-z0-9_.-]*\).*/\1/p' \
        "$repo_root/scripts/install.sh" \
        | sort -u
)

printf '\ninstall.sh serves:\n'
if [ -z "$served" ]; then
    fail "no target triples found in install.sh; has its case block changed shape?"
fi
for target in $served; do
    if printf '%s\n' "$known" | grep -qxF "$target"; then
        printf '  ok       %s\n' "$target"
    else
        fail "install.sh offers $target, which no release builds"
    fi
done

# --- and it has to ask for filenames a release actually publishes ------------

# install.sh's own templates, read out of the file. The first version of this
# check rebuilt "afi-$target.tar.gz" here instead, which made both sides of the
# comparison the same expression: it agreed with itself no matter what install.sh
# said, and renaming the templates to something no release publishes still passed.
#
# Extracted with the literal `$target` left in, then expanded per target below,
# so a change to the shape of the name is compared rather than assumed.
extract_template() {
    sed -n "s/^$1=\"\\(afi[^\"]*\\)\"[[:space:]]*$/\\1/p" \
        "$repo_root/scripts/install.sh" \
        | head -1
}

archive_template=$(extract_template archive)
checksum_template=$(extract_template checksum)

printf '\ninstall.sh asks for:\n'
printf '  %s\n  %s\n' "${archive_template:-<none>}" "${checksum_template:-<none>}"

# A template this cannot find is the same failure as a wrong one: the comparison
# below would silently have nothing to check. Fail rather than pass empty.
if [ -z "$archive_template" ] || [ -z "$checksum_template" ]; then
    fail "could not read the archive/checksum templates from install.sh; has it changed shape?"
fi

# Any released version will do: the tarball and checksum names carry the target
# and not the version, which is the part being compared.
assets=$("$targets_sh" --assets 0.0.0-1)
for target in $served; do
    for template in "$archive_template" "$checksum_template"; do
        [ -n "$template" ] || continue
        want=$(printf '%s\n' "$template" | sed "s/\\\$target/$target/g")
        printf '%s\n' "$assets" | grep -qxF "$want" || \
            fail "install.sh would fetch $want, which is not a published asset name"
    done
done

if [ "$failures" -ne 0 ]; then
    printf '\n%s inconsistenc(ies) with %s\n' "$failures" "$targets_sh" >&2
    exit 1
fi

printf '\neverything agrees with scripts/release-targets.sh\n'
