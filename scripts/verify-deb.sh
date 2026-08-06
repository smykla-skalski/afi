#!/bin/sh
set -eu

# Prove a .deb installs the way a user will install it: pulled from an apt
# repository over HTTP, with a signature apt actually verifies.
#
# Three containers rather than one. deb-verify/build-repo.sh assembles and signs
# the repository in a container that needs apt-utils and gpg, a second serves it,
# and deb-verify/install-check.sh installs from it in a third that starts clean.
# Doing all of it in one container would let a build-time package satisfy a
# runtime one, so a missing dependency would still pass.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

if [ $# -lt 1 ]; then
    printf 'usage: %s <path-to-deb> [install-image]\n' "$0" >&2
    exit 2
fi

deb_path=$1
install_image=${2:-ubuntu:24.04}
suite=stable

# Three images, reused for every helper role. GitHub-hosted runners share
# outbound addresses and Docker Hub rate-limits anonymous pulls per address, so
# each extra image is a way for a release to fail for reasons of its own.
# debian:stable-slim reads the package and builds the repository, this one serves
# it and does the odd jobs, and the package installs into $install_image.
build_image=debian:stable-slim
helper_image=python:3-alpine

[ -f "$deb_path" ] || { printf 'no such file: %s\n' "$deb_path" >&2; exit 1; }

deb_dir=$(CDPATH='' cd -- "$(dirname -- "$deb_path")" && pwd)
deb_file=$(basename "$deb_path")

work=$(mktemp -d "${TMPDIR:-/tmp}/afi-deb-verify.XXXXXX")
net="afi-deb-verify-$$"
server="afi-deb-server-$$"

cleanup() {
    docker rm -f "$server" >/dev/null 2>&1 || true
    docker network rm "$net" >/dev/null 2>&1 || true
    # The repository is written by a root process inside a container, so the host
    # user cannot always unlink it. Borrow a container to do the removal.
    docker run --rm -v "$work":/w "$helper_image" \
        rm -rf /w/repo /w/keyring.gpg >/dev/null 2>&1 || true
    rm -rf "$work" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

# dpkg-deb is the authority on what the package claims to be, and the host is
# not required to have it.
read_field() {
    docker run --rm -v "$deb_dir":/debs:ro "$build_image" \
        dpkg-deb -f "/debs/$deb_file" "$1" | tr -d '\r\n'
}

arch=$(read_field Architecture)
version=$(read_field Version)
package=$(read_field Package)

case $arch in
    amd64) platform=linux/amd64 ;;
    arm64) platform=linux/arm64 ;;
    *) printf 'unsupported architecture: %s\n' "$arch" >&2; exit 1 ;;
esac

printf 'verifying %s %s (%s) against %s\n' "$package" "$version" "$arch" "$install_image"

docker run --rm \
    -v "$deb_dir":/debs:ro \
    -v "$work":/out \
    -v "$repo_root/scripts/deb-verify":/steps:ro \
    -e ARCH="$arch" -e SUITE="$suite" -e DEB_FILE="$deb_file" \
    "$build_image" /steps/build-repo.sh

docker network create "$net" >/dev/null
docker run -d --rm --name "$server" --network "$net" \
    -v "$work/repo":/repo:ro \
    "$helper_image" python3 -m http.server 8000 --directory /repo >/dev/null

# Wait for the server instead of racing it, and confirm in passing that the
# signed index is reachable over HTTP.
if ! docker run --rm --network "$net" \
    -e URL="http://$server:8000/dists/$suite/InRelease" "$helper_image" \
    sh -c 'i=0; while [ "$i" -lt 30 ]; do wget -q -O /dev/null "$URL" && exit 0; i=$((i + 1)); sleep 1; done; exit 1'
then
    printf 'repository server never served dists/%s/InRelease\n' "$suite" >&2
    exit 1
fi

docker run --rm --network "$net" --platform "$platform" \
    -v "$work/keyring.gpg":/tmp/afi-keyring.gpg:ro \
    -v "$repo_root/scripts/deb-verify":/steps:ro \
    -e SUITE="$suite" -e VERSION="$version" -e ARCH="$arch" -e SERVER="$server" \
    "$install_image" /steps/install-check.sh
