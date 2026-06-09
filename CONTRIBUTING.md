# Contributing to rekody

Thanks for your interest in contributing! This guide covers everything you need to get started.

## Development Setup

### Prerequisites

- **Rust** — the toolchain is pinned in `rust-toolchain.toml`; [rustup](https://rustup.rs/) picks it up automatically
- A downloaded Whisper GGML model (see the [README](README.md#quick-start), or run `rekody setup`)

### Platform-specific requirements

- **macOS** (primary platform): Xcode Command Line Tools (`xcode-select --install`). Accessibility permission is required for hotkey listening and text injection; Microphone permission is granted to your terminal (see the README).
- Optional, macOS 26+ on Apple Silicon: build the on-device cleanup helper with `make fm-helper`.

### Getting started

```bash
git clone https://github.com/rekody/rekody.git
cd rekody
cargo build -p rekody-core
cargo run --bin rekody          # first run launches the setup wizard
```

## Project Structure

```
rekody/
  Cargo.toml               # Workspace root (version pinned here)
  rust-toolchain.toml      # Pinned Rust toolchain — local == CI
  config/
    default.toml           # Default configuration template
  crates/
    rekody-core/           # Pipeline orchestration, config, CLI, TUIs, skills, prompts
    rekody-audio/          # Mic capture, resampling, VAD
    rekody-stt/            # Speech-to-text (local Whisper + cloud engines)
    rekody-llm/            # LLM cleanup providers (cloud + Apple on-device)
    rekody-inject/         # Text injection (clipboard, native)
    rekody-hotkey/         # Global hotkey listener (CGEventTap)
  helpers/
    rekody-fm/             # Swift helper for Apple Foundation Models (macOS 26+)
  website/                 # rekody.com (Astro, deployed on Vercel)
  scripts/                 # Release/rollback tooling
  models/                  # Local model files (not committed)
```

## Running and Testing

```bash
# Run the CLI in development
cargo run --bin rekody

# Run all workspace tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p rekody-audio

# Build a release binary
cargo build --release -p rekody-core
```

## Code Style

- Run `cargo fmt` before committing. The project uses default rustfmt settings.
- Run `cargo clippy --workspace -- -D warnings` and fix any warnings. Clippy lints are treated as errors in CI (the toolchain pin keeps your local clippy in sync with CI).
- Follow standard Rust naming conventions (`snake_case` for functions/variables, `CamelCase` for types).
- Add doc comments (`///`) to all public items.
- Keep crate boundaries clean — each crate should have a focused responsibility.

## Pull Request Process

1. **Fork** the repository and create a feature branch from `main`.
2. Make your changes in small, focused commits.
3. Add or update tests for any changed behavior.
4. Ensure `cargo fmt`, `cargo clippy --workspace -- -D warnings`, and `cargo test --workspace` all pass.
5. Open a pull request against `main` with a clear description of what and why.
6. A maintainer will review your PR. Address any feedback, then it will be merged.

## Issue Labels

| Label | Description |
|---|---|
| `bug` | Something is broken |
| `enhancement` | New feature or improvement |
| `good first issue` | Suitable for newcomers |
| `help wanted` | Extra attention needed |
| `documentation` | Docs improvements |
| `platform:macos` | macOS-specific |

## Reporting Issues

When filing a bug report, please include:

- Your macOS version and Mac model
- rekody version (`rekody --version`) and install method (Homebrew, installer script, source)
- Steps to reproduce
- Expected vs. actual behavior
- Any relevant log output (run with `rekody -v` for verbose logs)
