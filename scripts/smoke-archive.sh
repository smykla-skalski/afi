#!/bin/sh
set -eu

# Unpack a release tarball and prove the binary inside it is the one the release
# claims to ship.
#
# The old workflow built five archives, uploaded them, and never opened one. Only
# the two musl binaries were ever executed at all, and then only indirectly,
# because verify-deb.sh runs `afi sessions` inside the installed package. The
# x86_64-gnu tarball was built, compressed, published, and downloaded by users
# without anything having run it once.
#
# `afi --version` is what makes this worth doing. It prints the version on the
# first line and the target triple it was compiled for as a field, so one
# invocation answers both "is this the version being released" and "is this the
# architecture the filename promises". A stale binary left in target/ from an
# earlier build fails the first; a mislabelled archive fails the second.
#
# Arguments: <archive.tar.gz> <target-triple> <expected-version>
#
# Environment:
#
#   AFI_SMOKE_REQUIRE=execute  fail rather than fall back to inspecting the file
#                              header when the binary cannot be run here
#
# A release sets that. Without it the strength of the check would be decided by
# whether the runner image happened to ship Rosetta, and an archive could be
# published having never been executed with nothing but a log line saying so.

if [ $# -ne 3 ]; then
    printf 'usage: %s <archive.tar.gz> <target-triple> <expected-version>\n' "$0" >&2
    exit 2
fi

archive=$1
target=$2
expected_version=$3

[ -f "$archive" ] || { printf 'no such archive: %s\n' "$archive" >&2; exit 1; }

archive_file=$(basename "$archive")

work=$(mktemp -d "${TMPDIR:-/tmp}/afi-smoke.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT HUP INT TERM

tar -xzf "$archive" -C "$work"

# The archive is flat: the binary and the documents sit at the top level, because
# the release workflow builds it with leading-dir off.
binary="$work/afi"
[ -x "$binary" ] || { printf 'no executable afi in %s\n' "$archive_file" >&2; exit 1; }

for doc in README.md LICENSE CHANGELOG.md; do
    [ -f "$work/$doc" ] || { printf 'missing %s in %s\n' "$doc" "$archive_file" >&2; exit 1; }
done

# Which of the three ways of running this binary applies. Decided up front and
# printed, so a log says which assurance the run actually produced rather than
# leaving "it passed" to mean any of them.
target_os=linux
case $target in
    *-apple-darwin) target_os=darwin ;;
esac
target_arch=${target%%-*}

host_os=$(uname -s)
case $host_os in
    Darwin) host_os=darwin ;;
    Linux) host_os=linux ;;
    *) printf 'unsupported host: %s\n' "$host_os" >&2; exit 1 ;;
esac
host_arch=$(uname -m)
[ "$host_arch" != arm64 ] || host_arch=aarch64

mode=inspect
if [ "$target_os" = "$host_os" ] && [ "$target_arch" = "$host_arch" ]; then
    mode=native
elif [ "$target_os" = linux ] && [ "$host_os" = linux ]; then
    # verify-deb.sh already relies on QEMU being registered on the release
    # runner, so the arm64 Linux binary can run on the x86_64 one.
    mode=qemu
elif [ "$target_os" = darwin ] && [ "$host_os" = darwin ]; then
    # An x86_64 binary on an arm64 runner. Rosetta is present on some GitHub
    # macOS images and absent on others, so ask rather than assume: running under
    # it is a real test, and claiming to have run when Rosetta is missing is not.
    if arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
        mode=rosetta
    fi
fi

printf 'smoke-testing %s (%s) in %s mode\n' "$archive_file" "$target" "$mode"

if [ "$mode" = inspect ]; then
    if [ "${AFI_SMOKE_REQUIRE:-}" = execute ]; then
        printf 'cannot execute a %s binary on %s/%s, and AFI_SMOKE_REQUIRE=execute\n' \
            "$target" "$host_os" "$host_arch" >&2
        exit 1
    fi
    # No way to execute it here. Assert the object format and the architecture
    # from the file header instead, which is weaker than running it but is a real
    # check and is reported as such.
    #
    # Both halves, not just the architecture: an arm64 Mach-O and an arm64 ELF
    # are the same machine and different operating systems, so checking the arch
    # alone would let a macOS build pass as the Linux one.
    described=$(file -b "$binary")
    printf '  file: %s\n' "$described"
    case $target_os in
        linux) want_format=ELF ;;
        darwin) want_format=Mach-O ;;
    esac
    case $described in
        *"$want_format"*) ;;
        *)
            printf 'binary is not %s: %s\n' "$want_format" "$described" >&2
            exit 1
            ;;
    esac
    case "$target_arch:$described" in
        x86_64:*x86_64*) ;;
        aarch64:*arm64*) ;;
        aarch64:*aarch64*) ;;
        *)
            printf 'binary is not %s: %s\n' "$target_arch" "$described" >&2
            exit 1
            ;;
    esac
    printf 'INSPECTED (not executed): %s\n' "$archive_file"
    exit 0
fi

# Runs `afi sessions` and then `afi --version`, and prints only the latter.
#
# Both in one invocation rather than two, because under QEMU each one is a
# container start on the release critical path. `afi sessions` is the one
# subcommand that prints and returns without a model endpoint or a terminal, so
# it proves the binary loads and runs rather than merely reporting its own
# metadata; `--version` goes last so stdout carries the block to assert against.
#
# ubuntu:24.04 because verify-deb.sh already pulls it for this architecture, and
# its comment explains why an extra image is worth avoiding: GitHub runners share
# outbound addresses and Docker Hub rate-limits anonymous pulls per address.
run_afi_checks() {
    case $mode in
        native)
            "$binary" sessions >/dev/null
            "$binary" --version
            ;;
        rosetta)
            arch -x86_64 "$binary" sessions >/dev/null
            arch -x86_64 "$binary" --version
            ;;
        qemu)
            platform=linux/amd64
            [ "$target_arch" != aarch64 ] || platform=linux/arm64
            docker run --rm --platform "$platform" \
                -v "$work":/w:ro -e HOME=/tmp -w /tmp \
                ubuntu:24.04 \
                sh -c '/w/afi sessions >/dev/null && /w/afi --version'
            ;;
    esac
}

version_output=$(run_afi_checks)
printf '%s\n' "$version_output" | sed 's/^/  /'

reported_version=$(printf '%s\n' "$version_output" | head -1 | awk '{print $2}')
if [ "$reported_version" != "$expected_version" ]; then
    printf 'binary reports %s, the release is %s\n' \
        "$reported_version" "$expected_version" >&2
    exit 1
fi

reported_target=$(
    printf '%s\n' "$version_output" \
        | awk '$1 == "target:" { print $2; exit }'
)
if [ "$reported_target" != "$target" ]; then
    printf 'binary was built for %s, the archive claims %s\n' \
        "$reported_target" "$target" >&2
    exit 1
fi

printf 'PASSED: %s runs and reports %s for %s\n' \
    "$archive_file" "$reported_version" "$reported_target"
