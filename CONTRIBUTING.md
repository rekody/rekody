# Contributing to rekody

Thanks for your interest in contributing! This guide covers everything you need to get started.

## Development Setup

### Prerequisites

- **Rust** stable toolchain (edition 2024) -- install via [rustup](https://rustup.rs/)
- **Node.js** >= 18 and npm
- **Tauri CLI** -- `cargo install tauri-cli`
- A downloaded Whisper GGML model (see the [README](README.md#quick-start))

### Platform-specific requirements

- **macOS**: Xcode Command Line Tools (`xcode-select --install`). Accessibility permissions are required for hotkey listening and text injection.
- **Linux**: `xdotool`, `xclip`, and standard ALSA/PulseAudio development headers.
- **Windows**: Visual Studio Build Tools with the C++ workload.

### Getting started

```bash
git clone https://github.com/tonykipkemboi/rekody.git
cd rekody
npm install
cargo tauri dev
```

## Project Structure

```
rekody/
  Cargo.toml              # Workspace root
  config/
    default.toml           # Default configuration template
  crates/
    rekody-core/           # Pipeline orchestration, config, context, prompts
    rekody-audio/          # Mic capture, resampling, VAD
    rekody-stt/            # Speech-to-text (local Whisper)
    rekody-llm/            # LLM providers (Cerebras, Groq, local stub)
    rekody-inject/         # Text injection (clipboard, native)
    rekody-hotkey/         # Global hotkey listener
  src-tauri/               # Tauri app shell and commands
  src/                     # React + TypeScript frontend
  models/                  # Local model files (not committed)
```

## Running and Testing

```bash
# Run in development mode (hot-reloads frontend)
cargo tauri dev

# Run all workspace tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p rekody-audio

# Build a production release
cargo tauri build
```

## Code Style

- Run `cargo fmt` before committing. The project uses default rustfmt settings.
- Run `cargo clippy --workspace` and fix any warnings. Clippy lints are treated as errors in CI.
- Follow standard Rust naming conventions (`snake_case` for functions/variables, `CamelCase` for types).
- Add doc comments (`///`) to all public items.
- Keep crate boundaries clean -- each crate should have a focused responsibility.

## Pull Request Process

1. **Fork** the repository and create a feature branch from `main`.
2. Make your changes in small, focused commits.
3. Add or update tests for any changed behavior.
4. Ensure `cargo fmt`, `cargo clippy --workspace`, and `cargo test --workspace` all pass.
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
| `platform:linux` | Linux-specific |
| `platform:windows` | Windows-specific |

## Reporting Issues

When filing a bug report, please include:

- Your OS and version
- Rust toolchain version (`rustc --version`)
- Steps to reproduce
- Expected vs. actual behavior
- Any relevant log output (run with `RUST_LOG=debug cargo tauri dev` for verbose logs)
