# rekody on Windows — port status

Tracking doc for the Windows CLI port. macOS remains the reference platform;
this captures what's proven, what's left, and the decisions still open.

## Status at a glance

| Layer | State | How it's verified |
|---|---|---|
| Hotkey (`rekody-hotkey`) | ✅ done | `WH_KEYBOARD_LL` hook; runtime-tested in CI via `win_hook_check` (all activation modes + deadman) |
| Text injection (`rekody-inject`) | ✅ done | clipboard + `SendInput` Ctrl+V; runtime-tested in CI via `win_inject_check` (pastes into a real EDIT control, reads it back verbatim) |
| Audio capture (`rekody-audio`) | ✅ builds | cpal → WASAPI; builds on Windows CI. End-to-end capture not yet VM-tested |
| STT (`rekody-stt`) | ✅ builds | whisper.cpp (cmake/MSVC) + onnxruntime (ort/parakeet-rs) build on Windows CI |
| Daemon (`rekody-core`) | ✅ builds (debug) | full `rekody.exe` builds on Windows CI; HUD unix-socket is `#[cfg(unix)]` with a Windows no-op stub |
| User-facing polish | ✅ done | platform `TRIGGER_LABEL` (Ctrl+Space), afplay gated, onboarding/doctor degrade gracefully |
| Release `.exe` packaging | ⏳ next | debug build proven; release build + smoke-run not yet wired |
| End-to-end dictation | ⏳ blocked | needs the local Win11 VM (audio → STT → inject on a real desktop) |

## What's verified in CI (`windows-spike` job)

Runs on `windows-latest` on every PR:
1. Portable crates + hotkey hook build.
2. Hotkey **runtime** test — synthesizes Ctrl+Space via `SendInput`, asserts the
   activation state machine emits the right events for ptt / toggle / both / deadman.
3. Injection **runtime** test — real EDIT control, runs the public `inject_text`,
   reads the control back and asserts the transcript round-tripped.
4. `rekody-stt` builds (the biggest C-dependency unknown).
5. Full `rekody-core` binary builds.

The two scariest unknowns — "do the C STT deps build on Windows?" and "do the
low-level hook + SendInput actually work in a headless CI session?" — are both
resolved positively.

## What's left for a beta-usable Windows CLI

**Phase 1 (before beta):**
- [ ] Release build + smoke-run in CI (`cargo build --release`, then `rekody.exe
      --version` / `--help` exit 0) — proves the shippable artifact builds and runs.
- [ ] Package `rekody.exe` as a release asset (zip) and wire it into `release.yml`.
- [ ] `install.ps1` (PowerShell `irm | iex`) mirroring `install.sh`.
- [ ] Tier-3: local Win11 VM end-to-end dictation (audio → STT → inject) on a real
      desktop. **This is the gate before shipping to Windows beta users.**

**Phase 2 (post-beta polish):**
- [ ] `command_mode` (PowerShell copy/paste) and `context` (active-app detection)
      have Windows code paths but are untested; "Unknown" app context is an
      acceptable fallback for now.
- [ ] Verify `keyring` uses the Windows Credential Manager backend at runtime.
- [ ] Config-template docstring still says `option_space`/`fn_key` — cosmetic;
      the Windows hook ignores `trigger_key` (hardcoded Ctrl+Space).
- [ ] Clip playback for `rekody fix --play` (currently macOS-only; honest
      "not supported yet" message elsewhere).

## Open decisions (need Tony)

1. **Trigger key.** Windows uses **Ctrl+Space** as a beta default. ⌥Space is
   macOS-only; on Windows Alt+Space opens the window menu and Win+Space switches
   input methods — both unusable. Ctrl+Space collides with some IMEs. The likely
   real answer is a **user-configurable hotkey**, which is also the cross-platform
   endgame. Decide whether to ship Ctrl+Space for beta or build config first.

2. **Code signing / SmartScreen.** An unsigned `.exe` triggers "Windows protected
   your PC" (SmartScreen) — the Windows analog of the macOS notarization work
   already done, and a real trust barrier for a privacy-first app. Options:
   ship unsigned for beta (with a documented "More info → Run anyway"), an OV
   cert (cheap, still shows warnings until reputation builds), or an EV cert
   (immediate trust, ~$300+/yr, hardware token). Decision + spend needed.

3. **Distribution channel.** macOS has Homebrew + `install.sh`. Windows options:
   `install.ps1` one-liner (fastest to ship), **Scoop** (dev-friendly, no signing
   needed), or **winget** (widest reach, submission + manifest review). Recommend
   `install.ps1` + Scoop for beta, winget later.

## Notes

- Windowing features the injection test needs live in a **target-gated
  dev-dependency** on `rekody-inject`, so the shipped lib's Windows surface stays
  at `KeyboardAndMouse` only.
- `crates/rekody-core/src/history_tui.rs` remains the untouchable gold-standard
  reference; nothing here modifies it.
