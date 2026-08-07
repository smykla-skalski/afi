#!/bin/sh
set -eu

# The list of targets a release ships, and what each one produces.
#
# One list, two readers. The release workflow builds its matrix from `--matrix`,
# and the reconciliation gate asks `--assets <version>` what a complete release
# is supposed to contain. Keeping both on this file is the point: the old
# workflow hardcoded the matrix in YAML and had nothing at all checking the
# result, which is how v0.2.0 shipped an amd64 .deb to the release page and
# never to the apt repository without anyone noticing.
#
# Fields:
#   target      the Rust triple
#   os          linux or macos, which decides the runner
#   build_tool  cross, or empty for a native cargo build
#   deb         true when this target is packaged as a .deb
#   arch        the Debian architecture, for the targets that are packaged
#
# Only the musl targets are packaged. A static binary needs no Depends and
# installs on every Debian derivative, which is what lets one upload serve
# any-distro/any-version.

usage() {
    printf 'usage: %s --matrix | --assets <version> | --deb-arches\n' "$0" >&2
    exit 2
}

# target|os|build_tool|deb|arch
targets() {
    cat <<'TARGETS'
x86_64-unknown-linux-gnu|linux||false|
x86_64-unknown-linux-musl|linux|cross|true|amd64
aarch64-unknown-linux-musl|linux|cross|true|arm64
x86_64-apple-darwin|macos||false|
aarch64-apple-darwin|macos||false|
TARGETS
}

# A JSON array of matrix entries, consumed by `fromJSON` in the workflow.
# Hand-rolled rather than piped through jq: this runs in the plan job before any
# toolchain is provisioned, and the shape is fixed and small.
matrix() {
    printf '['
    first=1
    while IFS='|' read -r target os build_tool deb arch; do
        [ -n "$target" ] || continue
        [ "$first" -eq 1 ] || printf ','
        first=0
        printf '{"target":"%s","os":"%s","build_tool":"%s","deb":%s' \
            "$target" "$os" "$build_tool" "$deb"
        [ -z "$arch" ] || printf ',"arch":"%s"' "$arch"
        printf '}'
    done <<EOF
$(targets)
EOF
    printf ']\n'
}

# Every file a finished release is expected to carry, one per line. The tarball
# and its checksum come from every target; the .deb only from the packaged ones.
#
# Takes the *Debian* version, not the Cargo one, because the two differ for a
# prerelease: 0.5.0-rc.1 is packaged as 0.5.0~rc.1-1 so dpkg sorts it below
# 0.5.0. Callers pass `$(scripts/deb-version.sh)` rather than a bare version, so
# the expected filenames match what cargo-deb actually wrote.
assets() {
    deb_version=$1
    while IFS='|' read -r target os build_tool deb arch; do
        [ -n "$target" ] || continue
        printf 'afi-%s.tar.gz\n' "$target"
        printf 'afi-%s.sha256\n' "$target"
        if [ "$deb" = true ]; then
            printf 'afi_%s_%s.deb\n' "$deb_version" "$arch"
        fi
    done <<EOF
$(targets)
EOF
}

deb_arches() {
    while IFS='|' read -r target os build_tool deb arch; do
        [ "$deb" = true ] || continue
        printf '%s\n' "$arch"
    done <<EOF
$(targets)
EOF
}

[ $# -ge 1 ] || usage

case $1 in
    --matrix) matrix ;;
    --assets)
        [ $# -eq 2 ] || usage
        assets "$2"
        ;;
    --deb-arches) deb_arches ;;
    *) usage ;;
esac
