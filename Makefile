# rekody — Build & Packaging
# Usage:
#   make build          Build release binary
#   make install        Build + install binary + download default model
#   make uninstall      Remove binary and config
#   make package-macos  Create a distributable .tar.gz
#   make clean          Cargo clean

BINARY_NAME  := rekody
INSTALL_DIR  := /usr/local/bin
MODEL_DIR    := $(HOME)/.local/share/rekody/models
CONFIG_DIR   := $(HOME)/.config/rekody
WHISPER_FILE := ggml-tiny.bin
WHISPER_URL  := https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$(WHISPER_FILE)

# Detect architecture for the package name
ARCH := $(shell uname -m)
VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')

HELPER_BIN_DIR := $(HOME)/.local/share/rekody/bin

.PHONY: build install uninstall package-macos clean fm-helper

build:
	cargo build --release -p rekody-core

# Build + install the Apple Foundation Models helper (macOS 26+, Apple Silicon).
# rekody discovers it at $(HELPER_BIN_DIR)/rekody-fm and uses it for on-device,
# zero-download LLM cleanup. Requires the Swift 6 toolchain (Xcode CLT) on a
# macOS 26 SDK. Safe no-op message on unsupported setups.
fm-helper:
	@echo "Building rekody-fm (Apple Foundation Models helper)..."
	@cd helpers/rekody-fm && swift build -c release
	@mkdir -p $(HELPER_BIN_DIR)
	@cp helpers/rekody-fm/.build/release/rekody-fm $(HELPER_BIN_DIR)/rekody-fm
	@chmod +x $(HELPER_BIN_DIR)/rekody-fm
	@echo "Installed: $(HELPER_BIN_DIR)/rekody-fm"
	@$(HELPER_BIN_DIR)/rekody-fm --check && \
		echo "Apple Foundation Models: available — set LLM provider to 'apple' (rekody setup or config)." || \
		echo "Helper installed, but Apple Intelligence is not available yet (enable it in System Settings)."

install: build
	@echo "Installing $(BINARY_NAME) to $(INSTALL_DIR)..."
	@sudo cp target/release/$(BINARY_NAME) $(INSTALL_DIR)/$(BINARY_NAME)
	@sudo chmod +x $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Ensuring model directory exists..."
	@mkdir -p $(MODEL_DIR)
	@if [ ! -f "$(MODEL_DIR)/$(WHISPER_FILE)" ]; then \
		echo "Downloading default Whisper model (tiny)..."; \
		curl -fSL --progress-bar -o "$(MODEL_DIR)/$(WHISPER_FILE)" "$(WHISPER_URL)"; \
	else \
		echo "Model already present at $(MODEL_DIR)/$(WHISPER_FILE)"; \
	fi
	@echo ""
	@echo "$(BINARY_NAME) installed successfully."
	@echo "  Binary:  $(INSTALL_DIR)/$(BINARY_NAME)"
	@echo "  Model:   $(MODEL_DIR)/$(WHISPER_FILE)"
	@echo ""
	@echo "Run 'rekody' to start. On first launch it will guide you through setup."

uninstall:
	@echo "Removing $(BINARY_NAME)..."
	@sudo rm -f $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Removing config directory $(CONFIG_DIR)..."
	@rm -rf $(CONFIG_DIR)
	@echo "Removing model directory $(MODEL_DIR)..."
	@rm -rf $(MODEL_DIR)
	@echo "Uninstall complete."

package-macos: build
	@echo "Packaging for macOS ($(ARCH))..."
	@mkdir -p dist
	@PKGDIR=$$(mktemp -d) && \
	cp target/release/$(BINARY_NAME) "$$PKGDIR/$(BINARY_NAME)" && \
	mkdir -p "$$PKGDIR/models" && \
	if [ -f "$(MODEL_DIR)/$(WHISPER_FILE)" ]; then \
		cp "$(MODEL_DIR)/$(WHISPER_FILE)" "$$PKGDIR/models/$(WHISPER_FILE)"; \
	else \
		echo "Downloading model for package..."; \
		curl -fSL --progress-bar -o "$$PKGDIR/models/$(WHISPER_FILE)" "$(WHISPER_URL)"; \
	fi && \
	cp config/default.toml "$$PKGDIR/config.toml" && \
	tar -czf "dist/rekody-$(VERSION)-macos-$(ARCH).tar.gz" -C "$$PKGDIR" . && \
	rm -rf "$$PKGDIR" && \
	echo "Package created: dist/rekody-$(VERSION)-macos-$(ARCH).tar.gz"

clean:
	cargo clean
