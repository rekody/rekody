# Changelog

## [Unreleased]

## [0.5.17] - 2026-07-12

### Added

- **Preferred input device.** Since mic-on-demand shipped, every recording re-queried the system default input, so capture silently drifted to whatever connected last (AirPods being the classic offender). Set `input_device = "MacBook Pro Microphone"` (any unique substring works) in config.toml or pick a device during `rekody setup`, and rekody pins capture to it. `rekody doctor` shows exactly which device will capture and warns when a pinned device is not connected, falling back to the system default so dictation never breaks.

### Changed

- Dependency freshness: tokio, serde_json, and libc patch updates; CI actions moved to newer SHA-pinned releases with artifact digest verification.

## [0.5.16] - 2026-07-11

### Fixed

- **AI cleanup can no longer answer you instead of cleaning your words.** Small cleanup models sometimes treated a dictation as a question and typed a reply at your cursor (a refusal, a fabricated response, or a single word). A deterministic guard now rejects cleanup output that collapses, balloons, or opens like a chat reply, and your dictionary-corrected words go through instead. User-selected skills are exempt since they legitimately transform text.

### Added

- **Every dictation records the app it landed in.** The frontmost app is captured at recording start (off the hot path, zero added latency) and stamped into history and the local training manifest, powering per-app tags and stats in the apps. Cleanup also reuses this capture instead of probing again, saving about 100ms when cleanup is on.

## [0.5.15] - 2026-07-11

### Added

- **Your dictionary now corrects instantly, no AI required.** A deterministic pass fixes near-miss transcriptions of your terms ("recody" becomes "Rekody") and recases exact matches, before and after optional AI cleanup. It never rewrites everyday English words into your terms ("change" stays "change" even when a term is spelled close), never guesses between two close terms, and runs in microseconds.
- **Snippets expand when spoken.** Say "slash sig" (or the literal "/sig") anywhere in a dictation and the saved snippet expands in place, multi-line signatures included.
- **History entries record measured speaking time.** Duration comes from the exact audio sample count, so words-per-minute stats in the apps are honest.

### Changed

- New setups default the recording safety stop to 30 minutes (was 10), matching the Mac app's picker, and the setup wizard shows the real Whisper download sizes.

## [0.5.14] - 2026-07-11

### Added

- **Rekody Streaming is now the streaming engine, served from Rekody's own model repo.** The streaming model downloads exclusively from huggingface.co/Rekody with pinned SHA-256 checksums verified on download. Whisper models also download mirror-first from Rekody/rekody-models with checksum verification and upstream fallback.

### Fixed

- `rekody update` verifies the release's SHA256SUMS before installing, so a tampered or corrupted download can never replace your binary.
- Whisper large model downloads work again after the upstream file rename.

### Changed

- Supply-chain hardening in CI: cargo-deny license and advisory gate, all GitHub Actions pinned to commit SHAs. Early Windows groundwork landed behind the scenes (nothing user-facing yet).

## [0.5.13] - 2026-06-26

### Fixed

- **Your last word no longer gets clipped.** Releasing the key the instant you finished speaking could drop the final word while the audio tail was still in flight. A 180ms capture tail now lets the pipeline drain before finalizing, and a quick re-press within the tail resumes the same dictation instead of splitting it.

## [0.5.12] - 2026-06-12

### Added

- **Hold or tap on the same key.** Hold ⌥ + space and release to insert, as always. Or quick-tap to latch hands-free recording and tap again to stop; the console shows the latch state. New configs default to `activation_mode = "both"`.
- **`rekody changelog`** shows what changed in the latest release from your terminal; `--all` lists recent releases and marks the one you are on.

### Changed

- **The mic opens only while you dictate.** The orange macOS mic indicator used to stay lit the whole time rekody ran. The stream now opens at key-down (about 50 to 90ms, faster than you can start speaking) and closes when the dictation ends. Every dictation re-checks the default input device, so switching headsets no longer needs a restart.

## [0.5.11] - 2026-06-12

### Added

- **`rekody fix`** corrects the transcript label of a recent dictation in the moment, keeping your local training dataset accurate.
- HUD groundwork: a socket server and helper supervision so a pill overlay can ride along with the daemon.

## [0.5.10] - 2026-06-12

### Added

- **Local training-data capture.** Each dictation can save its audio (FLAC) and raw transcript under `~/.local/share/rekody/training-data/`, building a personal fine-tuning dataset. Local only, owner-only permissions, off with `save_training_data = false`.
- **Studio CLI.** A live status region with waveform, timer, and branded setup replaces plain log lines.

### Changed

- `rekody setup` lists the streaming engine first.

## [0.5.9] - 2026-06-09

### Added

- **Real-time streaming dictation.** The on-device Nemotron engine transcribes while you talk; the final text lands about 50ms after you release the key, where batch engines take 1 to 3 seconds.
- **Deterministic number formatting.** A final cleanup pass converts spoken quantities to written form that LLMs handle inconsistently and the raw path not at all: compound numbers ("three hundred fifty" becomes "350"), currency ("fifty dollars" becomes "$50"), percent, and a curated set of unambiguous units. Conservative by design: isolated small numbers stay as words, ambiguous units are left alone.

## [0.5.8] - 2026-05-29

### Changed

- **More faithful default cleanup.** The cleanup prompt now (1) treats dictation strictly as text to clean, never an instruction to act on — so "send the email to Sarah" or "write a note that …" gets cleaned, not *composed* into an email/note; (2) honors spoken retractions — "scratch that", "never mind", "I changed my mind about the last statement", "actually, make it …" remove the retracted words (and the retraction phrase) and keep the final intent; and (3) preserves meaning and length, no padding or stylistic rewrites. The on-device Apple helper also now decodes greedily (temperature 0) so it stops embellishing.

## [0.5.7] - 2026-05-29

### Added

- **On-device cleanup via Apple Foundation Models** (macOS 26+) — a new `apple` LLM provider that cleans up / reshapes dictation using Apple Intelligence's built-in on-device model: zero download, no API key, fully private, ~0.5s per cleanup. Runs through a small Swift helper (`rekody-fm`) that ships **bundled in the Apple Silicon Homebrew release** (built on the `macos-26` CI runner; adhoc-signed like the main binary — no notarization needed for a Homebrew formula). Choose "Apple on-device" in `rekody setup` (or add a `name = "apple"` provider in config); `rekody doctor` reports availability. Building from source? Install the helper with `make fm-helper`. Falls through to other providers / raw transcript when unavailable, so it never breaks dictation.
- A **`RawTranscriptFallback`** final tier in the provider chain so dictation always returns text even if every configured LLM provider is unavailable.

### Fixed

- **Active skill now shows in the live status line.** After `⌥Space+Tab` cycling, the one-time startup banner couldn't repaint, so its "Skill" row went stale. The active skill is now shown in the idle status line (which re-renders) and stays current.

## [0.5.6] - 2026-05-29

### Added

- **Skills** — reusable LLM presets that reshape dictation into a specific form (email, notes, spec, commit message, slack, summary, todo, journal). Skills are Markdown files (frontmatter + a prompt body) in `~/.config/rekody/skills/`; a starter pack ships embedded and seeds on first run. Manage with `rekody skill` (interactive sticky picker), `rekody skill use <name>`, `rekody skill none`, and `rekody skill list`. An active skill overrides the built-in per-app prompt; skills may also declare `triggers:` to auto-apply when a matching app is focused. The applied skill is surfaced in the live status line and the startup banner. Requires LLM post-processing to take effect.
- **Live skill switching** — hold `⌥Space` and tap `Tab` to cycle the active skill (`Auto → … → Auto`) without stopping dictation. The new skill is surfaced in the status line and applies to the next dictation.
- **Custom vocabulary** — `rekody dictionary add/remove/list` manages a personal term list (`~/.config/rekody/dictionary.toml`) appended to the cleanup prompt so the model preserves jargon, names, and product terms verbatim (e.g. keeps "rekody" instead of "record"). Affects the LLM cleanup step.
- **`rekody bench`** — benchmark local Whisper transcription latency (mean / p50 / p95 / RTF) against a bundled audio sample. Handy for A/B'ing Core ML vs Metal on Apple Silicon.

### Fixed

- **`todo` skill no longer copies its own examples into output.** The example wording in the prompt (e.g. "Email Sarah the deck", "by Friday") could leak into results when the input mentioned a matching name; examples are now abstract and the prompt forbids reproducing them.

### Changed

- **`brew install` is now a one-liner** in all docs: `brew install rekody/rekody/rekody` (auto-taps), avoiding the missed-`brew tap` step.

## [0.5.3] - 2026-04-27

### Fixed

- **`rekody update` now replaces the binary that's actually running** instead of always writing to `/usr/local/bin/rekody`. Previously, Homebrew installs (or any install outside `/usr/local/bin`) silently desynced: the updater reported success while the running binary stayed on the old version. The install target now resolves from `std::env::current_exe()` and follows symlinks.
- **Atomic replace via `rename(2)`** — staging next to the target and renaming over avoids `ETXTBSY` ("text file busy") when replacing the running binary on Linux, and removes a small race window on all platforms.

### Changed

- **Homebrew-aware updater:** `rekody update` and `rekody update --check` now detect Homebrew installs (paths under `Cellar/` or `homebrew/`) and direct users to `brew upgrade rekody` rather than clobbering the keg and breaking `brew`'s bookkeeping.
- Sudo fallback now uses `install -m 0755` so permissions are set in one shot.

## [0.5.2] - 2026-04-23

### Fixed

- **Long-audio transcription:** `LocalWhisperEngine::build_params` now branches on audio length. Audio >25s uses multi-segment decoding with timestamps and hallucination guards (`no_speech_thold=0.6`, `logprob_thold=-1.0`). Audio ≤25s keeps the original single-segment fast path, so dictation latency is unchanged. Adds a `short_audio_smoke` smoke test.

### Changed

- **Moved to GitHub organization:** `tonykipkemboi/rekody` → `rekody/rekody`. All install commands, documentation, release workflow, and in-app references point to the new location.
- **Homebrew tap moved:** `tonykipkemboi/homebrew-rekody` → `rekody/homebrew-rekody`. Re-tap with `brew untap tonykipkemboi/rekody && brew tap rekody/rekody`.
- New lowercase `r` + dot lettermark replaces the former chamgei "C" mark across the website and GitHub avatar.

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
