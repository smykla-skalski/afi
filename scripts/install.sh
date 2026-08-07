#!/bin/sh
set -eu

# Install afi from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/smykla-skalski/afi/main/scripts/install.sh | sh
#
# For everyone the apt repository does not reach: macOS, and the Linux
# distributions that are not Debian derivatives. Before this, those users had to
# find the releases page, work out which of five archives matched their machine,
# and unpack it by hand.
#
# The checksum is verified always, and it is fetched separately from the archive
# so a truncated or swapped download fails here rather than at first run. The
# checksum alone proves only integrity, because both files come from the same
# place; when the GitHub CLI is present the script also verifies the release's
# build provenance, which proves the archive came from afi's release workflow.
#
# Environment:
#
#   AFI_VERSION   version to install, with or without a leading v (default: latest)
#   AFI_BIN_DIR   where to put the binary (default: ~/.local/bin, or /usr/local/bin
#                 when running as root)
#   AFI_NO_VERIFY set to 1 to skip the provenance check even when gh is available

REPO=smykla-skalski/afi

log() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required and was not found"
}

need uname
need tar
need mktemp

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    resolve_latest_tag() {
        curl -fsSLI -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest" \
            | sed 's#.*/tag/##'
    }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    resolve_latest_tag() {
        # wget prints the redirect chain on stderr; the last Location is the tag.
        wget -qS --spider --max-redirect=5 \
            "https://github.com/$REPO/releases/latest" 2>&1 \
            | sed -n 's#.*[Ll]ocation:.*/tag/##p' \
            | tail -1 \
            | tr -d '\r'
    }
else
    die "neither curl nor wget is available"
fi

# Settled before anything is downloaded, so a machine that cannot verify the
# archive is told so instead of finding out after pulling several megabytes.
if command -v shasum >/dev/null 2>&1; then
    verify_checksum() { (cd "$work" && shasum -a 256 -c "$checksum" >/dev/null); }
elif command -v sha256sum >/dev/null 2>&1; then
    verify_checksum() { (cd "$work" && sha256sum -c "$checksum" >/dev/null); }
else
    die "neither shasum nor sha256sum is available to check the download"
fi

# --- which build ----------------------------------------------------------

os=$(uname -s)
arch=$(uname -m)
[ "$arch" != arm64 ] || arch=aarch64

case "$os:$arch" in
    Darwin:aarch64) target=aarch64-apple-darwin ;;
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    # The musl builds are static and declare no dependencies, so they run on any
    # distribution regardless of its glibc version. That is what makes one
    # archive per architecture enough for all of Linux.
    Linux:aarch64) target=aarch64-unknown-linux-musl ;;
    Linux:x86_64) target=x86_64-unknown-linux-musl ;;
    *) die "no afi build for $os $arch. Build from source: cargo install afi-cli --locked" ;;
esac

version=${AFI_VERSION:-}
if [ -n "$version" ]; then
    tag="v${version#v}"
else
    # /releases/latest redirects to /releases/tag/<tag>, so the tag is in the
    # final URL. Read that rather than calling the API: the API costs one of the
    # 60 unauthenticated requests an hour that everyone behind a shared address
    # shares, and an installer people pipe into sh should not spend those.
    #
    # Drafts and prereleases are not "latest", so this cannot pick up a release
    # that is still being built.
    tag=$(resolve_latest_tag)
    [ -n "$tag" ] || die "could not work out the latest release of $REPO"
fi

archive="afi-$target.tar.gz"
checksum="afi-$target.sha256"
base="https://github.com/$REPO/releases/download/$tag"

# --- download and check ---------------------------------------------------

work=$(mktemp -d "${TMPDIR:-/tmp}/afi-install.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT HUP INT TERM

log "installing afi $tag for $target"

fetch "$base/$archive" "$work/$archive" \
    || die "no $archive in release $tag. See https://github.com/$REPO/releases/tag/$tag"
fetch "$base/$checksum" "$work/$checksum" \
    || die "no checksum for $archive in release $tag"

verify_checksum || die "$archive does not match its published checksum"
log "  checksum ok"

# Provenance, when the tooling is there to check it. Both files above came from
# the same host, so the checksum says the download is intact and nothing about
# where it came from. This says it was built by the release workflow in this
# repository, and is the check worth running on a binary about to be given
# permission to run shell commands.
if [ "${AFI_NO_VERIFY:-}" != 1 ] && command -v gh >/dev/null 2>&1; then
    if gh attestation verify "$work/$archive" --repo "$REPO" >/dev/null 2>&1; then
        log "  build provenance ok"
    else
        # Releases published before provenance was added have none, and a machine
        # with no network path to Sigstore cannot check it either. Neither is
        # evidence of tampering, so say what happened and let the operator judge.
        log "  build provenance NOT verified (no attestation, or gh could not reach Sigstore)"
        log "  to check by hand: gh attestation verify <file> --repo $REPO"
    fi
fi

# --- install --------------------------------------------------------------

tar -xzf "$work/$archive" -C "$work"
[ -x "$work/afi" ] || die "no afi binary in $archive"

if [ -n "${AFI_BIN_DIR:-}" ]; then
    bin_dir=$AFI_BIN_DIR
elif [ "$(id -u)" = 0 ]; then
    bin_dir=/usr/local/bin
else
    bin_dir=$HOME/.local/bin
fi

mkdir -p "$bin_dir"
# Written to a temporary name and moved into place, so an interrupted install
# cannot leave a half-written binary where a working one used to be, and so
# replacing a running afi does not fail with "text file busy".
install_tmp="$bin_dir/.afi.$$"
cp "$work/afi" "$install_tmp"
chmod 755 "$install_tmp"
mv -f "$install_tmp" "$bin_dir/afi"

log "installed $bin_dir/afi"

case ":${PATH}:" in
    *":$bin_dir:"*) ;;
    *)
        log ""
        log "$bin_dir is not on your PATH. Add it:"
        log "  export PATH=\"$bin_dir:\$PATH\""
        ;;
esac

"$bin_dir/afi" --version
