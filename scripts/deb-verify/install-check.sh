#!/bin/sh
set -eu

# Install afi from the apt repository served by verify-deb.sh and assert the
# result. Runs inside a clean distribution image with no build tooling.
#
# Inputs from the environment: SUITE, VERSION, ARCH, SERVER.

: "${SUITE:?SUITE is required}"
: "${VERSION:?VERSION is required}"
: "${ARCH:?ARCH is required}"
: "${SERVER:?SERVER is required}"

export DEBIAN_FRONTEND=noninteractive

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

# Drop the distribution mirrors. Anything that installs from here on has to come
# out of the afi repository alone, which turns "afi depends on nothing" from an
# assumption into an assertion.
rm -f /etc/apt/sources.list
rm -f /etc/apt/sources.list.d/*

# Official Debian and Ubuntu container images ship a dpkg config that discards
# /usr/share/doc and /usr/share/man to stay small. A real machine has no such
# config, and leaving it in place would make the documentation assertions below
# report on the base image instead of on the package.
rm -f /etc/dpkg/dpkg.cfg.d/excludes /etc/dpkg/dpkg.cfg.d/docker

install -d -m 0755 /etc/apt/keyrings
install -m 0644 /tmp/afi-keyring.gpg /etc/apt/keyrings/afi.gpg

cat > /etc/apt/sources.list.d/afi.sources <<SOURCES
Types: deb
URIs: http://$SERVER:8000
Suites: $SUITE
Components: main
Architectures: $ARCH
Signed-By: /etc/apt/keyrings/afi.gpg
SOURCES

echo '--- apt-get update ---'
apt-get update 2>&1 | tee /tmp/update.log

# apt can exit 0 while still refusing to trust a repository, so the log decides
# rather than the exit code.
if grep -Eqi 'NO_PUBKEY|is not signed|GPG error|following signatures' /tmp/update.log; then
    fail 'apt did not verify the repository signature'
fi

echo '--- apt-cache policy ---'
apt-cache policy afi
apt-cache policy afi | grep -qF "$VERSION" || fail "$VERSION not offered by the repository"

echo '--- apt-get install ---'
apt-get install -y afi

echo '--- assertions ---'
[ "$(command -v afi)" = /usr/bin/afi ] || fail 'afi is not at /usr/bin/afi'

installed_version=$(dpkg-query -W -f '${Version}' afi)
[ "$installed_version" = "$VERSION" ] || fail "installed version is $installed_version, want $VERSION"

installed_arch=$(dpkg-query -W -f '${Architecture}' afi)
[ "$installed_arch" = "$ARCH" ] || fail "installed architecture is $installed_arch, want $ARCH"

# An empty Depends is intended: the binary is static. Anything else means the
# packaging picked up a dependency it should not have.
installed_depends=$(dpkg-query -W -f '${Depends}' afi)
[ -z "$installed_depends" ] || fail "unexpected Depends: $installed_depends"

for doc in copyright README.md CHANGELOG.md reference.md sources.example.env; do
    [ -f "/usr/share/doc/afi/$doc" ] || fail "missing /usr/share/doc/afi/$doc"
done

# `afi sessions` is the one subcommand that prints and returns without a model
# endpoint or a terminal, so it proves the binary loads and runs rather than
# merely unpacking.
echo '--- afi sessions ---'
HOME=/root afi sessions

echo '--- apt-get purge ---'
apt-get purge -y afi >/dev/null
[ ! -e /usr/bin/afi ] || fail 'purge left /usr/bin/afi behind'

echo 'ALL CHECKS PASSED'
