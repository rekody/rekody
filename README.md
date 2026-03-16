# Chamgei

**An open-source, privacy-first voice dictation system.**

Chamgei turns your voice into text anywhere on your desktop. It runs a local Whisper model for speech-to-text, optionally polishes the output through fast cloud LLMs, and injects the result at your cursor — all behind a single hotkey.

## Features

- **Local speech-to-text** — On-device Whisper inference via whisper.cpp (Metal GPU on macOS).
- **LLM formatting** — Optional post-processing to clean filler words, add punctuation, and fix grammar.
- **11 LLM providers** — Groq, Cerebras, Together, OpenRouter, Fireworks, OpenAI, Anthropic, Gemini, Ollama, LM Studio, vLLM, or any OpenAI-compatible endpoint.
- **Context-aware** — Detects the active application and adapts formatting (code editors, chat apps, email, etc.).
- **Command mode** — Select text, speak an instruction, and have an LLM transform it in place.
- **Cross-platform** — macOS, Windows, and Linux with platform-native text injection.
- **Offline mode** — Works fully offline with local Whisper. LLM formatting is optional.
- **Privacy-first** — Audio never leaves your machine. LLM calls send only the transcript text.

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)

### Install & Run

```bash
# 1. Clone the repository
git clone https://github.com/tonykipkemboi/chamgei.git
cd chamgei

# 2. Build the CLI
cargo build -p chamgei-core --bin chamgei --release

# 3. Run it (first run starts the setup wizard)
./target/release/chamgei
```

The **first-run wizard** will walk you through:
1. Choosing an LLM provider (Groq, OpenAI, Anthropic, Ollama, etc.)
2. Entering your API key
3. Selecting and downloading a Whisper model
4. Checking macOS permissions

### One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/tonykipkemboi/chamgei/main/install.sh | bash
```

Or use Make:

```bash
make install
```

## Usage

### Hotkeys

| Action | Shortcut |
|--------|----------|
| **Push-to-talk** (hold to record, release to transcribe) | `Fn` |
| **Hands-free toggle** (press to start, press to stop) | `Fn + Space` |
| **Command mode** (transform selected text by voice) | `Fn + Enter` |

> **macOS note:** Set **System Settings > Keyboard > "Press 🌐 key to"** to **"Do Nothing"** so the Fn key isn't intercepted.

### Configuration

Config lives at `~/.config/chamgei/config.toml`. The setup wizard creates it for you, or copy the default:

```bash
mkdir -p ~/.config/chamgei
cp config/default.toml ~/.config/chamgei/config.toml
```

#### Provider config (new format)

```toml
activation_mode = "push_to_talk"
whisper_model = "tiny"
vad_threshold = 0.01
injection_method = "clipboard"

# Providers tried in order — first success wins
[[providers]]
name = "groq"
api_key = "gsk_..."
model = "openai/gpt-oss-20b"

# Add fallback providers
[[providers]]
name = "ollama"
model = "llama3.2:3b"
```

#### Supported providers

| Provider | Type | Auth | Default model |
|----------|------|------|---------------|
| `groq` | Cloud | API key | `openai/gpt-oss-20b` |
| `cerebras` | Cloud | API key | `llama3.1-8b` |
| `together` | Cloud | API key | `Meta-Llama-3.1-8B-Instruct-Turbo` |
| `openrouter` | Cloud | API key | `llama-3.1-8b-instruct:free` |
| `fireworks` | Cloud | API key | User's choice |
| `openai` | Cloud | API key | `gpt-4o-mini` |
| `anthropic` | Cloud | API key | `claude-sonnet-4-20250514` |
| `gemini` | Cloud | API key | `gemini-2.0-flash` |
| `ollama` | Local | None | `llama3.2:3b` |
| `lm-studio` | Local | None | Loaded model |
| `vllm` | Local | None | User's choice |
| Custom | Any | Optional | User's choice |

Whisper model is resolved from: `$CHAMGEI_MODEL_DIR` → `~/.local/share/chamgei/models/` → `./models/`

## Architecture

```
Fn key ──> Audio Capture ──> VAD ──> STT (Whisper) ──> LLM (optional) ──> Text Injection
              cpal/rubato    energy    whisper-rs       provider chain     clipboard/native
              16kHz mono     based     Metal GPU        with failover      CGEvent/SendInput
```

| Crate | Description |
|-------|-------------|
| `chamgei-core` | Pipeline orchestrator, config, context detection, prompts, onboarding |
| `chamgei-audio` | Microphone capture, resampling, energy-based VAD |
| `chamgei-stt` | Local Whisper STT + Groq cloud STT |
| `chamgei-llm` | LLM providers (11 presets + custom) with automatic failover |
| `chamgei-inject` | Cross-platform text injection (clipboard + native) |
| `chamgei-hotkey` | Global Fn-key listener (push-to-talk + toggle) |

## Tauri Desktop App (WIP)

The settings GUI is built with Tauri v2 + React + Tailwind but is still in development. To work on it:

```bash
# Install Tauri CLI
cargo install tauri-cli --version "^2"

# Install frontend deps
npm install --legacy-peer-deps

# Run the desktop app
cargo tauri dev
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and how to submit changes.

## License

Chamgei is dual-licensed under [MIT](LICENSE) or [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0), at your option.
