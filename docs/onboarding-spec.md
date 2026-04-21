# rekody Onboarding Flow -- Product Spec

**Version:** 1.0
**Date:** 2026-03-17
**Target:** Under 90 seconds from app launch to first successful dictation

---

## Architecture Overview

- **Frontend:** React 19 + Tailwind CSS 4, rendered in Tauri v2 webview
- **Backend:** Rust (Tauri v2 IPC commands), workspace crates: `rekody-core`, `rekody-audio`, `rekody-stt`, `rekody-llm`, `rekody-inject`, `rekody-hotkey`
- **Config:** TOML file at `~/.config/rekody/config.toml` (seeded from `config/default.toml`)
- **State persistence:** Onboarding completion flag stored in config. If `onboarding_completed = true`, skip directly to menu bar mode on launch.

---

## Global Onboarding State

All screens share a top-level onboarding controller that manages navigation, accumulated config, and progress.

### `OnboardingProvider` (React Context)

```
State:
  currentStep: number (0-6)
  config: OnboardingConfig {
    sttEngine: "local" | "groq" | "deepgram"
    sttApiKey: string | null
    whisperModel: "tiny" | "small" | "medium" | "large"  // only if sttEngine = "local"
    llmProvider: string | null   // "groq" | "ollama" | "openai" | "anthropic" | "cerebras" | "together" | "openrouter" | "gemini" | "lm-studio" | "vllm" | null
    llmApiKey: string | null
    llmModel: string | null
    skipLlm: boolean
    activationMode: "push_to_talk" | "toggle"
  }
  permissions: {
    microphone: "unknown" | "granted" | "denied"
    accessibility: "unknown" | "granted" | "denied"
  }
  micTestPassed: boolean
  firstDictationPassed: boolean

Methods:
  nextStep()
  prevStep()
  updateConfig(partial: Partial<OnboardingConfig>)
  updatePermissions(partial)
  completeOnboarding()  // writes config.toml, sets onboarding_completed = true
```

### Progress indicator

A thin progress bar or step dots displayed at the top of every screen (except Screen 1). Shows steps 1-7 with the current step highlighted. Clicking a completed step navigates back to it. Steps ahead of current are disabled.

### Window

- Fixed size: 640 x 520px, centered on screen, non-resizable during onboarding
- No title bar decorations (use Tauri `decorations: false` with custom drag region)
- Subtle drop shadow, rounded corners (macOS-native feel via `transparent: true` + CSS border-radius on outer container)

---

## Screen 1: Welcome

**Component:** `WelcomeScreen`

### Layout

- Centered vertically and horizontally
- App icon (96x96, the rekody logo from `src-tauri/icons/`)
- "rekody" in 32px semibold
- Tagline: "Privacy-first voice dictation" in 16px, muted text color
- 16px vertical spacer
- "Get Started" primary button (full-width, max 280px)
- Below button, small muted text: "No account required. No data collected."

### Props / State

- None beyond global onboarding context

### Tauri IPC Commands

- None

### Transitions

- Fade-in on mount (200ms ease-out)
- On "Get Started" click: slide-left transition (300ms) to Screen 2

### Error States

- None. This screen has no failure modes.

### Time Budget

- **5 seconds** (user reads tagline, clicks button)

---

## Screen 2: Choose STT Engine

**Component:** `SttEngineScreen`

### Layout

- Heading: "How should rekody hear you?" (24px semibold)
- Subheading: "Choose your speech-to-text engine" (14px muted)
- Three selectable cards in a vertical stack, each containing:

**Card 1 -- Groq Cloud Whisper** (DEFAULT SELECTED, highlighted border)
- Badge: "Recommended -- fastest setup"
- Icon: cloud icon
- Title: "Groq Cloud Whisper"
- Subtitle: "Fastest transcription. Audio sent to Groq servers."
- Tradeoff pills: `Fast setup` `Low latency` `Requires API key` `Cloud`
- Expandable section (revealed when selected): API key input field with placeholder "gsk_..." and a "Get free key" link (opens `https://console.groq.com` in default browser via `tauri-plugin-shell` `open`)

**Card 2 -- Local Whisper**
- Badge: "Maximum privacy"
- Icon: lock/shield icon
- Title: "Local Whisper"
- Subtitle: "Runs entirely on your Mac. No data leaves your device."
- Tradeoff pills: `100% private` `No API key` `~500MB download` `Slower on Intel`
- Expandable section: Model size selector (radio group): Tiny (75MB, fastest), Small (244MB, balanced), Medium (769MB, best accuracy). Default: Tiny. Show estimated download time based on a conservative 10 MB/s assumption.

**Card 3 -- Deepgram**
- Icon: waveform icon
- Title: "Deepgram Nova-2"
- Subtitle: "Highest accuracy. Audio sent to Deepgram servers."
- Tradeoff pills: `Most accurate` `Requires API key` `Cloud`
- Expandable section: API key input field, "Get free key" link to `https://console.deepgram.com`

- "Continue" button at bottom (disabled until a valid selection: for cloud engines, API key must be non-empty; for local, always valid)
- "Back" text button

### Props / State

```
Local state:
  selectedEngine: "groq" | "local" | "deepgram"
  groqApiKey: string
  deepgramApiKey: string
  whisperModel: "tiny" | "small" | "medium"
  apiKeyValidating: boolean
  apiKeyError: string | null
```

### Tauri IPC Commands

- `validate_api_key(provider: string, key: string) -> Result<bool, String>` -- Makes a lightweight test request to verify the key works. For Groq, hit the models endpoint. For Deepgram, hit the projects endpoint. Returns true/false. Called on blur of the API key field (debounced 500ms).
- `get_whisper_model_status(model: string) -> { downloaded: bool, size_bytes: u64 }` -- Check if the model file already exists in `~/.config/rekody/models/` or the `models/` directory.

### Transitions

- Slide-in from right (300ms)
- Card selection: 150ms border-color + background transition
- Expandable sections: 200ms height animation (CSS `max-height` transition)

### Error States

- Invalid API key: Red border on input, error message "Invalid API key. Check your key at [provider console]."
- Network error during validation: "Could not verify key. Check your internet connection." with a "Retry" button. Allow user to proceed anyway with a "Skip validation" link.

### Time Budget

- **20 seconds** (cloud path with API key paste), **10 seconds** (local path, no key needed)

---

## Screen 3: Choose LLM Provider

**Component:** `LlmProviderScreen`

### Layout

- Heading: "Add AI formatting (optional)" (24px semibold)
- Subheading: "An LLM cleans up grammar, adds punctuation, and formats your dictation. You can skip this and use raw transcription." (14px muted, max 480px)
- Scrollable card grid (2 columns, 3 rows max visible):

**Card: Groq** (DEFAULT SELECTED if user chose Groq STT -- reuse the same key)
- Badge: "Recommended" (only if Groq STT was selected)
- If same key as STT: show "Using your Groq key from previous step" with green checkmark, no second key input
- Model selector dropdown: default "openai/gpt-oss-20b"
- If different from STT or STT was not Groq: API key input

**Card: Ollama (Local)**
- Badge: "100% private"
- On selection: auto-detect running Ollama instance via `http://localhost:11434/api/tags`
- If detected: show dropdown of available models
- If not detected: show message "Ollama not detected. Install it at ollama.ai" with link
- No API key needed

**Card: OpenAI**
- API key input, model selector (default: gpt-4o-mini)

**Card: Anthropic**
- API key input, model selector (default: claude-sonnet-4-20250514)

**Card: Cerebras**
- API key input, model selector (default: llama3.1-8b)

**Card: Together AI**
- API key input, model selector

**Card: OpenRouter**
- API key input, model selector (default: meta-llama/llama-3.1-8b-instruct:free -- note the free tier)

**Card: Gemini**
- API key input, model selector (default: gemini-2.0-flash)

- Below the grid: prominent "Skip -- use raw transcription" link/button (not buried, visible without scrolling)
- "Continue" button (disabled until valid selection OR skip chosen)
- "Back" text button

### Props / State

```
Local state:
  selectedProvider: string | null
  apiKey: string
  model: string
  skipLlm: boolean
  ollamaModels: string[] | null     // null = not checked yet
  ollamaDetected: boolean
  validating: boolean
  validationError: string | null
```

### Tauri IPC Commands

- `validate_api_key(provider: string, key: string) -> Result<bool, String>` -- same as Screen 2
- `detect_ollama() -> Result<Vec<String>, String>` -- hits `http://localhost:11434/api/tags`, returns list of model names or error
- `get_provider_models(provider: string) -> Vec<{id: string, name: string}>` -- returns curated list of recommended models for each provider (hardcoded in Rust, not an API call)

### Transitions

- Slide-in from right (300ms)
- Card grid: staggered fade-in (50ms delay per card)

### Error States

- Invalid API key: same pattern as Screen 2
- Ollama not running: informational message (not blocking -- user can select a different provider)
- Network error: same pattern as Screen 2

### Time Budget

- **15 seconds** (if reusing Groq key), **25 seconds** (if entering new provider key), **3 seconds** (if skipping)

---

## Screen 4: Permissions

**Component:** `PermissionsScreen`

### Layout

- Heading: "Two quick permissions" (24px semibold)
- Subheading: "Both are required for rekody to work. We never collect or transmit your data." (14px muted)
- Two permission cards stacked vertically:

**Card 1 -- Microphone**
- Icon: microphone icon (48px)
- Title: "Microphone Access"
- Explanation: "rekody needs to hear your voice. Audio is processed locally or sent only to your chosen STT provider."
- Status badge: dynamic
  - `unknown`: "Not yet granted" (gray)
  - `granted`: "Granted" (green, with checkmark)
  - `denied`: "Denied -- open System Settings" (red)
- "Grant Access" button: triggers macOS microphone permission dialog via Tauri IPC
- When granted, button transforms into green checkmark with "Granted" text (no button)

**Card 2 -- Accessibility**
- Icon: accessibility/cursor icon (48px)
- Title: "Accessibility Access"
- Explanation: "Required to type text into any app. rekody simulates keystrokes to insert your dictated text."
- Status badge: same pattern as above
- "Open System Settings" button: opens `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility` via `tauri-plugin-shell` `open`
- Note below: "Toggle rekody ON in the list, then come back here."

- "Continue" button at bottom: DISABLED until both permissions are `granted`. Tooltip on hover when disabled: "Grant both permissions to continue."
- "Back" text button

### Props / State

```
Local state:
  micPermission: "unknown" | "granted" | "denied"
  accessibilityPermission: "unknown" | "granted" | "denied"
  pollingInterval: ReturnType<typeof setInterval> | null
```

### Tauri IPC Commands

- `request_microphone_permission() -> Result<String, String>` -- triggers the macOS microphone permission prompt. Returns "granted" or "denied". On macOS, this involves calling `AVCaptureDevice.requestAccess(for: .audio)` equivalent from Rust (via `coreaudio-rs` or direct `objc` call).
- `check_microphone_permission() -> "unknown" | "granted" | "denied"` -- checks current status without prompting.
- `check_accessibility_permission() -> bool` -- checks if accessibility is enabled via `AXIsProcessTrusted()`.
- `open_accessibility_settings()` -- opens System Settings to the Accessibility pane with rekody highlighted.

### Polling

- On mount, start polling both permission statuses every 1 second (using `setInterval` calling the `check_*` IPC commands). This handles the case where the user grants accessibility in System Settings and returns to the app.
- Stop polling when both are granted, or on unmount.

### Transitions

- Slide-in from right (300ms)
- Permission status changes: 300ms color transition + subtle scale pulse (1.0 -> 1.02 -> 1.0 over 300ms)
- When both granted: "Continue" button animates from disabled to enabled (opacity + color transition, 200ms)

### Error States

- Microphone denied: "Microphone access was denied. Open System Settings > Privacy & Security > Microphone and enable rekody." with "Open Settings" button.
- Accessibility not granted after 30 seconds of being on this screen: show helper text "Having trouble? Make sure you see rekody in the Accessibility list and the toggle is ON."
- If user navigates back and returns, re-check permissions immediately.

### Time Budget

- **15 seconds** (if permissions already granted from previous install), **30 seconds** (first-time grant including System Settings navigation)

---

## Screen 5: Mic Test

**Component:** `MicTestScreen`

### Layout

- Heading: "Let's test your microphone" (24px semibold)
- Detected input device name shown below heading: "Using: MacBook Pro Microphone" (14px muted, pulled from system)
- Large centered audio visualizer area (320x80px):
  - Idle state: flat gray line with subtle pulse animation
  - Active state: animated waveform bars (16-24 vertical bars) responding to real-time audio RMS levels
  - Color: gradient from blue (quiet) to green (good level) to orange (too loud/clipping)
- Below visualizer: status text
  - Before audio detected: "Speak something to test your microphone..."
  - During speech: "Hearing you!" (with animated ellipsis)
  - After sufficient audio detected (>1.5 seconds of speech): green checkmark + "Microphone is working great!"
- Volume level indicator: simple horizontal bar showing current RMS level vs. the `vad_threshold` from config
- "Continue" button: DISABLED until mic test passes (audio above VAD threshold detected for at least 1.5 cumulative seconds)
- "Skip" text link (small, muted) for users who know their mic works
- "Back" text button

### Props / State

```
Local state:
  inputDevice: string                    // detected device name
  audioLevel: number                     // current RMS 0.0-1.0
  audioLevelsHistory: number[]           // last 24 RMS samples for visualizer bars
  speechDetectedDuration: number         // cumulative ms of speech detected
  testPassed: boolean
  isListening: boolean
```

### Tauri IPC Commands

- `get_input_device_name() -> String` -- returns the current default audio input device name
- `start_mic_test() -> ()` -- begins capturing audio from default input. Sends periodic events (not request-response).
- `stop_mic_test() -> ()` -- stops the test capture.

### Tauri Events (backend -> frontend)

- `mic-level` -- emitted every ~50ms during mic test. Payload: `{ rms: f32, is_speech: bool }`. Frontend subscribes via `listen("mic-level", callback)` from `@tauri-apps/api/event`.

### Transitions

- Slide-in from right (300ms)
- Visualizer bars: CSS transitions on height (50ms, matching event emission rate)
- Test passed: checkmark scales in from 0 to 1 (300ms spring easing), status text fades in (200ms)
- "Continue" button: enabled animation same as Screen 4

### Error States

- No audio device found: "No microphone detected. Please connect a microphone and click Retry." with "Retry" button that calls `get_input_device_name()` again.
- Audio device found but no signal after 10 seconds: "We're not picking up any sound. Check that your microphone isn't muted in System Settings > Sound." with link to open Sound settings.
- Very low levels (audio detected but below VAD threshold consistently): "Your microphone volume seems low. Try speaking louder or moving closer." and optionally: "Adjust sensitivity" link that reveals a slider to tweak `vad_threshold` (range 0.001 to 0.05).

### Time Budget

- **10 seconds** (speak a few words, see it work, continue)

---

## Screen 6: First Dictation

**Component:** `FirstDictationScreen`

### Layout

- Heading: "Try your first dictation!" (24px semibold)
- Instruction: "Hold the **Fn** key and say something" (16px, with the Fn key rendered as a keyboard key cap `<kbd>` element)
- Centered dictation area (400x160px, rounded border, light background):
  - Idle: placeholder text "Your words will appear here..." in muted italic
  - Recording: pulsing red dot indicator in top-right corner + "Listening..." label, subtle red border glow
  - Processing: spinning indicator + "Transcribing..." (if cloud) or "Processing..." (if local)
  - Result: transcribed text appears with a typewriter-style reveal animation (characters appear sequentially over 500ms). If LLM is enabled, show raw transcription first, then smoothly morph into the formatted version (fade-through transition, 300ms).
- Below the dictation area:
  - Before success: "Hold Fn, speak, then release Fn" (helper text)
  - After success: celebration animation (confetti burst, 1.5 seconds, lightweight CSS-only implementation using pseudo-elements) + "Nice! That's all there is to it." (16px, green)
- "Finish Setup" button: DISABLED until first successful dictation
- "Try Again" button (visible only after first attempt, whether success or failure)
- "Skip" text link (small, muted)
- "Back" text button

### Props / State

```
Local state:
  dictationState: "idle" | "recording" | "processing" | "success" | "error"
  rawTranscription: string | null
  formattedTranscription: string | null
  recordingDuration: number             // ms, for UI indicator
  showCelebration: boolean
  attemptCount: number
```

### Tauri IPC Commands

- `start_recording() -> ()` -- begins audio capture for dictation. Called when Fn key is pressed.
- `stop_recording() -> Result<String, String>` -- stops capture, runs STT pipeline, returns raw transcription.
- `format_text(raw: string) -> Result<String, String>` -- sends raw transcription to the configured LLM provider for formatting. Only called if `skipLlm` is false.

### Tauri Events (backend -> frontend)

- `hotkey-pressed` -- emitted when Fn (or configured hotkey) is pressed. Frontend starts recording state.
- `hotkey-released` -- emitted when Fn is released. Frontend triggers stop_recording.
- `recording-status` -- payload: `{ state: "recording" | "processing" | "done", duration_ms: u32 }`

### Hotkey Note

During onboarding, the hotkey listener should be active ONLY on this screen. The `rekody-hotkey` crate needs an IPC command to enable/disable listening:

- `enable_hotkey_listener() -> ()` -- called on mount
- `disable_hotkey_listener() -> ()` -- called on unmount (cleanup)

### Transitions

- Slide-in from right (300ms)
- Recording state: border glow animation (CSS box-shadow pulse, 1s infinite)
- Typewriter text reveal: staggered character opacity (custom CSS animation or lightweight JS)
- Raw-to-formatted morph: crossfade (300ms)
- Celebration confetti: CSS keyframes, auto-removes after 1.5s
- "Finish Setup" button enable animation

### Error States

- STT failure (network error for cloud, model error for local): "Transcription failed. [Error details]" with "Try Again" button. If cloud STT: "Check your internet connection and API key." If local: "There may be an issue with the Whisper model. Try re-downloading."
- LLM failure: show raw transcription with note "Formatting unavailable. Showing raw transcription." -- this is NOT blocking. User can still proceed.
- No speech detected in recording: "We didn't detect any speech. Make sure to speak while holding Fn." (only if recording was < 0.5s or RMS was below threshold the entire time)
- Hotkey not working: after 15 seconds with no hotkey event, show: "Not detecting the Fn key? Try pressing and holding it firmly." and a fallback: "Or click this button to record" (a manual record button that bypasses the hotkey for onboarding purposes only).

### Time Budget

- **10 seconds** (hold Fn, speak 3-5 words, see result, celebrate)

---

## Screen 7: Summary + Done

**Component:** `SummaryScreen`

### Layout

- Heading: "You're all set!" (28px semibold)
- Subtle celebration icon or animated checkmark (Lottie-free -- use CSS animation of a circle-drawing checkmark, 600ms)
- Configuration summary card (rounded, light background, full width):
  - Row: "Speech-to-Text" -- value (e.g., "Groq Cloud Whisper")
  - Row: "AI Formatting" -- value (e.g., "Groq -- openai/gpt-oss-20b") or "Disabled (raw transcription)"
  - Row: "Activation" -- "Hold Fn to dictate" (or configured mode)
  - Row: "Privacy" -- "Audio processed by [Groq/locally]" or "100% local -- nothing leaves your Mac" (if both STT and LLM are local)
  - Small "Edit" link on each row that navigates back to the relevant screen

- Quick reference box below summary card (subtle border, monospace-friendly):
  - "Quick Reference"
  - `Fn (hold)` -- Start dictating
  - `Fn (release)` -- Stop and transcribe
  - `Cmd + Shift + C` -- Open rekody settings (placeholder, configurable later)

- "Start rekody" primary button (full width, max 280px)
- Below button: "rekody will live in your menu bar" (14px muted, with a small arrow icon pointing up-right toward where the menu bar icon will appear)

### Props / State

```
Props (from OnboardingProvider):
  config: OnboardingConfig         // full accumulated config
  firstTranscription: string       // what they said in Screen 6 (for a nice callback)
```

### Tauri IPC Commands

- `save_config(config: OnboardingConfig) -> Result<(), String>` -- writes the assembled configuration to `~/.config/rekody/config.toml`. Sets `onboarding_completed = true`.
- `start_app() -> ()` -- transitions from the onboarding window to menu-bar-only mode. Closes the onboarding window, initializes the system tray, starts the hotkey listener, and the app is live.

### Transitions

- Slide-in from right (300ms)
- Checkmark draw animation on mount (600ms)
- Summary rows: staggered fade-in (100ms delay per row)
- "Start rekody" click: window shrinks/fades out (300ms) as the menu bar icon appears

### Error States

- Config write failure: "Could not save configuration. [Error details]" with "Retry" button. Provide the file path (`~/.config/rekody/config.toml`) so the user can manually check permissions.
- App start failure: "Could not start rekody. Please try relaunching the app." This should be exceedingly rare.

### Time Budget

- **5 seconds** (scan summary, click Start)

---

## Total Time Budget

| Screen | Typical Time | Worst Case |
|--------|-------------|------------|
| 1. Welcome | 5s | 5s |
| 2. STT Engine | 10-20s | 30s |
| 3. LLM Provider | 3-15s | 25s |
| 4. Permissions | 15s | 30s |
| 5. Mic Test | 10s | 20s |
| 6. First Dictation | 10s | 20s |
| 7. Summary | 5s | 10s |
| **Total** | **58-80s** | **140s** |

**Typical happy path (Groq STT, reuse key for LLM, permissions already granted):** ~58 seconds
**Fast path (local Whisper, skip LLM, permissions pre-granted):** ~40 seconds
**Worst case (new user, cloud STT, new LLM key, both permissions to grant):** ~140 seconds

The 90-second target is achievable for the typical and fast paths. The worst case exceeds 90s but only for users entering multiple API keys for the first time, which is unavoidable.

---

## Tauri IPC Command Summary

All commands are exposed from `src-tauri` via `#[tauri::command]` and invoked from React via `invoke()` from `@tauri-apps/api/core`.

| Command | Screen | Crate | Description |
|---------|--------|-------|-------------|
| `validate_api_key(provider, key)` | 2, 3 | `rekody-core` or `rekody-llm`/`rekody-stt` | Lightweight API call to verify key validity |
| `get_whisper_model_status(model)` | 2 | `rekody-stt` | Check if model file exists locally |
| `detect_ollama()` | 3 | `rekody-llm` | Hit Ollama API, return available models |
| `get_provider_models(provider)` | 3 | `rekody-llm` | Return hardcoded recommended model list |
| `request_microphone_permission()` | 4 | `rekody-audio` | Trigger macOS mic permission prompt |
| `check_microphone_permission()` | 4 | `rekody-audio` | Check mic permission status |
| `check_accessibility_permission()` | 4 | `rekody-inject` | Call `AXIsProcessTrusted()` |
| `open_accessibility_settings()` | 4 | `rekody-inject` | Open System Settings to Accessibility |
| `get_input_device_name()` | 5 | `rekody-audio` | Return default input device name |
| `start_mic_test()` | 5 | `rekody-audio` | Begin audio capture, emit `mic-level` events |
| `stop_mic_test()` | 5 | `rekody-audio` | Stop test capture |
| `enable_hotkey_listener()` | 6 | `rekody-hotkey` | Start listening for Fn key |
| `disable_hotkey_listener()` | 6 | `rekody-hotkey` | Stop listening for Fn key |
| `start_recording()` | 6 | `rekody-audio` | Begin dictation audio capture |
| `stop_recording()` | 6 | `rekody-audio` + `rekody-stt` | Stop capture, run STT, return text |
| `format_text(raw)` | 6 | `rekody-llm` | Send raw text to LLM for formatting |
| `save_config(config)` | 7 | `rekody-core` | Write config.toml |
| `start_app()` | 7 | `rekody-core` | Transition to menu bar mode |

### Tauri Events (backend -> frontend)

| Event | Screen | Payload | Emit Frequency |
|-------|--------|---------|----------------|
| `mic-level` | 5 | `{ rms: f32, is_speech: bool }` | Every ~50ms during mic test |
| `hotkey-pressed` | 6 | `{}` | On Fn key press |
| `hotkey-released` | 6 | `{}` | On Fn key release |
| `recording-status` | 6 | `{ state: string, duration_ms: u32 }` | On state changes during recording |

---

## Component File Structure

```
src/
  main.tsx                          # existing entry point
  App.tsx                           # route: if onboarding needed -> OnboardingShell, else -> menu bar mode
  onboarding/
    OnboardingShell.tsx             # layout wrapper: progress bar + screen container + transitions
    OnboardingProvider.tsx          # React context with all shared state
    screens/
      WelcomeScreen.tsx
      SttEngineScreen.tsx
      LlmProviderScreen.tsx
      PermissionsScreen.tsx
      MicTestScreen.tsx
      FirstDictationScreen.tsx
      SummaryScreen.tsx
    components/
      ProgressBar.tsx               # step indicator dots/bar
      EngineCard.tsx                # reusable selectable card for STT/LLM options
      PermissionCard.tsx            # permission card with status badge
      AudioVisualizer.tsx           # waveform/bar visualizer for mic test
      ApiKeyInput.tsx               # password input with validation state
      KeyCap.tsx                    # <kbd> styled key indicator
      ConfettiAnimation.tsx         # CSS-only confetti burst
      AnimatedCheckmark.tsx         # CSS circle-draw checkmark
    hooks/
      usePermissionPolling.ts       # polls mic + accessibility status
      useMicLevel.ts                # subscribes to mic-level Tauri event
      useHotkey.ts                  # subscribes to hotkey-pressed/released events
      useDictation.ts               # manages recording -> STT -> LLM pipeline
```

---

## Design Tokens (Tailwind)

The onboarding should feel clean, spacious, and fast. Key design decisions:

- **Background:** `bg-neutral-950` (dark mode only for v1 -- matches macOS menu bar apps)
- **Card background:** `bg-neutral-900` with `border border-neutral-800`
- **Selected card:** `border-blue-500 bg-neutral-900/80`
- **Primary button:** `bg-blue-600 hover:bg-blue-500 text-white rounded-xl px-6 py-3`
- **Muted text:** `text-neutral-400`
- **Heading text:** `text-neutral-50`
- **Error text:** `text-red-400`
- **Success/granted:** `text-green-400`
- **Font:** system font stack (`font-sans` in Tailwind, which resolves to `-apple-system, BlinkMacSystemFont, ...` on macOS)
- **Spacing:** generous -- 24px between sections, 16px between cards, 12px card padding
- **Border radius:** `rounded-2xl` for cards, `rounded-xl` for buttons, `rounded-lg` for inputs
- **Transitions:** all interactive elements have `transition-all duration-200`

---

## Edge Cases and Special Behaviors

### Returning Users (Re-onboarding)

If a user deletes their config file or sets `onboarding_completed = false`, the onboarding flow restarts. Previously saved API keys should NOT be pre-populated (they were in the deleted config). This is a clean start.

A "Re-run Setup" option should be available from the menu bar tray menu for users who want to change their STT/LLM provider later.

### Model Download During Onboarding (Local Whisper)

If the user selects Local Whisper on Screen 2, the model download should begin immediately in the background (non-blocking). A small progress indicator appears in the progress bar area. If the download is not complete by Screen 6 (First Dictation), show a progress bar: "Downloading Whisper model... 73%" and disable dictation until complete.

IPC command: `download_whisper_model(model: string) -> ()` (starts background download)
Event: `model-download-progress` with payload `{ model: string, bytes_downloaded: u64, bytes_total: u64, complete: bool }`

### Keyboard Navigation

All screens must be fully keyboard-navigable:
- `Tab` / `Shift+Tab` cycles through interactive elements
- `Enter` activates buttons and selects cards
- `Arrow keys` navigate between cards in a group
- `Escape` triggers "Back" (except on Screen 1)

### Window Behavior

- The onboarding window should be `always_on_top: false` (not aggressive)
- If the user switches to System Settings for permissions, the onboarding window should remain visible when they switch back (no re-focus stealing)
- The window should be draggable via the top 40px (custom drag region since decorations are off)

### Analytics

None. Zero telemetry. No tracking of onboarding completion rates, drop-off screens, or timing. This is a core brand promise. If analytics are ever desired, they must be explicitly opt-in with a dedicated consent screen (not part of this spec).

---

## Open Questions for Engineering

1. **Fn key detection on macOS:** The Fn key behaves differently from regular modifier keys on macOS. Confirm that `rekody-hotkey` can reliably detect Fn press/release via `CGEventTap` or similar. If Fn is problematic, the fallback is `Right Option` or a configurable key shown during onboarding.

2. **Accessibility permission prompt:** macOS does not provide a programmatic way to grant accessibility. The user must manually toggle it in System Settings. Confirm the polling approach (1s interval calling `AXIsProcessTrusted()`) is performant and reliable.

3. **Whisper model download location:** Currently models appear to live in the project's `models/` directory. For the installed app, they should go to `~/Library/Application Support/com.rekody.app/models/` or `~/.config/rekody/models/`. Align with the existing `rekody-stt` crate's expectations.

4. **Window transition to menu bar:** The `start_app()` command needs to close the onboarding webview window and initialize the tray. Confirm Tauri v2 supports closing/hiding the main window while keeping the tray alive without the app quitting (requires `RunEvent::ExitRequested` handling or `prevent_close` + `hide`).
