#!/usr/bin/env bash
# Chamgei — One-line installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/tonykipkemboi/chamgei/main/install.sh | bash
#
# What it does:
#   1. Checks for Rust toolchain (suggests rustup if missing)
#   2. Clones the repo (or pulls if already present)
#   3. Builds in release mode
#   4. Copies binary to /usr/local/bin/
#   5. Downloads the tiny Whisper model
#   6. Prints success message

set -euo pipefail

REPO_URL="https://github.com/tonykipkemboi/chamgei.git"
INSTALL_DIR="/usr/local/bin"
MODEL_DIR="$HOME/.local/share/chamgei/models"
WHISPER_FILE="ggml-tiny.en.bin"
WHISPER_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$WHISPER_FILE"
BUILD_DIR="$HOME/.chamgei-build"

# Known SHA-256 checksum for the tiny.en model (update if model changes upstream).
# Verify at: https://huggingface.co/ggerganov/whisper.cpp/tree/main
WHISPER_SHA256="921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"

echo ""
echo "======================================"
echo "  Chamgei Installer"
echo "  Voice dictation for macOS"
echo "======================================"
echo ""

# --- Check for Rust toolchain ---
if ! command -v cargo &>/dev/null; then
    echo "ERROR: Rust toolchain not found."
    echo ""
    echo "Install Rust first with:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo ""
    echo "Then re-run this installer."
    exit 1
fi

echo "[1/5] Rust toolchain found: $(rustc --version)"

# --- Clone or update repo ---
echo "[2/5] Getting source code..."

if [ -d "$BUILD_DIR" ]; then
    echo "  Updating existing clone at $BUILD_DIR"
    cd "$BUILD_DIR"
    git pull --ff-only || {
        echo "  Pull failed; removing and re-cloning..."
        cd /
        rm -rf "$BUILD_DIR"
        git clone --depth 1 "$REPO_URL" "$BUILD_DIR"
        cd "$BUILD_DIR"
    }
else
    git clone --depth 1 "$REPO_URL" "$BUILD_DIR"
    cd "$BUILD_DIR"
fi

# --- Build ---
echo "[3/5] Building release binary (this may take a few minutes)..."
cargo build --release -p chamgei-core

# --- Install binary ---
echo "[4/5] Installing binary to $INSTALL_DIR..."

if [ -w "$INSTALL_DIR" ]; then
    cp target/release/chamgei "$INSTALL_DIR/chamgei"
else
    echo "  (requires sudo for $INSTALL_DIR)"
    sudo cp target/release/chamgei "$INSTALL_DIR/chamgei"
fi
chmod +x "$INSTALL_DIR/chamgei"

# --- Download Whisper model ---
echo "[5/5] Downloading Whisper model (tiny, ~75 MB)..."

mkdir -p "$MODEL_DIR"

if [ -f "$MODEL_DIR/$WHISPER_FILE" ]; then
    echo "  Model already present at $MODEL_DIR/$WHISPER_FILE"
else
    curl -fSL --progress-bar -o "$MODEL_DIR/$WHISPER_FILE" "$WHISPER_URL"
    echo "  Downloaded to $MODEL_DIR/$WHISPER_FILE"

    # Verify SHA-256 checksum.
    if command -v shasum &>/dev/null; then
        ACTUAL_HASH=$(shasum -a 256 "$MODEL_DIR/$WHISPER_FILE" | awk '{print $1}')
    elif command -v sha256sum &>/dev/null; then
        ACTUAL_HASH=$(sha256sum "$MODEL_DIR/$WHISPER_FILE" | awk '{print $1}')
    else
        ACTUAL_HASH=""
        echo "  WARNING: Neither shasum nor sha256sum found — skipping checksum verification."
    fi

    if [ -n "$ACTUAL_HASH" ]; then
        if [ "$ACTUAL_HASH" = "$WHISPER_SHA256" ]; then
            echo "  Checksum verified (SHA-256 matches)."
        else
            echo "  WARNING: SHA-256 checksum mismatch!"
            echo "    Expected: $WHISPER_SHA256"
            echo "    Actual:   $ACTUAL_HASH"
            echo "    The model file may have been updated upstream."
            echo "    If you trust the source, you can ignore this warning."
        fi
    fi
fi

# --- Done ---
echo ""
echo "======================================"
echo "  Chamgei installed successfully!"
echo "======================================"
echo ""
echo "  Binary:  $INSTALL_DIR/chamgei"
echo "  Model:   $MODEL_DIR/$WHISPER_FILE"
echo ""
echo "  Run 'chamgei' to start."
echo "  On first launch, it will walk you through setup"
echo "  (LLM provider, API key, permissions)."
echo ""
echo "  To uninstall:"
echo "    rm $INSTALL_DIR/chamgei"
echo "    rm -rf ~/.config/chamgei"
echo "    rm -rf ~/.local/share/chamgei"
echo "    rm -rf $BUILD_DIR"
echo ""
