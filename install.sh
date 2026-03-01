#!/bin/sh
# Midtown installer — downloads the latest release binary for your platform.
# Usage: curl -fsSL https://raw.githubusercontent.com/btucker/midtown/main/install.sh | sh

set -e

REPO="btucker/midtown"
INSTALL_DIR="${MIDTOWN_INSTALL_DIR:-$HOME/.cargo/bin}"

# ── Detect platform ──────────────────────────────────────────────────────────

detect_os() {
    case "$(uname -s)" in
        Darwin) echo "darwin" ;;
        Linux)  echo "linux" ;;
        *)
            echo "Error: unsupported OS: $(uname -s)" >&2
            exit 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)   echo "amd64" ;;
        aarch64|arm64)  echo "arm64" ;;
        *)
            echo "Error: unsupported architecture: $(uname -m)" >&2
            exit 1
            ;;
    esac
}

OS="$(detect_os)"
ARCH="$(detect_arch)"

# ── Resolve latest version ───────────────────────────────────────────────────

echo "Detecting latest midtown release..."

# Use the redirect from /releases/latest to extract the version tag.
# This avoids the GitHub API and its 60 req/hour unauthenticated rate limit.
VERSION="$(curl -fsSI "https://github.com/${REPO}/releases/latest" \
    | grep -i '^location:' \
    | sed 's|.*/tag/||;s/[[:space:]]*$//')"

if [ -z "$VERSION" ]; then
    echo "Error: could not determine latest release version" >&2
    exit 1
fi

echo "Latest version: ${VERSION}"

# ── Download and install ─────────────────────────────────────────────────────

# Normalize version: strip leading "v" to get the bare version number.
# Asset naming must match .github/workflows/publish.yml which produces
# midtown-<os>-<arch>-v<bare_version>.tar.gz
BARE_VERSION="${VERSION#v}"
ASSET="midtown-${OS}-${ARCH}-v${BARE_VERSION}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"

echo "Downloading ${ASSET}..."

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "$URL" -o "${TMP_DIR}/${ASSET}"
tar xzf "${TMP_DIR}/${ASSET}" -C "$TMP_DIR"

mkdir -p "$INSTALL_DIR"
mv "${TMP_DIR}/midtown" "${INSTALL_DIR}/midtown"
chmod +x "${INSTALL_DIR}/midtown"

# Install bundled web-app if present in the tarball
if [ -d "${TMP_DIR}/web-app" ]; then
    rm -rf "${INSTALL_DIR}/web-app"
    mv "${TMP_DIR}/web-app" "${INSTALL_DIR}/web-app"
    echo "Installed web UI to ${INSTALL_DIR}/web-app/"
fi

echo ""
echo "Installed midtown ${VERSION} to ${INSTALL_DIR}/midtown"

# ── Verify PATH ──────────────────────────────────────────────────────────────

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    echo "Note: ${INSTALL_DIR} is not in your PATH."
    echo "Add it with:"
    echo ""
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
fi

echo "Run 'midtown --help' to get started."
