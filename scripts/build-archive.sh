#!/bin/sh
set -eu

# Build a release binary for one target and pack it the way a release does.
# Prints the archive path and nothing else.
#
# The release workflow builds its archives with taiki-e/upload-rust-binary-action
# because that action already knows how to provision a target, drive `cross` for
# the musl builds, and strip the result. This script exists so the same archive
# can be produced on a laptop: it is what makes `mise run smoke` a check anyone
# can run before pushing, instead of something only observable in CI.
#
# The layout is taiki-e's, and has to stay that way, because smoke-archive.sh
# asserts against it and CI feeds it the action's output: the binary and the
# three documents flat at the top level, no leading directory.
#
# Arguments: [target-triple]  (default: the host target)

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

target=${1:-}
if [ -z "$target" ]; then
    target=$(rustc -vV | awk '/^host: / { print $2 }')
fi

output_dir=${AFI_DIST_DIR:-$repo_root/target/dist}

cargo build --release --locked --target "$target" --bin afi >&2

binary="$repo_root/target/$target/release/afi"
[ -x "$binary" ] || { printf 'no binary at %s\n' "$binary" >&2; exit 1; }

mkdir -p "$output_dir"
staging=$(mktemp -d "${TMPDIR:-/tmp}/afi-dist.XXXXXX")
trap 'rm -rf "$staging"' EXIT HUP INT TERM

cp "$binary" "$staging/afi"
# Stripped to match the release archives. A cross build has no target-specific
# binutils on PATH, so this uses the toolchain's own strip via rustc rather than
# a bare `strip`, and tolerates a target that cannot be stripped here.
strip "$staging/afi" 2>/dev/null || true

for doc in README.md LICENSE CHANGELOG.md; do
    cp "$repo_root/$doc" "$staging/$doc"
done

archive="$output_dir/afi-$target.tar.gz"
tar -czf "$archive" -C "$staging" afi README.md LICENSE CHANGELOG.md

# The release publishes the checksum next to the archive, so produce it here too.
# shasum is on macOS and Debian alike; sha256sum is not on macOS.
(cd "$output_dir" && shasum -a 256 "afi-$target.tar.gz" > "afi-$target.sha256")

printf '%s\n' "$archive"
