---
name: chamgei-setup
description: Install and configure Chamgei voice dictation CLI. Use when the user wants to set up voice dictation, install Chamgei, configure STT/LLM providers, or troubleshoot dictation issues.
allowed-tools: Bash, Read, Grep, Write, AskUserQuestion
---

# Chamgei -- Agent Setup Skill

> Point your AI agent (Claude Code, Codex, Cursor, Windsurf, Aider, etc.) at this file
> and it will install and configure Chamgei voice dictation for you.

Chamgei is an open-source, privacy-first voice dictation system. It turns your voice
into text anywhere on your desktop using a local Whisper model for speech-to-text,
optional LLM cleanup, and text injection at your cursor -- all behind a single hotkey.

## Agent Behavior Instructions

**IMPORTANT:** Do NOT make assumptions about the user's preferences. Use the `AskUserQuestion` tool (or equivalent in your agent framework) to ask clarifying questions before proceeding at these decision points:

1. **STT Engine choice** — Ask: "Which speech-to-text engine would you like? Options: [Local] Whisper (private, audio stays on device), [Cloud] Groq Whisper (fast), [Cloud] Deepgram Nova-2 (built-in punctuation). If you pick a cloud option, your audio is sent to that provider."
2. **LLM Provider choice** — Ask: "Would you like an LLM to clean up transcriptions (fix grammar, remove filler words)? Options: Groq (free tier), Ollama (local), OpenAI, Anthropic, or None (skip — Deepgram already includes punctuation)."
3. **API Keys** — Ask: "Please provide your [provider] API key. You can get one at [signup URL]." Never guess or skip API keys.
4. **Whisper model size** (if local STT) — Ask: "Which Whisper model? Tiny (75MB, fastest), Small (250MB, balanced), Medium (750MB), or Large (1.5GB, most accurate)?"
5. **Hotkey preference** — Inform the user: "Chamgei uses the Fn key for dictation. You'll need to set System Settings > Keyboard > 'Press globe key to' → 'Do Nothing'. Is that okay, or would you prefer to configure a different key?"

If the user gives a vague instruction like "just set it up", ask them at minimum which STT engine and whether they want LLM cleanup. Do not default to cloud providers without explicit consent — privacy is a core value of this tool.

**Repository:** <https://github.com/tonykipkemboi/chamgei>
**Version:** 0.3.0
**License:** MIT OR Apache-2.0

---

## Prerequisites

Before installing, verify the following. Run each check and report any failures to the user.

### Required

```bash
# 1. Operating system (macOS or Linux; Windows support is experimental)
uname -s
# Expected: "Darwin" (macOS) or "Linux"

# 2. Architecture
uname -m
# Expected: "arm64" / "aarch64" (Apple Silicon, ARM Linux) or "x86_64"

# 3. curl must be available
command -v curl
```

### Optional (only needed for building from source)

```bash
# Rust toolchain -- only needed if no precompiled binary exists for the platform
rustc --version   # stable 1.85+
cargo --version
```

### macOS-specific

On macOS, Chamgei needs **Accessibility** and **Microphone** permissions. The first-run
wizard will prompt to open System Settings, but an agent can also open them directly:

```bash
# Open Accessibility permissions
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"

# Open Microphone permissions
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
```

**IMPORTANT -- Fn key setting (macOS only):**
The user MUST set **System Settings > Keyboard > "Press globe key to"** to **"Do Nothing"**
so the Fn key is not intercepted by the system. Without this, Chamgei hotkeys will not work.

---

## Installation

Follow the decision tree below. Prefer the one-line installer (Option A) for simplicity.

### Option A: One-line installer (recommended)

Downloads the precompiled binary and a Whisper model. No Rust toolchain needed.

```bash
curl -fsSL https://raw.githubusercontent.com/tonykipkemboi/chamgei/main/install.sh | bash
```

This script will:
1. Detect the platform (macOS/Linux) and architecture (arm64/x86_64)
2. Download the precompiled binary to `/usr/local/bin/chamgei`
3. Download the tiny Whisper model (~75 MB) to `~/.local/share/chamgei/models/`
4. If no precompiled binary is available, fall back to building from source

After install, verify:

```bash
chamgei --version
# Expected output: chamgei 0.3.0
```

### Option B: Build from source

Use this if the one-line installer fails or you want the latest code.

```bash
# 1. Ensure Rust is installed
if ! command -v cargo &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

# 2. Clone the repository
git clone https://github.com/tonykipkemboi/chamgei.git
cd chamgei

# 3. Build the release binary
cargo build -p chamgei-core --bin chamgei --release

# 4. Install the binary
sudo cp target/release/chamgei /usr/local/bin/chamgei
sudo chmod +x /usr/local/bin/chamgei

# 5. Download the default Whisper model
mkdir -p ~/.local/share/chamgei/models
curl -fSL --progress-bar \
    -o ~/.local/share/chamgei/models/ggml-tiny.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
```

### Option C: Make install

If the repo is already cloned:

```bash
cd /path/to/chamgei
make install
```

### Verify installation

```bash
# Binary exists and is executable
which chamgei

# Whisper model is present
ls ~/.local/share/chamgei/models/ggml-tiny.en.bin
```

---

## Configuration

Configuration lives at `~/.config/chamgei/config.toml`. The first-run wizard creates
this file interactively, but an agent can also create or edit it directly.

### Running the interactive onboarding wizard

If you want the user to go through the interactive wizard:

```bash
# Delete existing config to trigger the wizard on next run
# rm ~/.config/chamgei/config.toml

# Run chamgei -- if no config exists, the wizard starts automatically
chamgei
```

The wizard walks through: LLM provider selection, API key entry, Whisper model download,
and macOS permission checks.

### Creating the config file directly (non-interactive)

This is the preferred method for an agent. Write `~/.config/chamgei/config.toml` directly.

```bash
mkdir -p ~/.config/chamgei
chmod 700 ~/.config/chamgei
```

#### Minimal config (local-only, no LLM)

```toml
activation_mode = "push_to_talk"
whisper_model = "tiny"
vad_threshold = 0.01
injection_method = "clipboard"
stt_engine = "local"
```

#### Recommended config (Groq LLM for cleanup)

```toml
activation_mode = "push_to_talk"
whisper_model = "tiny"
vad_threshold = 0.01
injection_method = "clipboard"
stt_engine = "local"

[[providers]]
name = "groq"
api_key = "REPLACE_WITH_API_KEY"
model = "openai/gpt-oss-20b"
```

#### Full config reference

```toml
# ---- Activation ----
# "push_to_talk" = hold Fn to record, release to transcribe
# "toggle"       = press Fn to start, press again to stop
activation_mode = "push_to_talk"

# ---- Whisper model ----
# Options: "tiny" (75 MB), "small" (250 MB), "medium" (750 MB), "large" (1.5 GB)
# Larger models are more accurate but slower.
whisper_model = "tiny"

# ---- Voice Activity Detection ----
# RMS energy threshold. Lower = more sensitive. 0.01 works for most microphones.
vad_threshold = 0.01

# ---- Text injection ----
# "clipboard" = paste via Cmd+V / Ctrl+V (works everywhere)
# "native"    = direct keystroke injection via CGEvent (macOS) / SendInput (Windows)
injection_method = "clipboard"

# ---- Speech-to-text engine ----
# "local"    = on-device Whisper via whisper.cpp (private, no network)
# "groq"     = Groq Cloud Whisper Large v3 (fastest cloud STT, uses groq_api_key or provider key)
# "deepgram" = Deepgram Nova-2 (most accurate cloud STT, needs deepgram_api_key)
stt_engine = "local"

# Only needed if stt_engine = "deepgram"
# deepgram_api_key = "dg_..."

# Only needed if stt_engine = "groq" and no groq provider is configured
# groq_api_key = "gsk_..."

# ---- LLM providers ----
# Providers are tried in order. First success wins. Failover is automatic.
# Local providers (ollama, lm-studio, vllm) need no API key.

[[providers]]
name = "groq"
api_key = "gsk_..."
model = "openai/gpt-oss-20b"

# Uncomment to add fallback providers:
# [[providers]]
# name = "ollama"
# model = "llama3.2:3b"

# [[providers]]
# name = "cerebras"
# api_key = "csk-..."
# model = "llama3.1-8b"

# [[providers]]
# name = "openai"
# api_key = "sk-..."
# model = "gpt-4o-mini"

# [[providers]]
# name = "anthropic"
# api_key = "sk-ant-..."
# model = "claude-sonnet-4-20250514"

# [[providers]]
# name = "gemini"
# api_key = "AIza..."
# model = "gemini-2.0-flash"

# ---- Custom OpenAI-compatible endpoint ----
# [[providers]]
# name = "my-server"
# base_url = "https://my-llm-server.com/v1/chat/completions"
# api_key = "my-secret-key"
# model = "my-model"
```

### Supported LLM providers

| Provider     | Type  | Auth     | Default model                              | Sign-up URL                        |
|-------------|-------|----------|--------------------------------------------|------------------------------------|
| `groq`      | Cloud | API key  | `openai/gpt-oss-20b`                      | https://console.groq.com           |
| `cerebras`  | Cloud | API key  | `llama3.1-8b`                              | https://cerebras.ai                |
| `together`  | Cloud | API key  | `Meta-Llama-3.1-8B-Instruct-Turbo`        | https://together.ai                |
| `openrouter`| Cloud | API key  | `meta-llama/llama-3.1-8b-instruct:free`   | https://openrouter.ai              |
| `fireworks` | Cloud | API key  | User's choice                              | https://fireworks.ai               |
| `openai`    | Cloud | API key  | `gpt-4o-mini`                              | https://platform.openai.com        |
| `anthropic` | Cloud | API key  | `claude-sonnet-4-20250514`                 | https://console.anthropic.com      |
| `gemini`    | Cloud | API key  | `gemini-2.0-flash`                         | https://aistudio.google.com        |
| `ollama`    | Local | None     | `llama3.2:3b`                              | https://ollama.com                 |
| `lm-studio` | Local | None    | Loaded model                               | https://lmstudio.ai                |
| `vllm`      | Local | None     | User's choice                              | https://vllm.ai                    |
| Custom      | Any   | Optional | User's choice                              | --                                 |

### Supported STT engines

| Engine    | Privacy     | Speed    | Accuracy    | Requirements               |
|-----------|-------------|----------|-------------|-----------------------------|
| `local`   | Best (offline) | Good  | Good        | Whisper model downloaded     |
| `groq`    | Audio sent to Groq | Fastest | Very good | Groq API key            |
| `deepgram`| Audio sent to Deepgram | Fast | Best     | Deepgram API key            |

### API key storage

On macOS, Chamgei stores API keys in the system Keychain under the service
`com.chamgei.voice`. The onboarding wizard handles this automatically.

To store a key via the agent without running the wizard, write the key directly
into the config file. The config file should have permissions `600`:

```bash
chmod 600 ~/.config/chamgei/config.toml
```

### Changing settings without re-running onboarding

Edit `~/.config/chamgei/config.toml` directly with any text editor or agent tool.
Changes take effect the next time `chamgei` is started. There is no need to re-run
the onboarding wizard.

Examples of common changes:

```bash
# Switch to a larger Whisper model for better accuracy
# In config.toml, change: whisper_model = "small"
# Then download the model:
curl -fSL --progress-bar \
    -o ~/.local/share/chamgei/models/ggml-small.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin

# Switch to toggle mode instead of push-to-talk
# In config.toml, change: activation_mode = "toggle"

# Add a fallback provider
# Append to config.toml:
# [[providers]]
# name = "ollama"
# model = "llama3.2:3b"
```

### Whisper model downloads

Models are stored in `~/.local/share/chamgei/models/` (or `$CHAMGEI_MODEL_DIR` if set).

| Model    | File                   | Size    | Download URL                                                                       |
|----------|------------------------|---------|------------------------------------------------------------------------------------|
| tiny     | `ggml-tiny.en.bin`     | ~75 MB  | `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin`       |
| small    | `ggml-small.en.bin`    | ~250 MB | `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin`      |
| medium   | `ggml-medium.en.bin`   | ~750 MB | `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin`     |
| large    | `ggml-large.bin`       | ~1.5 GB | `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large.bin`         |

---

## Verification

After installation and configuration, verify the setup works end-to-end.

### Step 1: Check binary

```bash
chamgei --version
# Expected: chamgei 0.3.0
```

### Step 2: Check config

```bash
test -f ~/.config/chamgei/config.toml && echo "Config exists" || echo "Config MISSING"
```

### Step 3: Check Whisper model

```bash
WHISPER_MODEL=$(grep 'whisper_model' ~/.config/chamgei/config.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
case "$WHISPER_MODEL" in
    tiny)   FILE="ggml-tiny.en.bin" ;;
    small)  FILE="ggml-small.en.bin" ;;
    medium) FILE="ggml-medium.en.bin" ;;
    large)  FILE="ggml-large.bin" ;;
    *)      FILE="ggml-tiny.en.bin" ;;
esac
test -f ~/.local/share/chamgei/models/$FILE && echo "Model exists" || echo "Model MISSING"
```

### Step 4: Test launch

```bash
# Run chamgei -- it should start listening for hotkeys.
# If config is valid, you will see log output like:
#   chamgei pipeline starting
#   hotkey listener started
#   audio capture initialized
#
# Press Ctrl+C to stop.
chamgei
```

### Step 5: Verify Fn key (macOS only)

Tell the user:
1. Open any text editor or text field
2. Hold the Fn key and speak a short phrase
3. Release the Fn key
4. The transcribed text should appear at the cursor

If the Fn key opens the emoji picker instead, the user needs to change:
**System Settings > Keyboard > "Press globe key to" > "Do Nothing"**

---

## Troubleshooting

### Fn key opens emoji picker / input sources (macOS)

**Cause:** macOS intercepts the Fn key before Chamgei can see it.
**Fix:** System Settings > Keyboard > set "Press globe key to" to "Do Nothing".

### "Permission denied" when accessing microphone

**Cause:** macOS has not granted microphone access to the terminal / chamgei.
**Fix:**
```bash
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
```
Add Terminal.app (or iTerm, Alacritty, etc.) to the allowed list.

### "Permission denied" for Accessibility

**Cause:** Text injection via native keystrokes requires Accessibility permission.
**Fix:**
```bash
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
```
Add Terminal.app to the allowed list. If using `injection_method = "clipboard"`, this
is less critical since clipboard paste usually works without Accessibility.

### API key errors ("401 Unauthorized")

**Cause:** Invalid or expired API key in config.
**Fix:** Check the key in `~/.config/chamgei/config.toml` and verify it at the
provider's dashboard (e.g., https://console.groq.com for Groq).

### No sound detected / empty transcriptions

**Cause:** VAD threshold too high, or wrong microphone selected.
**Fix:**
- Lower `vad_threshold` in config (try `0.005`)
- Check that the correct microphone is set as the system default input device
- Speak louder or closer to the microphone

### Whisper model not found

**Cause:** Model file missing from the model directory.
**Fix:**
```bash
# Check which model is configured
grep 'whisper_model' ~/.config/chamgei/config.toml

# Download the matching model (example for "tiny")
mkdir -p ~/.local/share/chamgei/models
curl -fSL --progress-bar \
    -o ~/.local/share/chamgei/models/ggml-tiny.en.bin \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
```

### Build fails with Rust errors

**Cause:** Outdated Rust toolchain or missing system dependencies.
**Fix:**
```bash
rustup update stable
# On Linux, you may also need:
# sudo apt install libasound2-dev libxdo-dev  (Debian/Ubuntu)
# sudo dnf install alsa-lib-devel xdotool     (Fedora)
```

### LLM formatting not working (raw text injected)

**Cause:** No LLM provider configured, or all providers failing.
**Fix:** Check that at least one `[[providers]]` block in config.toml has a valid
API key (or is a local provider like Ollama that is running).

---

## For Agents: Integration Guide

This section is for AI agents that want to USE Chamgei data or control Chamgei
programmatically, not just install it.

### Reading transcription history

All dictations are saved to `~/.config/chamgei/history.json`. The file is JSON
with this structure:

```json
{
  "entries": [
    {
      "text": "The final LLM-cleaned text that was injected",
      "raw_transcript": "the raw stt output before llm cleanup",
      "timestamp": "2026-03-18T14:30:00Z",
      "stt_latency_ms": 450,
      "llm_latency_ms": 120,
      "provider": "groq",
      "app_context": "VS Code"
    }
  ]
}
```

Entries are stored newest-first, capped at 5000. The file has `600` permissions.

**Example: Read the last 5 dictations**

```bash
cat ~/.config/chamgei/history.json | python3 -c "
import json, sys
data = json.load(sys.stdin)
for e in data['entries'][:5]:
    print(f\"[{e['timestamp']}] ({e['app_context']}) {e['text']}\")
"
```

**Example: Search history for a phrase**

```bash
cat ~/.config/chamgei/history.json | python3 -c "
import json, sys
data = json.load(sys.stdin)
query = sys.argv[1].lower()
for e in data['entries']:
    if query in e['text'].lower() or query in e['raw_transcript'].lower():
        print(f\"[{e['timestamp']}] {e['text']}\")
" "search term"
```

### File locations summary

| What                 | Path                                          |
|----------------------|-----------------------------------------------|
| Binary               | `/usr/local/bin/chamgei`                      |
| Config               | `~/.config/chamgei/config.toml`               |
| History              | `~/.config/chamgei/history.json`              |
| Whisper models       | `~/.local/share/chamgei/models/`              |
| Keychain service     | `com.chamgei.voice` (macOS Keychain)          |
| Model dir override   | `$CHAMGEI_MODEL_DIR` environment variable     |

### Hotkey reference

| Action                                              | Shortcut       |
|-----------------------------------------------------|----------------|
| Push-to-talk (hold to record, release to transcribe)| `Fn`           |
| Hands-free toggle (press to start, press to stop)   | `Fn + Space`   |
| Command mode (transform selected text by voice)     | `Fn + Enter`   |

### Architecture overview

```
Fn key --> Audio Capture --> VAD --> STT (Whisper) --> LLM (optional) --> Text Injection
             cpal/rubato    energy    whisper-rs       provider chain     clipboard/native
             16kHz mono     based     Metal GPU        with failover      CGEvent/SendInput
```

| Crate           | Purpose                                                    |
|-----------------|------------------------------------------------------------|
| `chamgei-core`  | Pipeline orchestrator, config, context detection, prompts  |
| `chamgei-audio` | Microphone capture, resampling, energy-based VAD           |
| `chamgei-stt`   | Local Whisper STT + Groq/Deepgram cloud STT                |
| `chamgei-llm`   | LLM providers (11 presets + custom) with automatic failover|
| `chamgei-inject` | Cross-platform text injection (clipboard + native)        |
| `chamgei-hotkey` | Global Fn-key listener (push-to-talk + toggle)            |

### Uninstall

```bash
# Remove binary
sudo rm -f /usr/local/bin/chamgei

# Remove config and history
rm -rf ~/.config/chamgei

# Remove Whisper models
rm -rf ~/.local/share/chamgei
```
