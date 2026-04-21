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

.PHONY: build install uninstall package-macos clean

build:
	cargo build --release -p rekody-core

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
