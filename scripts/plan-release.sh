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

# Whether CHANGELOG.md carries a section for a version. Used to catch
# `set-version` moving a heading instead of adding one - see below.
#
# A literal prefix match, not a pattern. Interpolating the version into a regex
# makes every `.` in it match any character, so `0.6.0` also answers yes to a
# heading reading `0x6y0`. That direction matters: the caller reads a yes for the
# *previous* version as "its section survived", so a false positive would wave
# through exactly the destructive rename this exists to stop.
changelog_has() {
    awk -v want="## [$1]" '
        index($0, want) == 1 { found = 1; exit }
        END { exit !found }
    ' "$repo_root/CHANGELOG.md"
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
        had_previous=false
        if changelog_has "$before"; then
            had_previous=true
        fi

        # `set-version` does not add a changelog section. It renames the topmost
        # one. So give it one of its own to rename: `update` writes a section for
        # the version it would have picked, listing every commit since the last
        # tag, and `set-version` then forces the number to the one that was asked
        # for. The entry is correct either way, because the commits it lists do
        # not depend on what the release ends up being called.
        #
        # Without this, the topmost section is the *previous release's*, and
        # renaming that ships it under the new number and deletes it from the
        # file. Measured on a tree at 0.5.0 with no pending bump: `set-version
        # 0.6.0` rewrote `## [0.5.0]` to `## [0.6.0]`, silently.
        #
        # `update` is allowed to decline. It writes nothing when no commit since
        # the last tag is releasable, and it cannot work at all until the package
        # has a baseline to diff against. Both leave the previous section on top,
        # which the check below catches.
        release-plz update >&2 || true

        # release-plz reports what it did on stdout, and this script's stdout is
        # the version alone, so its chatter goes to stderr with everything else.
        release-plz set-version "$requested" >&2

        if ! changelog_has "$requested"; then
            printf 'set-version left no "## [%s]" section in CHANGELOG.md\n' \
                "$requested" >&2
            exit 1
        fi
        if [ "$had_previous" = true ] && ! changelog_has "$before"; then
            printf 'Refusing to release %s: it would carry the release notes of %s.\n' \
                "$requested" "$before" >&2
            printf '\n' >&2
            printf 'release-plz had no new changelog section to give this release, so\n' >&2
            printf 'set-version renamed the %s section instead of adding one. That drops\n' \
                "$before" >&2
            printf '%s from CHANGELOG.md and gives its notes to %s.\n' "$before" "$requested" >&2
            printf '\n' >&2
            printf 'Usually this means there is nothing to release: no commit since v%s is a\n' \
                "$before" >&2
            printf 'feat, fix, perf, or refactor. It also happens when the package has no\n' >&2
            printf 'baseline yet, which is the state a crate rename leaves behind.\n' >&2
            printf '\n' >&2
            printf 'To release anyway, land the version and its changelog section on the\n' >&2
            printf 'default branch; a release then publishes the manifest as it stands.\n' >&2
            # set-version already wrote before this check could run. A release
            # runner is thrown away, so this only matters to someone running the
            # script by hand -- say so rather than guess which of their changes
            # to discard.
            printf '\n' >&2
            printf 'CHANGELOG.md, Cargo.toml and Cargo.lock have been modified in place.\n' >&2
            exit 1
        fi
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
