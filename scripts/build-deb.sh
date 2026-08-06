#!/bin/sh
set -eu

# Package an already-built release binary as a .deb. Prints the path of the
# package it created, and nothing else, so a caller can consume stdout directly.
#
# --no-build: the caller has already built the binary. The release workflow
# builds and uploads the tarball first, so reusing that same binary makes the
# .deb and the tarball ship identical bytes instead of two compilations that
# could drift apart.
#
# --no-strip, --no-dbgsym, --no-separate-debug-symbols: the binary arrives
# stripped. cargo-deb's own strip would need a target-specific binutils that a
# cross build has no reason to carry on PATH, and a debug-symbol package split
# out of an already-stripped binary would be empty. Disabling all three leaves
# exactly one .deb in the output directory, which the release workflow relies on.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

if [ $# -lt 1 ]; then
    printf 'usage: %s <target-triple> [output-dir]\n' "$0" >&2
    exit 2
fi

target=$1
output_dir=${2:-$repo_root/target/debian}

binary="$repo_root/target/$target/release/afi"
if [ ! -x "$binary" ]; then
    printf 'no release binary at %s\n' "$binary" >&2
    printf 'build it first: cargo build --release --locked --target %s --bin afi\n' "$target" >&2
    exit 1
fi

if ! command -v cargo-deb >/dev/null 2>&1; then
    printf 'cargo-deb is not on PATH. Run "mise install" to provision it.\n' >&2
    exit 1
fi

deb_version=$("$repo_root/scripts/deb-version.sh")

mkdir -p "$output_dir"

cargo-deb \
    --manifest-path "$repo_root/Cargo.toml" \
    --target "$target" \
    --deb-version "$deb_version" \
    --output "$output_dir" \
    --no-build \
    --no-strip \
    --no-dbgsym \
    --no-separate-debug-symbols
