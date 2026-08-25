#!/bin/sh
#
# Install spr from a release of this fork.
#
#   curl -fsSL https://raw.githubusercontent.com/HongboDu-at/spr/main/install.sh | sh
#
# The environment controls the details:
#
#   SPR_VERSION      The version to install, for example 1.3.4-enhanced.1.
#                    The default is the newest release.
#   SPR_INSTALL_DIR  The directory to install into.
#                    The default is $HOME/.local/bin.

set -eu

REPO=HongboDu-at/spr
INSTALL_DIR=${SPR_INSTALL_DIR:-$HOME/.local/bin}

die() {
    echo "install.sh: $*" >&2
    exit 1
}

# --- Which binary does this machine need? ---------------------------------

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) die "there is no spr binary for $(uname -s)" ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=arm64 ;;
    *) die "there is no spr binary for $(uname -m)" ;;
esac

archive="spr-$os-$arch.tar.gz"

# An archive holds no version in its name, so the newest release needs no call
# to the API. That also keeps the script away from the limit on the number of
# requests, which a shared address can reach.
if [ -n "${SPR_VERSION:-}" ]; then
    url="https://github.com/$REPO/releases/download/v$SPR_VERSION/$archive"
else
    url="https://github.com/$REPO/releases/latest/download/$archive"
fi

echo "Installing spr for $os-$arch"

# --- Download and check ---------------------------------------------------

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

curl -fsSL "$url" -o "$work/$archive" ||
    die "could not download $url"
curl -fsSL "$url.sha256" -o "$work/$archive.sha256" ||
    die "could not download the checksum of $archive"

# The release makes each checksum with `shasum`, which is on macOS and on most
# Linux systems. `sha256sum` reads the same form.
if command -v shasum >/dev/null 2>&1; then
    check="shasum -a 256 -c"
else
    check="sha256sum -c"
fi

(cd "$work" && $check "$archive.sha256" >/dev/null) ||
    die "the checksum of $archive is wrong"

# --- Install --------------------------------------------------------------

tar xzf "$work/$archive" -C "$work"
[ -f "$work/spr" ] || die "$archive holds no spr binary"

mkdir -p "$INSTALL_DIR"
mv "$work/spr" "$INSTALL_DIR/spr"

echo "Installed $INSTALL_DIR/spr"
"$INSTALL_DIR/spr" --version

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Note: $INSTALL_DIR is not in your PATH." >&2 ;;
esac
