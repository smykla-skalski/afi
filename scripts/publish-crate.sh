#!/bin/sh
set -eu

# Publish the crate to crates.io, unless that version is already there.
#
# Run last in a release, after the reconciliation gate and after the GitHub
# release has been made visible. This is the only step of a release that cannot
# be undone: a crate can be yanked, which stops new dependents resolving to it,
# but it can never be removed. Everything ahead of it is reversible, so it goes
# at the end where a failure earlier cannot leave it stranded.
#
# release-plz used to do this in the same step that created the tag. See the
# comment on `publish` in release-plz.toml for why it no longer does.
#
# Idempotent on purpose. Re-running a release that failed after this point is the
# documented recovery, and `cargo publish` on a version that already exists fails
# with "crate version is already uploaded". Asking the registry first turns that
# into an ordinary no-op rather than something the operator has to read a log to
# understand.
#
# Inputs from the environment:
#
#   CARGO_REGISTRY_TOKEN  a crates.io token with publish-update scope
#
# Arguments: none.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

name=$(
    awk '
      /^[[:space:]]*\[/ { in_package = ($0 ~ /^[[:space:]]*\[package\]/); next }
      in_package && /^[[:space:]]*name[[:space:]]*=/ {
        sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]*#.*$/, "")
        gsub(/^"|"$/, ""); print; exit
      }
    ' "$repo_root/Cargo.toml"
)
version=$("$repo_root/scripts/cargo-version.sh")

[ -n "$name" ] || { printf 'no name in the [package] section of Cargo.toml\n' >&2; exit 1; }

# The sparse index, not the API: it is the file cargo itself resolves against, so
# a hit here means cargo would already refuse the upload.
#
# All four of the index's path layouts, not just the one this crate happens to
# use. A wrong path 404s, and a 404 is read below as "not published yet" -- so
# getting the layout wrong for a short name would silently turn the idempotency
# check into an unconditional publish attempt.
case ${#name} in
    1) path="1/$name" ;;
    2) path="2/$name" ;;
    3) path="3/$(printf '%s' "$name" | cut -c1)/$name" ;;
    *) path="$(printf '%s' "$name" | cut -c1-2)/$(printf '%s' "$name" | cut -c3-4)/$name" ;;
esac
index="https://index.crates.io/$path"

published=$(
    curl -fsS "$index" 2>/dev/null \
        | sed -n 's/.*"vers":"\([^"]*\)".*/\1/p' \
        || true
)

if printf '%s\n' "$published" | grep -qxF "$version"; then
    printf '%s %s is already on crates.io; nothing to publish\n' "$name" "$version"
    exit 0
fi

printf 'publishing %s %s to crates.io\n' "$name" "$version"

: "${CARGO_REGISTRY_TOKEN:?CARGO_REGISTRY_TOKEN is required to publish}"

# --locked so the published crate resolves the versions this release was built
# and tested against, rather than whatever is newest at upload time.
cd "$repo_root"
cargo publish --locked
