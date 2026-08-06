#!/bin/sh
set -eu

# Print the Debian package version for the version in Cargo.toml.
#
# SemVer and Debian read the hyphen differently, and the difference decides
# which package apt considers newer. SemVer uses it to introduce a prerelease
# ("0.2.0-rc.1"). Debian uses it to separate the upstream version from the
# packaging revision, and dpkg sorts a hyphenated suffix *after* the bare
# version. Passed through untouched, "0.2.0-rc.1" would outrank "0.2.0" and apt
# would push a release candidate onto stable users. dpkg sorts "~" before an
# empty component, so the prerelease becomes "0.2.0~rc.1" and orders correctly.
#
# SemVer build metadata ("+9a1f7c2") is dropped rather than translated. It has
# no ordering meaning in SemVer and no dpkg equivalent, so carrying it across
# would turn two builds of one source tree into two package versions that apt
# cannot rank against each other.
#
# The trailing revision counts packaging attempts at a single upstream version.
# afi never repackages a released version, so it stays 1 unless
# AFI_DEB_REVISION overrides it.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
revision=${AFI_DEB_REVISION:-1}

version=$("$repo_root/scripts/cargo-version.sh" "$@")

# SemVer 2.0: no leading zeros, hyphens allowed inside a prerelease identifier,
# optional build metadata. A narrower pattern would refuse a version the
# manifest legitimately holds.
if ! printf '%s\n' "$version" | grep -Eq \
    '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
then
    printf '%s is not a semver version\n' "$version" >&2
    exit 1
fi

# Drop build metadata, then turn the prerelease separator into a tilde. Only the
# first hyphen is that separator; later hyphens belong to the prerelease
# identifier and stay put. dpkg splits the revision off at the *last* hyphen, so
# an upstream version may itself contain hyphens as long as a revision follows,
# and one always does here.
upstream=$(printf '%s\n' "${version%%+*}" | sed 's/-/~/')

printf '%s-%s\n' "$upstream" "$revision"
