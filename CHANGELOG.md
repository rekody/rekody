# Changelog

## [0.5.1] - 2026-04-21

### Added

- **Turbo Whisper model** (`ggml-large-v3-turbo-q5_0.bin`, ~574 MB) — distilled large-v3 quantized to 5-bit. ~8× faster decode than full large with near-large accuracy. Now the default for local STT.

### Changed

- Local Whisper picker in `rekody setup` now preselects **Turbo** and lists it first.
- `config/default.toml` default `whisper_model` changed from `"tiny"` to `"turbo"`.
- Unknown `whisper_model` values fall back to `turbo` (previously `small`).

## [0.5.0] - 2026-04-19

### Changed (Breaking)

- **Project renamed: `chamgei` → `rekody`.** Hard cutover, no backward compatibility.
- **Binary renamed:** `chamgei` → `rekody`. Update scripts, aliases, and shell completions.
- **All 6 crates renamed:**
  - `chamgei-core` → `rekody-core`
  - `chamgei-audio` → `rekody-audio`
  - `chamgei-stt` → `rekody-stt`
  - `chamgei-llm` → `rekody-llm`
  - `chamgei-inject` → `rekody-inject`
  - `chamgei-hotkey` → `rekody-hotkey`
- **Config directory moved:** `~/.config/chamgei/` → `~/.config/rekody/` (including `config.toml` and `history.json`).
- **Model directory moved:** `~/.local/share/chamgei/models/` → `~/.local/share/rekody/models/`.
- **Keychain service changed:** `com.chamgei.voice` → `com.rekody.voice`. **Users must re-add API keys** — stored keys under the old service will not be read.
- **Environment variable renamed:** `CHAMGEI_MODEL_DIR` → `REKODY_MODEL_DIR`.
- **GitHub repo renamed:** `tonykipkemboi/chamgei` → `tonykipkemboi/rekody`.
- **Homebrew tap moved:** `tonykipkemboi/homebrew-chamgei` → `tonykipkemboi/homebrew-rekody`. Re-tap with `brew untap tonykipkemboi/chamgei && brew tap tonykipkemboi/rekody`.

### Migration

Existing users should run `rekody setup` fresh to regenerate config, move/redownload models, and re-store API keys in the keychain. The old `~/.config/chamgei/` directory can be deleted once you've confirmed `rekody` is working.

## v0.3.0 (2026-03-18)

### Added
- GUI onboarding wizard (7-step Tauri app)
- 11 LLM providers: Groq, Cerebras, Together, OpenRouter, Fireworks, OpenAI, Anthropic, Gemini, Ollama, LM Studio, vLLM
- 3 STT engines: Local Whisper (Metal GPU), Groq Cloud Whisper, Deepgram Nova-2
- Secure API key storage via macOS Keychain
- Transcription history with searchable UI
- Polished CLI with cliclack onboarding and indicatif status
- Context-aware LLM formatting (code editors, messaging, email)
- Command mode for voice-driven text transformation
- Personal dictionary and saved snippets
- Auto-learning from corrections
- Usage statistics tracking
- 10-minute max recording (beats Wispr Flow's 6 min)
- One-line installer script
- Security: config permissions, input sanitization, checksum verification

### Fixed
- Whisper.cpp stderr output suppressed in TUI
- Empty LLM responses fall back to raw transcript
- Clipboard restored on injection error
- VAD no longer chunks speech during push-to-talk recording

## v0.1.0 (2026-03-16)
- Initial release: core pipeline, basic CLI
