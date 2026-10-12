#!/bin/sh
# po installer (Linux x86_64 / macOS Apple Silicon).
#
#   curl -fsSL https://raw.githubusercontent.com/sumofbytes/po/HEAD/scripts/install.sh | sh
#   curl -fsSL ... | sh -s -- 0.1.0                    # pin a version
#   PO_INSTALL_DIR=/usr/local/bin curl -fsSL ... | sh  # custom directory
#
# Env:
#   PO_VERSION       version to install ("latest" or e.g. "0.1.0" / "v0.1.0")
#   PO_INSTALL_DIR   install directory (default: $HOME/.local/bin)

set -eu

REPO="${PO_REPO:-sumofbytes/po}"
INSTALL_DIR="${PO_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${1:-${PO_VERSION:-latest}}"

err() {
    echo "po-install: $1" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || err "curl not found"
command -v tar >/dev/null 2>&1 || err "tar not found"

# --- detect platform -------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os-$arch" in
Linux-x86_64 | Linux-amd64) TARGET="x86_64-unknown-linux-gnu" ;;
Darwin-arm64 | Darwin-aarch64) TARGET="aarch64-apple-darwin" ;;
*) err "no prebuilt binary for $os $arch. Build from source: cargo install --git https://github.com/$REPO" ;;
esac

# --- resolve version -------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
    # Follow the /releases/latest redirect; no API call, so no rate limits.
    VERSION="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||')" ||
        err "cannot determine latest release (none published yet?)"
fi
case "$VERSION" in
v*) ;;
*) VERSION="v$VERSION" ;;
esac

BASE="https://github.com/$REPO/releases/download/$VERSION"
NAME="po-$VERSION-$TARGET"
ASSET="$NAME.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading po $VERSION for $TARGET ..."
curl -fsSL -o "$TMP/$ASSET" "$BASE/$ASSET" ||
    err "download failed ($BASE/$ASSET) — see https://github.com/$REPO/releases"
curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" ||
    err "download failed ($BASE/SHA256SUMS)"

# --- verify checksum -------------------------------------------------------
expected="$(awk -v f="$ASSET" '$2 == f { print $1 }' "$TMP/SHA256SUMS")"
[ -n "$expected" ] || err "no checksum entry for $ASSET"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$TMP/$ASSET" | awk '{print $1}')"
else
    actual="$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')"
fi
[ "$actual" = "$expected" ] ||
    err "checksum mismatch for $ASSET (got $actual, want $expected)"

# --- install ---------------------------------------------------------------
tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/$NAME/po" ] || err "archive did not contain a po binary"

mkdir -p "$INSTALL_DIR"
mv "$TMP/$NAME/po" "$INSTALL_DIR/po"
chmod 755 "$INSTALL_DIR/po"

case ":$PATH:" in
*":$INSTALL_DIR:"*) ;;
*)
    echo "note: $INSTALL_DIR is not in your PATH."
    echo "      add:  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac

echo "Installed po $VERSION to $INSTALL_DIR/po"
echo
echo "Next: install an ACP agent (e.g. opencode), then run \`po\` in a project."
