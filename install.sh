#!/usr/bin/env bash
# Chamgei — One-line installer (downloads precompiled binary, no Rust needed)
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/tonykipkemboi/chamgei/main/install.sh | bash
#
# What it does:
#   1. Downloads the precompiled binary for your platform
#   2. Installs to /usr/local/bin/
#   3. Downloads the tiny Whisper model (~75 MB)
#   4. First run walks you through setup

set -euo pipefail

GITHUB_REPO="tonykipkemboi/chamgei"
INSTALL_DIR="/usr/local/bin"
MODEL_DIR="$HOME/.local/share/chamgei/models"
WHISPER_FILE="ggml-tiny.bin"
WHISPER_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$WHISPER_FILE"

# Version resolution (in priority order):
#   1. Positional arg:  curl -fsSL .../install.sh | bash -s v0.4.0
#   2. Env var:         VERSION=v0.4.0 curl ... | bash
#   3. Latest release from GitHub API (default)
if [ -n "${1:-}" ]; then
  VERSION="${1}"
elif [ -n "${VERSION:-}" ]; then
  VERSION="${VERSION}"
else
  VERSION=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
fi

if [ -z "$VERSION" ]; then
  echo "  ERROR: Could not determine latest release. Check your internet connection."
  exit 1
fi

echo ""
echo "  ╔══════════════════════════════════════╗"
echo "  ║   Chamgei Installer                  ║"
echo "  ║   Privacy-first voice dictation      ║"
echo "  ╚══════════════════════════════════════╝"
echo ""

# --- Detect platform ---
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin) PLATFORM="macos" ;;
    Linux)  PLATFORM="linux" ;;
    *)      echo "  ERROR: Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    arm64|aarch64) ARCH_NAME="arm64" ;;
    x86_64)        ARCH_NAME="x86_64" ;;
    *)             echo "  ERROR: Unsupported architecture: $ARCH"; exit 1 ;;
esac

TARBALL="chamgei-${VERSION}-${PLATFORM}-${ARCH_NAME}.tar.gz"
DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${TARBALL}"

echo "  Platform:  $PLATFORM ($ARCH_NAME)"
echo ""

# --- Download binary ---
echo "  [1/3] Downloading chamgei ${VERSION}..."

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

SUMS_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/SHA256SUMS"

if ! curl -fSL --progress-bar -o "$TMPDIR/$TARBALL" "$DOWNLOAD_URL" 2>&1; then
    echo ""
    echo "  Download failed. Falling back to building from source..."
    echo ""

    # Fallback: build from source if binary not available
    if ! command -v cargo &>/dev/null; then
        echo "  ERROR: No precompiled binary for $PLATFORM-$ARCH_NAME and Rust is not installed."
        echo ""
        echo "  Option 1: Install Rust first:"
        echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        echo ""
        echo "  Option 2: Download the .dmg from:"
        echo "    https://github.com/${GITHUB_REPO}/releases"
        exit 1
    fi

    echo "  Building from source (this takes 1-2 minutes)..."
    BUILD_DIR="$HOME/.chamgei-build"
    # Try release tag first; fall back to main so help/CLI always works
    git clone --depth 1 --branch "$VERSION" "https://github.com/${GITHUB_REPO}.git" "$BUILD_DIR" 2>/dev/null \
      || git clone --depth 1 "https://github.com/${GITHUB_REPO}.git" "$BUILD_DIR"
    cd "$BUILD_DIR"
    cargo build --release -p chamgei-core 2>&1 | tail -1
    cp target/release/chamgei "$TMPDIR/chamgei"
    cd -
else
    # Verify tarball checksum against the SHA256SUMS published with the release
    if curl -fsSL -o "$TMPDIR/SHA256SUMS" "$SUMS_URL" 2>/dev/null; then
        EXPECTED=$(grep "$TARBALL" "$TMPDIR/SHA256SUMS" | awk '{print $1}')
        if [ -n "$EXPECTED" ]; then
            if command -v shasum &>/dev/null; then
                ACTUAL=$(shasum -a 256 "$TMPDIR/$TARBALL" | awk '{print $1}')
            elif command -v sha256sum &>/dev/null; then
                ACTUAL=$(sha256sum "$TMPDIR/$TARBALL" | awk '{print $1}')
            else
                ACTUAL=""
            fi
            if [ -n "$ACTUAL" ] && [ "$ACTUAL" != "$EXPECTED" ]; then
                echo "  ERROR: Binary checksum mismatch — aborting for safety."
                echo "         Expected: $EXPECTED"
                echo "         Got:      $ACTUAL"
                exit 1
            fi
        fi
    fi

    # Extract binary from tarball
    tar -xzf "$TMPDIR/$TARBALL" -C "$TMPDIR"
fi

# --- Install binary ---
echo "  [2/3] Installing to $INSTALL_DIR..."

if [ -w "$INSTALL_DIR" ]; then
    cp "$TMPDIR/chamgei" "$INSTALL_DIR/chamgei"
else
    sudo cp "$TMPDIR/chamgei" "$INSTALL_DIR/chamgei"
fi
chmod +x "$INSTALL_DIR/chamgei"

# --- Download Whisper model ---
echo "  [3/3] Downloading Whisper model (tiny, ~75 MB)..."

mkdir -p "$MODEL_DIR"

if [ -f "$MODEL_DIR/$WHISPER_FILE" ]; then
    echo "         Already present at $MODEL_DIR/$WHISPER_FILE"
else
    curl -fSL --progress-bar -o "$MODEL_DIR/$WHISPER_FILE" "$WHISPER_URL"

    # Verify checksum
    if command -v shasum &>/dev/null; then
        ACTUAL=$(shasum -a 256 "$MODEL_DIR/$WHISPER_FILE" | awk '{print $1}')
    elif command -v sha256sum &>/dev/null; then
        ACTUAL=$(sha256sum "$MODEL_DIR/$WHISPER_FILE" | awk '{print $1}')
    else
        ACTUAL=""
    fi

    # Checksum verification is skipped — ggml-tiny.bin is fetched directly
    # from the official HuggingFace repo and its hash isn't pinned here.
    if [ -z "$ACTUAL" ]; then
        echo "  WARNING: Could not verify model checksum (shasum/sha256sum not found)"
    else
        echo "  Model checksum: $ACTUAL"
    fi
fi

# --- Done ---
echo ""
echo "  ✓ Chamgei installed successfully!"
echo ""
echo "  Run 'chamgei' to start."
echo "  First launch will walk you through setup."
echo ""
echo "  To uninstall:"
echo "    rm $INSTALL_DIR/chamgei"
echo "    rm -rf ~/.config/chamgei ~/.local/share/chamgei"
echo ""
