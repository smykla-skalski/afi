#!/bin/sh
set -eu

# Decide which version to release and write it into Cargo.toml, Cargo.lock, and
# CHANGELOG.md. Prints that version on stdout and nothing else.
#
# The tag is the record of what shipped, not Cargo.toml, and every decision below
# asks the tag rather than the manifest.
#
# That matters because the bump lands on the default branch in one step and the tag
# is created in the next, so anything failing in between leaves a version in the
# manifest that was never released - and release-plz reads `v<manifest version>` as
# the previous release, so it cannot recover from that state on its own. It sees no
# such tag, concludes nothing was ever released, and offers the entire history as
# one version. The untagged case is therefore handled here before release-plz is
# asked anything.
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

# Local tags only. The caller checks the remote before publishing; here the
# question is what this checkout has already released, and a full clone has every
# tag because release-plz needs them to work out the version at all.
released() {
    git -C "$repo_root" rev-parse -q --verify "refs/tags/v$1" >/dev/null 2>&1
}

before=$("$repo_root/scripts/cargo-version.sh")

if [ -n "$requested" ]; then
    requested=${requested#v}
    if ! printf '%s\n' "$requested" | grep -Eq "$semver"; then
        printf '%s is not a semver version\n' "$requested" >&2
        exit 1
    fi
    # Asking for a version that already shipped would produce a duplicate tag.
    # Asking for the one sitting untagged in the manifest is the resume case, so
    # the tag decides, not the manifest.
    if released "$requested"; then
        printf 'v%s is already released; pick a different version\n' "$requested" >&2
        exit 1
    fi
    if [ "$requested" != "$before" ]; then
        # release-plz reports what it did on stdout, and this script's stdout is
        # the version alone, so its chatter goes to stderr with everything else.
        release-plz set-version "$requested" >&2
    fi
    printf '%s\n' "$requested"
    exit 0
fi

# A manifest version with no tag is a release that got as far as the bump commit
# and died before the tag. Publish it as it stands, and do not ask release-plz to
# recompute: it treats `v<manifest version>` as the previous release, so with that
# tag missing it concludes nothing has ever shipped and answers with the whole
# project history as one release. Measured, not assumed - run against this exact
# state it proposed 0.4.0 and a changelog going back to the first commit.
#
# The cost is that anything merged between the failed run and this one ships in
# the tree without appearing in the notes. That beats notes listing every commit
# since the beginning, and it only arises after a release has already failed.
if ! released "$before"; then
    printf 'Cargo.toml is at %s with no v%s tag: publishing it as it stands\n' \
        "$before" "$before" >&2
    printf '%s\n' "$before"
    exit 0
fi

release-plz update >&2

after=$("$repo_root/scripts/cargo-version.sh")

if [ "$after" != "$before" ]; then
    printf '%s\n' "$after"
    exit 0
fi

if [ "$force" != "true" ]; then
    exit 3
fi

# Forced with nothing releasable in the history. release-plz has no opinion to
# offer, so take the smallest step that still produces a new version.
major=$(printf '%s\n' "$before" | cut -d. -f1)
minor=$(printf '%s\n' "$before" | cut -d. -f2)
patch=$(printf '%s\n' "$before" | cut -d. -f3 | cut -d- -f1 | cut -d+ -f1)
after="${major}.${minor}.$((patch + 1))"
release-plz set-version "$after" >&2

printf '%s\n' "$after"
