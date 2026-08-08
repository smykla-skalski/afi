#!/bin/sh
set -eu

# Ask the Homebrew tap to publish a version, then prove that it did.
#
# The formula and the workflow that rewrites it live in another repository, so
# all this can do is dispatch and check. The checking is the point: a
# `repository_dispatch` is accepted with a 204 whether or not any workflow
# matches it, so a release that dispatched and moved on would report success
# while the tap stayed a version behind. This waits for Formula/afi.rb itself to
# name the release, which is the same move reconcile-release.sh makes against
# Cloudsmith -- check the outcome, not the request.
#
# Run last in a release, after the crate. The crate cannot be undone and this
# can: a formula is one commit in a repository anyone with push access can move,
# so it has no business standing between the release going public and the one
# publication that is final. A failure here means the tap is a version behind
# and nothing else, which is what makes this the right place to re-run by hand.
#
# The formula carries no `version` stanza -- Homebrew reads the version out of
# the URLs -- so the URLs are what gets checked, which is also what a
# `brew install` will actually fetch.
#
# Inputs from the environment:
#
#   GH_TOKEN               a token that can dispatch to and read the tap
#   AFI_HOMEBREW_TAP       the tap repository, for pointing this at a fork
#   AFI_HOMEBREW_WORKFLOW  the workflow there that rewrites the formula
#   AFI_HOMEBREW_TIMEOUT   seconds to wait for the formula, default 900
#
# Arguments: [--check-only] <version>
#
# --check-only asks whether the tap is already current and dispatches nothing.
# Worth running when a release went red here and someone has since driven the
# tap by hand, or months later when nobody remembers whether it caught up.

tap=${AFI_HOMEBREW_TAP:-smykla-skalski/homebrew-tap}
workflow=${AFI_HOMEBREW_WORKFLOW:-update-afi-formula.yml}
timeout=${AFI_HOMEBREW_TIMEOUT:-900}
interval=15

check_only=false
if [ "${1:-}" = --check-only ]; then
    check_only=true
    shift
fi

if [ $# -ne 1 ]; then
    printf 'usage: %s [--check-only] <version>\n' "$0" >&2
    exit 2
fi
version=$1

# A prerelease is not an error, it is a version the tap deliberately does not
# carry: Homebrew has no notion of one, so a formula naming 0.9.0-rc.1 would
# take every `brew upgrade afi` onto a release candidate. Reported and skipped
# rather than refused, so a release that cuts one is not red for doing as it
# was told, and a hand-run says the same thing instead of quietly working.
#
# This is the only place afi decides it. The tap checks the shape again at its
# own end, because its workflow_dispatch takes a version from a hand this one
# never sees.
case $version in
    *-*)
        printf '%s is a prerelease; the tap carries stable releases only.\n' "$version"
        exit 0
        ;;
esac

# Whether the formula names this release. One request, and the only question
# that actually matters.
formula_names_version() {
    gh api "repos/$tap/contents/Formula/afi.rb?ref=main" \
        --header 'Accept: application/vnd.github.raw' 2>/dev/null \
        | grep -qF "/download/v$version/"
}

if [ "$check_only" = true ]; then
    if formula_names_version; then
        printf 'Formula/afi.rb on %s points at v%s.\n' "$tap" "$version"
        exit 0
    fi
    printf 'Formula/afi.rb on %s does not point at v%s.\n' "$tap" "$version" >&2
    exit 1
fi

# Read before dispatching, so the wait below cannot mistake the previous
# release's run for this one's. Zero when the workflow exists and has never run,
# which is the first release through here.
#
# It also answers the question worth asking before dispatching at all. `gh run
# list` fails with a 404 on a workflow that is not on the tap's default branch,
# and a dispatch that matches nothing is accepted and dropped, so without this
# the wait would sit out its whole deadline for something that was never going
# to start.
if ! before=$(
    gh run list --repo "$tap" --workflow "$workflow" \
        --limit 1 --json databaseId --jq '.[0].databaseId // 0' 2>/dev/null
); then
    printf '%s is not on the default branch of %s, so a dispatch goes nowhere.\n' \
        "$workflow" "$tap" >&2
    exit 1
fi

jq -n --arg v "$version" \
    '{event_type: "afi-release", client_payload: {version: $v}}' \
    | gh api --method POST "repos/$tap/dispatches" --input -
printf 'Dispatched afi-release %s to %s.\n' "$version" "$tap"

# The newest run this dispatch could have started, as two lines: its state, then
# its URL. Asked of jq in the shape the caller needs rather than as JSON to pick
# apart afterwards, so one request per pass answers both "has it failed" and
# "which run should the operator look at".
newest_run() {
    gh run list --repo "$tap" --workflow "$workflow" --limit 5 \
        --json databaseId,status,conclusion,url \
        --jq "[.[] | select(.databaseId > $before)] | first // empty
              | \"\(.status) \(.conclusion // \"none\")\", .url" \
        2>/dev/null || true
}

deadline=$(( $(date +%s) + timeout ))
url=

while [ "$(date +%s)" -lt "$deadline" ]; do
    # The tap installs the formula on a macOS runner to prove it works, so
    # nothing can have happened yet on the first pass.
    sleep "$interval"

    if formula_names_version; then
        printf 'Formula/afi.rb on %s now points at v%s.\n' "$tap" "$version"
        exit 0
    fi

    # A transient API error is not worth failing a release over; the next pass
    # asks again. A run that has finished badly is, and saying so as soon as it
    # is known costs a minute rather than the whole deadline.
    run=$(newest_run)
    [ -n "$run" ] || continue
    state=$(printf '%s\n' "$run" | head -1)
    url=$(printf '%s\n' "$run" | tail -1)
    case $state in
        "completed success") ;;
        "completed "*)
            printf "the tap's formula update ended as %s: %s\n" \
                "${state#completed }" "$url" >&2
            exit 1
            ;;
    esac
done

# Not "is the workflow there" -- the check above already settled that, and
# repeating it here would send the operator after a cause that was ruled out
# before anything was dispatched.
if [ -n "$url" ]; then
    tail=$(printf 'The tap run is %s' "$url")
else
    tail=$(printf 'No run started; is %s disabled?' "$workflow")
fi
printf 'Formula/afi.rb on %s did not reach v%s within %ss. %s\n' \
    "$tap" "$version" "$timeout" "$tail" >&2
exit 1
