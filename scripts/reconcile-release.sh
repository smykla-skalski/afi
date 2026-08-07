#!/bin/sh
set -eu

# Assert that a release is complete before anyone can see it.
#
# Two places have to agree about what shipped, and nothing used to compare them.
# v0.2.0 attached an amd64 .deb to its GitHub release and never pushed that same
# package to the apt repository: the release page offered it, `apt-get install`
# on amd64 could not find it, and the release was reported green. This is the
# check that fails instead.
#
# Run against the draft release, before it is published. Everything it looks at
# is already in place by then, so a gap here costs a failed run rather than a
# broken release.
#
# Inputs from the environment:
#
#   GH_TOKEN              a token that can read the draft release
#   CLOUDSMITH_WORKSPACE  the apt workspace
#   CLOUDSMITH_REPOSITORY the apt repository
#
# Arguments: <tag> <deb-version>
#
# The apt side is read through Cloudsmith's public API rather than the CLI. The
# repository is an open-source one and its package list is world-readable, so
# this needs no credential and no OIDC exchange: the gate can run in a job with
# no privileges at all. The push job immediately before it is what proves the
# publishing credential still works.

if [ $# -ne 2 ]; then
    printf 'usage: %s <tag> <deb-version>\n' "$0" >&2
    exit 2
fi

tag=$1
deb_version=$2

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
: "${CLOUDSMITH_WORKSPACE:?CLOUDSMITH_WORKSPACE is required}"
: "${CLOUDSMITH_REPOSITORY:?CLOUDSMITH_REPOSITORY is required}"

failures=0
fail() {
    printf 'MISSING: %s\n' "$1" >&2
    failures=$((failures + 1))
}

# --- the GitHub release ---------------------------------------------------

# The draft is not reachable by tag through the releases API, so find it by
# listing. `gh release view` handles drafts for a user who can see them.
attached=$(gh release view "$tag" --json assets --jq '.assets[].name' | sort)

printf 'release %s carries:\n' "$tag"
printf '%s\n' "$attached" | sed 's/^/  /'

expected=$("$repo_root/scripts/release-targets.sh" --assets "$deb_version" | sort)

printf '\nchecking against the target list:\n'
for want in $expected; do
    if printf '%s\n' "$attached" | grep -qxF "$want"; then
        printf '  ok       %s\n' "$want"
    else
        fail "$tag has no asset named $want"
    fi
done

# An asset nobody expected is as much a signal as a missing one: it means the
# target list and what the matrix built have drifted apart.
for have in $attached; do
    printf '%s\n' "$expected" | grep -qxF "$have" || \
        printf '  unexpected asset, not on the target list: %s\n' "$have" >&2
done

# --- the apt repository ---------------------------------------------------

printf '\nchecking the apt repository for %s:\n' "$deb_version"
# The version can contain a tilde for a prerelease, which is legal in a query
# string but has to survive the shell and the URL intact.
query=$(printf 'name:afi version:%s' "$deb_version" | jq -sRr @uri)
published=$(
    curl -fsS \
        "https://api.cloudsmith.io/v1/packages/${CLOUDSMITH_WORKSPACE}/${CLOUDSMITH_REPOSITORY}/?query=${query}" \
        | jq -r '.[] | select(.status_str == "Completed") | .architectures[].name' \
        | sort -u
)

for arch in $("$repo_root/scripts/release-targets.sh" --deb-arches); do
    if printf '%s\n' "$published" | grep -qxF "$arch"; then
        printf '  ok       afi %s %s in apt\n' "$deb_version" "$arch"
    else
        fail "the apt repository has no afi $deb_version for $arch"
    fi
done

if [ "$failures" -ne 0 ]; then
    printf '\n%s check(s) failed; the release stays a draft\n' "$failures" >&2
    exit 1
fi

printf '\nthe release and the apt repository agree on %s\n' "$tag"
