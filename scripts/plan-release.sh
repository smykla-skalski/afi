#!/bin/sh
set -eu

# Decide which version to release and write it into Cargo.toml, Cargo.lock, and
# CHANGELOG.md. Prints that version on stdout and nothing else.
#
# Exit 3 means "nothing to release": no commit since the last tag was of a type
# that release-plz.toml treats as releasable. That is the ordinary outcome of the
# daily run on a quiet day, so the caller should treat it as success.
#
# Inputs, both optional and both from the environment so the caller does not have
# to quote a command line through GitHub Actions:
#
#   AFI_RELEASE_VERSION  an exact version, bypassing the commit history entirely
#   AFI_RELEASE_FORCE    "true" to release even when no releasable commit exists

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

requested=${AFI_RELEASE_VERSION:-}
force=${AFI_RELEASE_FORCE:-false}

semver='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'

before=$("$repo_root/scripts/cargo-version.sh")

if [ -n "$requested" ]; then
    requested=${requested#v}
    if ! printf '%s\n' "$requested" | grep -Eq "$semver"; then
        printf '%s is not a semver version\n' "$requested" >&2
        exit 1
    fi
    # An explicit version equal to the current one would produce a tag that
    # already exists. Say so here rather than failing later against the remote.
    if [ "$requested" = "$before" ]; then
        printf 'Cargo.toml is already at %s; pick a different version\n' "$requested" >&2
        exit 1
    fi
    release-plz set-version "$requested"
else
    release-plz update
fi

after=$("$repo_root/scripts/cargo-version.sh")

if [ "$after" = "$before" ]; then
    if [ "$force" != "true" ]; then
        exit 3
    fi
    # Forced with nothing releasable in the history. release-plz has no opinion
    # to offer, so take the smallest step that still produces a new version.
    major=$(printf '%s\n' "$before" | cut -d. -f1)
    minor=$(printf '%s\n' "$before" | cut -d. -f2)
    patch=$(printf '%s\n' "$before" | cut -d. -f3 | cut -d- -f1 | cut -d+ -f1)
    after="${major}.${minor}.$((patch + 1))"
    release-plz set-version "$after"
fi

printf '%s\n' "$after"
