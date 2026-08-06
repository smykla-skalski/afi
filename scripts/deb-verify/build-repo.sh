#!/bin/sh
set -eu

# Assemble and sign a one-package apt repository. Runs inside a throwaway
# Debian container; see ../verify-deb.sh for why this is not done in the same
# container that installs the package.
#
# Inputs come from the environment so the caller does not have to quote a
# command line through docker: ARCH, SUITE, DEB_FILE.
#
# The signing key is generated here and dies with the container. It exists to
# exercise apt's verification path, not to stand in for the release key.

: "${ARCH:?ARCH is required}"
: "${SUITE:?SUITE is required}"
: "${DEB_FILE:?DEB_FILE is required}"

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends apt-utils gnupg >/dev/null

gpg --batch --quiet --passphrase '' \
    --quick-generate-key 'afi deb verify <afi@example.invalid>' default default never

repo=/out/repo
mkdir -p "$repo/pool/main/a/afi" "$repo/dists/$SUITE/main/binary-$ARCH"
cp "/debs/$DEB_FILE" "$repo/pool/main/a/afi/"

cd "$repo"

# Filename: fields in Packages are relative to the repository root, so this has
# to run from the root with a relative pool path.
apt-ftparchive --arch "$ARCH" packages pool \
    > "dists/$SUITE/main/binary-$ARCH/Packages"
gzip -9kf "dists/$SUITE/main/binary-$ARCH/Packages"

apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=afi \
    -o APT::FTPArchive::Release::Label=afi \
    -o APT::FTPArchive::Release::Suite="$SUITE" \
    -o APT::FTPArchive::Release::Codename="$SUITE" \
    -o APT::FTPArchive::Release::Architectures="$ARCH" \
    -o APT::FTPArchive::Release::Components=main \
    release "dists/$SUITE" > "dists/$SUITE/Release"

# InRelease is what modern apt fetches. Release.gpg is written too, for clients
# that still ask for the detached signature.
gpg --batch --yes --clearsign -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"
gpg --batch --yes --armor --detach-sign -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"

# A binary export is already in the form Signed-By wants, so the install side
# needs no dearmor step.
gpg --batch --export > /out/keyring.gpg

test -s "dists/$SUITE/InRelease"
test -s "dists/$SUITE/main/binary-$ARCH/Packages"
test -s /out/keyring.gpg
