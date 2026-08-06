#!/bin/sh
set -eu

# Print the version from Cargo.toml's [package] section.
#
# Deliberately toolchain-free. The release workflow reads the version before
# mise or rustup has supplied cargo, so `cargo metadata` is not available yet;
# a runner without Rust preinstalled would otherwise die on the version guard
# rather than on the build.
#
# Scoped to [package] on purpose: [dependencies] holds entries like
# `serde = { version = "1" }` that an unscoped match would return first.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
manifest=${1:-$repo_root/Cargo.toml}

version=$(awk '
  /^[[:space:]]*\[/ { in_package = ($0 ~ /^[[:space:]]*\[package\]/); next }
  in_package && /^[[:space:]]*version[[:space:]]*=/ {
    sub(/^[^=]*=[[:space:]]*/, "")
    sub(/[[:space:]]*#.*$/, "")
    sub(/[[:space:]]*$/, "")
    gsub(/^"|"$/, "")
    print
    exit
  }
' "$manifest")

if [ -z "$version" ]; then
    printf 'no version in the [package] section of %s\n' "$manifest" >&2
    exit 1
fi

printf '%s\n' "$version"
