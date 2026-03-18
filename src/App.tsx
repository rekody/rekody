import React, { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

// ─── Types ───────────────────────────────────────────────────────────────────

type Tab = "general" | "inference" | "audio" | "hotkeys" | "history";

interface HistoryEntry {
  text: string;
  timestamp: string;
  stt_ms: number;
  llm_ms: number;
  provider: string;
  app_context: string;
}

interface Settings {
  activationMode: "push_to_talk" | "toggle";
  sttEngine: "local" | "groq" | "deepgram";
  llmProvider: "cerebras" | "groq" | "local";
  groqApiKey: string;
  cerebrasApiKey: string;
  deepgramApiKey: string;
  whisperModel: "tiny" | "small" | "medium" | "large";
  vadThreshold: number;
  injectionMethod: "clipboard" | "native";
}

const defaultSettings: Settings = {
  activationMode: "push_to_talk",
  sttEngine: "local",
  llmProvider: "cerebras",
  groqApiKey: "",
  deepgramApiKey: "",
  cerebrasApiKey: "",
  whisperModel: "small",
  vadThreshold: 0.5,
  injectionMethod: "clipboard",
};

type SttEngine = "local" | "groq" | "deepgram";

interface OnboardingState {
  step: number;
  sttEngine: SttEngine;
  sttApiKey: string;
  whisperModel: "tiny" | "small" | "medium" | "large";
  llmProvider: string;
  llmApiKey: string;
  llmModel: string;
  micGranted: boolean;
  accessibilityGranted: boolean;
  micWorking: boolean;
  firstDictationDone: boolean;
}

const TOTAL_STEPS = 7;

// ─── Root App ────────────────────────────────────────────────────────────────

function App() {
  const [showOnboarding, setShowOnboarding] = useState<boolean | null>(null);

  useEffect(() => {
    invoke<boolean>("needs_onboarding")
      .then((needs) => setShowOnboarding(needs))
      .catch(() => setShowOnboarding(false));
  }, []);

  if (showOnboarding === null) {
    return (
      <div className="flex items-center justify-center h-screen text-sm text-[var(--text-secondary)]">
        Loading...
      </div>
    );
  }

  if (showOnboarding) {
    return <OnboardingWizard onComplete={() => setShowOnboarding(false)} />;
  }

  return <SettingsApp />;
}

// ─── Onboarding Wizard ──────────────────────────────────────────────────────

function OnboardingWizard({ onComplete }: { onComplete: () => void }) {
  const [state, setState] = useState<OnboardingState>({
    step: 1,
    sttEngine: "groq",
    sttApiKey: "",
    whisperModel: "small",
    llmProvider: "groq",
    llmApiKey: "",
    llmModel: "llama-3.3-70b-versatile",
    micGranted: false,
    accessibilityGranted: false,
    micWorking: false,
    firstDictationDone: false,
  });

  const [transitioning, setTransitioning] = useState(false);

  const update = <K extends keyof OnboardingState>(
    key: K,
    value: OnboardingState[K]
  ) => {
    setState((prev) => ({ ...prev, [key]: value }));
  };

  const goTo = (step: number) => {
    setTransitioning(true);
    setTimeout(() => {
      update("step", step);
      setTransitioning(false);
    }, 200);
  };

  const next = () => goTo(state.step + 1);
  const back = () => goTo(state.step - 1);

  const renderStep = () => {
    switch (state.step) {
      case 1:
        return <WelcomeStep onNext={next} />;
      case 2:
        return (
          <SttStep state={state} update={update} onNext={next} onBack={back} />
        );
      case 3:
        return (
          <LlmStep state={state} update={update} onNext={next} onBack={back} />
        );
      case 4:
        return (
          <PermissionsStep
            state={state}
            update={update}
            onNext={next}
            onBack={back}
          />
        );
      case 5:
        return (
          <MicTestStep
            state={state}
            update={update}
            onNext={next}
            onBack={back}
          />
        );
      case 6:
        return (
          <TryItStep
            state={state}
            update={update}
            onNext={next}
            onBack={back}
          />
        );
      case 7:
        return (
          <AllSetStep state={state} onComplete={onComplete} onBack={back} />
        );
      default:
        return null;
    }
  };

  return (
    <div className="flex flex-col h-screen select-none bg-[var(--bg-primary)]">
      <div className="flex-1 overflow-y-auto">
        <div
          className={transitioning ? "step-exit" : "step-active"}
          key={state.step}
        >
          {renderStep()}
        </div>
      </div>
      {/* Step indicator dots */}
      <div className="flex justify-center gap-2 py-4">
        {Array.from({ length: TOTAL_STEPS }, (_, i) => (
          <div
            key={i}
            className={`w-2 h-2 rounded-full transition-colors ${
              i + 1 === state.step
                ? "bg-[var(--accent)]"
                : i + 1 < state.step
                ? "bg-[var(--accent)] opacity-40"
                : "bg-[var(--border)]"
            }`}
          />
        ))}
      </div>
    </div>
  );
}

// ─── Step 1: Welcome ────────────────────────────────────────────────────────

function WelcomeStep({ onNext }: { onNext: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center min-h-[80vh] px-8 text-center">
      {/* App icon placeholder */}
      <div className="w-20 h-20 rounded-2xl bg-gradient-to-br from-teal-500 to-teal-700 flex items-center justify-center mb-6 shadow-lg">
        <svg
          className="w-10 h-10 text-white"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={2}
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M12 18.75a6 6 0 006-6v-1.5m-6 7.5a6 6 0 01-6-6v-1.5m6 7.5v3.75m-3.75 0h7.5M12 15.75a3 3 0 01-3-3V4.5a3 3 0 116 0v8.25a3 3 0 01-3 3z"
          />
        </svg>
      </div>

      <h1 className="text-3xl font-bold text-[var(--text-primary)] mb-2">
        Chamgei
      </h1>
      <p className="text-lg text-[var(--text-secondary)] mb-8">
        Privacy-first voice dictation
      </p>

      <button
        onClick={onNext}
        className="px-8 py-3 text-base font-semibold rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer shadow-md"
      >
        Get Started
      </button>

      <span className="mt-4 text-xs text-[var(--text-secondary)] bg-[var(--bg-card)] border border-[var(--border)] rounded-full px-3 py-1">
        No account needed
      </span>
    </div>
  );
}

// ─── Step 2: STT Engine ─────────────────────────────────────────────────────

interface StepProps {
  state: OnboardingState;
  update: <K extends keyof OnboardingState>(
    key: K,
    value: OnboardingState[K]
  ) => void;
  onNext: () => void;
  onBack: () => void;
}

function SttStep({ state, update, onNext, onBack }: StepProps) {
  const engines: {
    id: SttEngine;
    name: string;
    icon: string;
    desc: string;
    note: string;
    recommended?: boolean;
  }[] = [
    {
      id: "local",
      name: "Local Whisper",
      icon: "shield",
      desc: "Private — audio stays on your Mac",
      note: "Requires ~75MB download",
    },
    {
      id: "groq",
      name: "Groq Cloud",
      icon: "lightning",
      desc: "Fastest & most accurate",
      note: "Audio sent to Groq",
      recommended: true,
    },
    {
      id: "deepgram",
      name: "Deepgram",
      icon: "target",
      desc: "Most accurate",
      note: "Audio sent to Deepgram",
    },
  ];

  const iconMap: Record<string, React.ReactNode> = {
    shield: (
      <svg
        className="w-6 h-6"
        fill="none"
        viewBox="0 0 24 24"
        stroke="currentColor"
        strokeWidth={2}
      >
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z"
        />
      </svg>
    ),
    lightning: (
      <svg
        className="w-6 h-6"
        fill="none"
        viewBox="0 0 24 24"
        stroke="currentColor"
        strokeWidth={2}
      >
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          d="M3.75 13.5l10.5-11.25L12 10.5h8.25L9.75 21.75 12 13.5H3.75z"
        />
      </svg>
    ),
    target: (
      <svg
        className="w-6 h-6"
        fill="none"
        viewBox="0 0 24 24"
        stroke="currentColor"
        strokeWidth={2}
      >
        <circle cx="12" cy="12" r="10" />
        <circle cx="12" cy="12" r="6" />
        <circle cx="12" cy="12" r="2" />
      </svg>
    ),
  };

  return (
    <div className="px-8 py-6 max-w-lg mx-auto">
      <button
        onClick={onBack}
        className="text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4"
      >
        &larr; Back
      </button>
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">
        Speech-to-Text Engine
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-5">
        Choose how your voice is transcribed
      </p>

      <div className="grid grid-cols-3 gap-3 mb-5">
        {engines.map((eng) => {
          const selected = state.sttEngine === eng.id;
          return (
            <button
              key={eng.id}
              onClick={() => update("sttEngine", eng.id)}
              className={`relative p-4 rounded-lg border text-left transition-all cursor-pointer ${
                selected
                  ? "border-[var(--accent)] bg-[var(--bg-card)]"
                  : "border-[var(--border)] bg-[var(--bg-card)] hover:border-[var(--accent)] opacity-70 hover:opacity-100"
              }`}
            >
              {eng.recommended && (
                <span className="absolute -top-2 left-1/2 -translate-x-1/2 text-[10px] bg-[var(--accent)] text-white px-2 py-0.5 rounded-full whitespace-nowrap">
                  Recommended
                </span>
              )}
              {selected && (
                <span className="absolute top-2 right-2 w-5 h-5 rounded-full bg-[var(--accent)] flex items-center justify-center">
                  <svg
                    className="w-3 h-3 text-white"
                    fill="none"
                    viewBox="0 0 24 24"
                    stroke="currentColor"
                    strokeWidth={3}
                  >
                    <path
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      d="M4.5 12.75l6 6 9-13.5"
                    />
                  </svg>
                </span>
              )}
              <div className="text-[var(--accent)] mb-2">
                {iconMap[eng.icon]}
              </div>
              <div className="text-sm font-semibold text-[var(--text-primary)] mb-1">
                {eng.name}
              </div>
              <div className="text-xs text-[var(--text-secondary)] mb-1">
                {eng.desc}
              </div>
              <div className="text-[10px] text-[var(--text-secondary)] opacity-60">
                {eng.note}
              </div>
            </button>
          );
        })}
      </div>

      {/* Cloud API key input */}
      {(state.sttEngine === "groq" || state.sttEngine === "deepgram") && (
        <div className="mb-5">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            {state.sttEngine === "groq" ? "Groq" : "Deepgram"} API Key
          </label>
          <input
            type="password"
            value={state.sttApiKey}
            onChange={(e) => update("sttApiKey", e.target.value)}
            placeholder={
              state.sttEngine === "groq" ? "gsk_..." : "dg_..."
            }
            className="w-full px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      )}

      {/* Local model selector */}
      {state.sttEngine === "local" && (
        <div className="mb-5">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            Whisper Model Size
          </label>
          <div className="grid grid-cols-4 gap-2">
            {(
              ["tiny", "small", "medium", "large"] as const
            ).map((size) => (
              <button
                key={size}
                onClick={() => update("whisperModel", size)}
                className={`px-3 py-2 text-sm rounded-lg border transition-colors cursor-pointer capitalize ${
                  state.whisperModel === size
                    ? "bg-[var(--accent)] border-[var(--accent)] text-white"
                    : "bg-[var(--input-bg)] border-[var(--border)] text-[var(--text-secondary)] hover:border-[var(--accent)]"
                }`}
              >
                {size}
              </button>
            ))}
          </div>
          <p className="mt-1.5 text-xs text-[var(--text-secondary)]">
            Smaller models are faster but less accurate.
          </p>
        </div>
      )}

      <button
        onClick={onNext}
        className="w-full py-2.5 rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white font-semibold transition-colors cursor-pointer"
      >
        Continue
      </button>
    </div>
  );
}

// ─── Step 3: LLM Provider ───────────────────────────────────────────────────

const LLM_PROVIDERS = [
  {
    id: "groq",
    name: "Groq",
    desc: "Fast cloud inference",
    defaultModel: "llama-3.3-70b-versatile",
    recommended: true,
    isLocal: false,
  },
  {
    id: "ollama",
    name: "Ollama",
    desc: "Local, free, private",
    defaultModel: "",
    recommended: false,
    isLocal: true,
  },
  {
    id: "openai",
    name: "OpenAI",
    desc: "GPT-4o & more",
    defaultModel: "gpt-4o-mini",
    recommended: false,
    isLocal: false,
  },
  {
    id: "anthropic",
    name: "Anthropic",
    desc: "Claude models",
    defaultModel: "claude-sonnet-4-20250514",
    recommended: false,
    isLocal: false,
  },
  {
    id: "gemini",
    name: "Gemini",
    desc: "Google AI models",
    defaultModel: "gemini-2.0-flash",
    recommended: false,
    isLocal: false,
  },
  {
    id: "cerebras",
    name: "Cerebras",
    desc: "Ultra-fast inference",
    defaultModel: "llama-3.3-70b",
    recommended: false,
    isLocal: false,
  },
];

function LlmStep({ state, update, onNext, onBack }: StepProps) {
  const [ollamaModels, setOllamaModels] = useState<string[]>([]);

  const provider = LLM_PROVIDERS.find((p) => p.id === state.llmProvider);

  useEffect(() => {
    if (state.llmProvider === "ollama") {
      invoke<string>("list_ollama_models")
        .then((raw) => {
          try {
            const parsed = JSON.parse(raw);
            if (Array.isArray(parsed)) {
              setOllamaModels(parsed);
              if (parsed.length > 0 && !state.llmModel) {
                update("llmModel", parsed[0]);
              }
            }
          } catch {
            setOllamaModels([]);
          }
        })
        .catch(() => setOllamaModels([]));
    }
  }, [state.llmProvider, state.llmModel, update]);

  const selectProvider = (id: string) => {
    const p = LLM_PROVIDERS.find((pr) => pr.id === id);
    update("llmProvider", id);
    update("llmApiKey", "");
    if (p) update("llmModel", p.defaultModel);
  };

  return (
    <div className="px-8 py-6 max-w-lg mx-auto">
      <button
        onClick={onBack}
        className="text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4"
      >
        &larr; Back
      </button>
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">
        LLM Provider
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-5">
        Choose an LLM to clean up your transcriptions
      </p>

      <div className="grid grid-cols-2 gap-3 mb-5">
        {LLM_PROVIDERS.map((p) => {
          const selected = state.llmProvider === p.id;
          return (
            <button
              key={p.id}
              onClick={() => selectProvider(p.id)}
              className={`relative p-3 rounded-lg border text-left transition-all cursor-pointer ${
                selected
                  ? "border-[var(--accent)] bg-[var(--bg-card)]"
                  : "border-[var(--border)] bg-[var(--bg-card)] hover:border-[var(--accent)] opacity-70 hover:opacity-100"
              }`}
            >
              {p.recommended && (
                <span className="absolute -top-2 right-2 text-[10px] bg-[var(--accent)] text-white px-2 py-0.5 rounded-full">
                  Recommended
                </span>
              )}
              {selected && (
                <span className="absolute top-2 right-2 w-4 h-4 rounded-full bg-[var(--accent)] flex items-center justify-center">
                  <svg
                    className="w-2.5 h-2.5 text-white"
                    fill="none"
                    viewBox="0 0 24 24"
                    stroke="currentColor"
                    strokeWidth={3}
                  >
                    <path
                      strokeLinecap="round"
                      strokeLinejoin="round"
                      d="M4.5 12.75l6 6 9-13.5"
                    />
                  </svg>
                </span>
              )}
              <div className="w-8 h-8 rounded bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-xs font-bold text-[var(--accent)] mb-2">
                {p.name[0]}
              </div>
              <div className="text-sm font-semibold text-[var(--text-primary)]">
                {p.name}
              </div>
              <div className="text-xs text-[var(--text-secondary)]">
                {p.desc}
              </div>
              {p.isLocal && (
                <span className="mt-1 inline-block text-[10px] text-green-400 bg-green-400/10 px-1.5 py-0.5 rounded">
                  Local &middot; Free
                </span>
              )}
            </button>
          );
        })}
      </div>

      {/* API key for cloud providers */}
      {provider && !provider.isLocal && (
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            {provider.name} API Key
          </label>
          <input
            type="password"
            value={state.llmApiKey}
            onChange={(e) => update("llmApiKey", e.target.value)}
            placeholder="Enter your API key..."
            className="w-full px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      )}

      {/* Model input or Ollama dropdown */}
      {state.llmProvider === "ollama" ? (
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            Ollama Model
          </label>
          {ollamaModels.length > 0 ? (
            <select
              value={state.llmModel}
              onChange={(e) => update("llmModel", e.target.value)}
              className="w-full px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] cursor-pointer"
            >
              {ollamaModels.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          ) : (
            <p className="text-xs text-[var(--text-secondary)]">
              No Ollama models found. Make sure Ollama is running.
            </p>
          )}
        </div>
      ) : (
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            Model Name
          </label>
          <input
            type="text"
            value={state.llmModel}
            onChange={(e) => update("llmModel", e.target.value)}
            className="w-full px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      )}

      <button
        onClick={onNext}
        className="w-full py-2.5 rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white font-semibold transition-colors cursor-pointer mb-3"
      >
        Continue
      </button>

      <button
        onClick={() => {
          update("llmProvider", "none");
          update("llmApiKey", "");
          update("llmModel", "");
          onNext();
        }}
        className="w-full text-center text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer py-1"
      >
        Skip — no LLM cleanup
      </button>
    </div>
  );
}

// ─── Step 4: Permissions ────────────────────────────────────────────────────

function PermissionsStep({ state, update, onNext, onBack }: StepProps) {
  // Poll permissions every 2 seconds
  useEffect(() => {
    const poll = setInterval(() => {
      invoke<{ mic: boolean; accessibility: boolean }>("check_permissions")
        .then((perms) => {
          update("micGranted", perms.mic);
          update("accessibilityGranted", perms.accessibility);
        })
        .catch(() => {});
    }, 2000);

    // Check immediately on mount
    invoke<{ mic: boolean; accessibility: boolean }>("check_permissions")
      .then((perms) => {
        update("micGranted", perms.mic);
        update("accessibilityGranted", perms.accessibility);
      })
      .catch(() => {});

    return () => clearInterval(poll);
  }, [update]);

  const bothGranted = state.micGranted && state.accessibilityGranted;

  return (
    <div className="px-8 py-6 max-w-lg mx-auto">
      <button
        onClick={onBack}
        className="text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4"
      >
        &larr; Back
      </button>
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">
        Permissions
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-5">
        Chamgei needs two macOS permissions to work
      </p>

      <div className="space-y-3 mb-6">
        {/* Microphone */}
        <div className="p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--border)]">
          <div className="flex items-start gap-3">
            <div className="w-10 h-10 rounded-lg bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-[var(--accent)] shrink-0">
              <svg
                className="w-5 h-5"
                fill="none"
                viewBox="0 0 24 24"
                stroke="currentColor"
                strokeWidth={2}
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M12 18.75a6 6 0 006-6v-1.5m-6 7.5a6 6 0 01-6-6v-1.5m6 7.5v3.75m-3.75 0h7.5M12 15.75a3 3 0 01-3-3V4.5a3 3 0 116 0v8.25a3 3 0 01-3 3z"
                />
              </svg>
            </div>
            <div className="flex-1">
              <div className="flex items-center gap-2 mb-1">
                <span className="text-sm font-semibold text-[var(--text-primary)]">
                  Microphone
                </span>
                <StatusBadge granted={state.micGranted} />
              </div>
              <p className="text-xs text-[var(--text-secondary)] mb-2">
                Required to capture your voice
              </p>
              {!state.micGranted && (
                <button
                  onClick={() =>
                    invoke("open_mic_settings").catch(() => {})
                  }
                  className="px-3 py-1.5 text-xs rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer"
                >
                  Grant Access
                </button>
              )}
            </div>
          </div>
        </div>

        {/* Accessibility */}
        <div className="p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--border)]">
          <div className="flex items-start gap-3">
            <div className="w-10 h-10 rounded-lg bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-[var(--accent)] shrink-0">
              <svg
                className="w-5 h-5"
                fill="none"
                viewBox="0 0 24 24"
                stroke="currentColor"
                strokeWidth={2}
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M6.75 7.5l3 2.25-3 2.25m4.5 0h3m-9 8.25h13.5A2.25 2.25 0 0021 18V6a2.25 2.25 0 00-2.25-2.25H5.25A2.25 2.25 0 003 6v12a2.25 2.25 0 002.25 2.25z"
                />
              </svg>
            </div>
            <div className="flex-1">
              <div className="flex items-center gap-2 mb-1">
                <span className="text-sm font-semibold text-[var(--text-primary)]">
                  Accessibility
                </span>
                <StatusBadge granted={state.accessibilityGranted} />
              </div>
              <p className="text-xs text-[var(--text-secondary)] mb-2">
                Required to type text at your cursor
              </p>
              {!state.accessibilityGranted && (
                <button
                  onClick={() =>
                    invoke("open_accessibility_settings").catch(() => {})
                  }
                  className="px-3 py-1.5 text-xs rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer"
                >
                  Grant Access
                </button>
              )}
            </div>
          </div>
        </div>
      </div>

      <button
        onClick={onNext}
        disabled={!bothGranted}
        className={`w-full py-2.5 rounded-lg font-semibold transition-colors cursor-pointer ${
          bothGranted
            ? "bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white"
            : "bg-[var(--bg-secondary)] text-[var(--text-secondary)] cursor-not-allowed"
        }`}
      >
        Continue
      </button>

      {!bothGranted && (
        <button
          onClick={onNext}
          className="w-full text-center text-xs text-[var(--text-secondary)] opacity-50 hover:opacity-100 cursor-pointer py-2 mt-1"
        >
          Continue anyway
        </button>
      )}
    </div>
  );
}

function StatusBadge({ granted }: { granted: boolean }) {
  if (granted) {
    return (
      <span className="inline-flex items-center gap-1 text-[10px] text-green-400 bg-green-400/10 px-1.5 py-0.5 rounded-full">
        <svg
          className="w-3 h-3"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={3}
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M4.5 12.75l6 6 9-13.5"
          />
        </svg>
        Granted
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 text-[10px] text-yellow-400 bg-yellow-400/10 px-1.5 py-0.5 rounded-full">
      <svg
        className="w-3 h-3"
        fill="none"
        viewBox="0 0 24 24"
        stroke="currentColor"
        strokeWidth={2}
      >
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z"
        />
      </svg>
      Not granted
    </span>
  );
}

// ─── Step 5: Mic Test ───────────────────────────────────────────────────────

function MicTestStep({ state: _state, update, onNext, onBack }: StepProps) {
  const [levels, setLevels] = useState<number[]>(new Array(24).fill(0));
  const [detected, setDetected] = useState(false);
  const detectedTimeRef = useRef(0);

  useEffect(() => {
    const interval = setInterval(() => {
      invoke<number>("get_audio_level")
        .then((level) => {
          setLevels((prev) => {
            const next = [...prev.slice(1), level];
            return next;
          });

          if (level > 0.05) {
            detectedTimeRef.current += 100;
            if (detectedTimeRef.current >= 500 && !detected) {
              setDetected(true);
              update("micWorking", true);
            }
          } else {
            detectedTimeRef.current = 0;
          }
        })
        .catch(() => {});
    }, 100);

    return () => clearInterval(interval);
  }, [detected, update]);

  return (
    <div className="px-8 py-6 max-w-lg mx-auto">
      <button
        onClick={onBack}
        className="text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4"
      >
        &larr; Back
      </button>
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">
        Mic Test
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-6">
        Speak something to test your microphone
      </p>

      {/* Audio visualizer */}
      <div className="flex items-end justify-center gap-1 h-32 mb-6 p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--border)]">
        {levels.map((level, i) => (
          <div
            key={i}
            className="w-2 rounded-full bg-[var(--accent)] transition-all duration-100"
            style={{
              height: `${Math.max(4, level * 100)}%`,
              opacity: 0.4 + level * 0.6,
            }}
          />
        ))}
      </div>

      {detected ? (
        <div className="mb-6 p-3 rounded-lg bg-green-400/10 border border-green-400/30 text-center">
          <span className="text-sm font-semibold text-green-400">
            Microphone working!
          </span>
        </div>
      ) : (
        <p className="text-center text-xs text-[var(--text-secondary)] mb-6">
          Waiting for audio input...
        </p>
      )}

      <button
        onClick={onNext}
        className="w-full py-2.5 rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white font-semibold transition-colors cursor-pointer"
      >
        Continue
      </button>
    </div>
  );
}

// ─── Step 6: Try It ─────────────────────────────────────────────────────────

function TryItStep({ state, update, onNext, onBack }: StepProps) {
  const [phase, setPhase] = useState<
    "idle" | "recording" | "processing" | "done"
  >("idle");
  const [result, setResult] = useState("");

  // Poll pipeline status to detect recording/processing/done
  useEffect(() => {
    if (phase === "idle" || phase === "done") return;

    const interval = setInterval(() => {
      invoke<string>("get_pipeline_status")
        .then((status) => {
          if (status === "recording" && phase !== "recording") {
            setPhase("recording");
          } else if (status === "processing" && phase !== "processing") {
            setPhase("processing");
          } else if (status.startsWith("done:")) {
            setResult(status.slice(5));
            setPhase("done");
            update("firstDictationDone", true);
          }
        })
        .catch(() => {});
    }, 200);

    return () => clearInterval(interval);
  }, [phase, update]);

  // Start listening for pipeline status when user is supposed to try
  useEffect(() => {
    if (phase !== "idle") return;

    const interval = setInterval(() => {
      invoke<string>("get_pipeline_status")
        .then((status) => {
          if (status === "recording") {
            setPhase("recording");
          }
        })
        .catch(() => {});
    }, 200);

    return () => clearInterval(interval);
  }, [phase]);

  return (
    <div className="px-8 py-6 max-w-lg mx-auto flex flex-col items-center min-h-[70vh] justify-center">
      <button
        onClick={onBack}
        className="self-start text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4"
      >
        &larr; Back
      </button>

      {phase === "idle" && (
        <>
          <h2 className="text-2xl font-bold text-[var(--text-primary)] mb-2 text-center">
            Hold Fn and speak
          </h2>
          <p className="text-sm text-[var(--text-secondary)] mb-8 text-center">
            Try your first dictation
          </p>
          {/* Pulsing Fn key */}
          <div className="animate-pulse-ring w-20 h-20 rounded-xl bg-[var(--bg-card)] border-2 border-[var(--accent)] flex items-center justify-center mb-6">
            <span className="text-2xl font-bold text-[var(--accent)]">Fn</span>
          </div>
        </>
      )}

      {phase === "recording" && (
        <>
          <div className="flex items-center gap-2 mb-4">
            <div className="w-3 h-3 rounded-full bg-red-500 animate-pulse-ring" />
            <span className="text-lg font-semibold text-red-400">
              Recording...
            </span>
          </div>
          <p className="text-sm text-[var(--text-secondary)]">
            Release Fn when you're done speaking
          </p>
        </>
      )}

      {phase === "processing" && (
        <>
          <div className="flex items-center gap-2 mb-4">
            <svg
              className="w-5 h-5 text-[var(--accent)] animate-spin"
              fill="none"
              viewBox="0 0 24 24"
            >
              <circle
                className="opacity-25"
                cx="12"
                cy="12"
                r="10"
                stroke="currentColor"
                strokeWidth="4"
              />
              <path
                className="opacity-75"
                fill="currentColor"
                d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
              />
            </svg>
            <span className="text-lg font-semibold text-[var(--text-primary)]">
              Processing...
            </span>
          </div>
        </>
      )}

      {phase === "done" && (
        <div className="w-full animate-confetti-pop">
          <div className="text-center mb-4">
            <span className="inline-flex items-center justify-center w-12 h-12 rounded-full bg-green-400/20 mb-3">
              <svg
                className="w-6 h-6 text-green-400"
                fill="none"
                viewBox="0 0 24 24"
                stroke="currentColor"
                strokeWidth={3}
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  d="M4.5 12.75l6 6 9-13.5"
                />
              </svg>
            </span>
            <p className="text-lg font-bold text-[var(--text-primary)]">
              Dictation works!
            </p>
          </div>
          <div className="p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--accent)] text-sm text-[var(--text-primary)] leading-relaxed">
            {result}
          </div>
        </div>
      )}

      {(phase === "done" || state.firstDictationDone) && (
        <button
          onClick={onNext}
          className="w-full mt-6 py-2.5 rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white font-semibold transition-colors cursor-pointer"
        >
          Continue
        </button>
      )}

      {phase === "idle" && (
        <button
          onClick={onNext}
          className="mt-8 text-xs text-[var(--text-secondary)] opacity-50 hover:opacity-100 cursor-pointer"
        >
          Skip for now
        </button>
      )}
    </div>
  );
}

// ─── Step 7: All Set ────────────────────────────────────────────────────────

function AllSetStep({
  state,
  onComplete,
  onBack,
}: {
  state: OnboardingState;
  onComplete: () => void;
  onBack: () => void;
}) {
  const handleFinish = async () => {
    const config = JSON.stringify({
      stt_engine: state.sttEngine,
      stt_api_key: state.sttApiKey,
      whisper_model: state.whisperModel,
      llm_provider: state.llmProvider,
      llm_api_key: state.llmApiKey,
      llm_model: state.llmModel,
    });

    try {
      await invoke("save_config", { config });
    } catch {
      // Config save failed, proceed anyway
    }
    onComplete();
  };

  const sttLabel =
    state.sttEngine === "local"
      ? `Local Whisper (${state.whisperModel})`
      : state.sttEngine === "groq"
      ? "Groq Cloud"
      : "Deepgram";

  const llmLabel =
    state.llmProvider === "none"
      ? "None (raw transcription)"
      : `${state.llmProvider}${state.llmModel ? ` / ${state.llmModel}` : ""}`;

  return (
    <div className="px-8 py-6 max-w-lg mx-auto flex flex-col items-center min-h-[70vh] justify-center">
      <button
        onClick={onBack}
        className="self-start text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4"
      >
        &larr; Back
      </button>

      {/* Success icon */}
      <div className="w-16 h-16 rounded-full bg-green-400/20 flex items-center justify-center mb-4">
        <svg
          className="w-8 h-8 text-green-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth={3}
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            d="M4.5 12.75l6 6 9-13.5"
          />
        </svg>
      </div>

      <h2 className="text-2xl font-bold text-[var(--text-primary)] mb-1">
        All Set!
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-6">
        Chamgei is ready to go
      </p>

      {/* Config summary */}
      <div className="w-full p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--border)] mb-6 text-sm space-y-2">
        <div className="flex justify-between">
          <span className="text-[var(--text-secondary)]">STT</span>
          <span className="text-[var(--text-primary)] font-medium">
            {sttLabel}
          </span>
        </div>
        <div className="flex justify-between">
          <span className="text-[var(--text-secondary)]">LLM</span>
          <span className="text-[var(--text-primary)] font-medium">
            {llmLabel}
          </span>
        </div>
        <div className="border-t border-[var(--border)] pt-2 mt-2 space-y-1">
          <div className="flex justify-between text-xs">
            <span className="text-[var(--text-secondary)]">Dictate</span>
            <kbd className="px-1.5 py-0.5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--accent)] font-mono">
              Fn
            </kbd>
          </div>
          <div className="flex justify-between text-xs">
            <span className="text-[var(--text-secondary)]">Toggle</span>
            <kbd className="px-1.5 py-0.5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--accent)] font-mono">
              Fn + Space
            </kbd>
          </div>
          <div className="flex justify-between text-xs">
            <span className="text-[var(--text-secondary)]">Command</span>
            <kbd className="px-1.5 py-0.5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--accent)] font-mono">
              Fn + Enter
            </kbd>
          </div>
        </div>
      </div>

      <p className="text-xs text-[var(--text-secondary)] text-center mb-1">
        Chamgei will run in your menu bar.
      </p>
      <p className="text-xs text-[var(--text-secondary)] text-center mb-6">
        Open Settings anytime from the menu bar icon.
      </p>

      <button
        onClick={handleFinish}
        className="w-full py-3 rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white font-semibold text-base transition-colors cursor-pointer shadow-md"
      >
        Start Chamgei
      </button>
    </div>
  );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Settings App (existing tabs for returning users)
// ═══════════════════════════════════════════════════════════════════════════════

function SettingsApp() {
  const [activeTab, setActiveTab] = useState<Tab>("general");
  const [settings, setSettings] = useState<Settings>(defaultSettings);

  const tabs: { id: Tab; label: string }[] = [
    { id: "general", label: "General" },
    { id: "inference", label: "Inference" },
    { id: "audio", label: "Audio" },
    { id: "hotkeys", label: "Hotkeys" },
    { id: "history", label: "History" },
  ];

  const update = <K extends keyof Settings>(key: K, value: Settings[K]) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
  };

  return (
    <div className="flex flex-col h-screen select-none">
      {/* Header */}
      <header className="flex items-center gap-2 px-5 py-3 bg-[var(--bg-secondary)] border-b border-[var(--border)]">
        <span className="text-lg font-bold tracking-wide text-[var(--accent)]">
          Chamgei
        </span>
        <span className="text-xs text-[var(--text-secondary)]">v0.1.0</span>
      </header>

      {/* Tabs */}
      <nav className="flex gap-0 bg-[var(--bg-secondary)] border-b border-[var(--border)]">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`px-5 py-2.5 text-sm font-medium transition-colors cursor-pointer border-b-2 ${
              activeTab === tab.id
                ? "border-[var(--accent)] text-[var(--accent)]"
                : "border-transparent text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
            }`}
          >
            {tab.label}
          </button>
        ))}
      </nav>

      {/* Content */}
      <main className="flex-1 overflow-y-auto p-5">
        {activeTab === "general" && (
          <GeneralTab settings={settings} update={update} />
        )}
        {activeTab === "inference" && (
          <InferenceTab settings={settings} update={update} />
        )}
        {activeTab === "audio" && (
          <AudioTab settings={settings} update={update} />
        )}
        {activeTab === "hotkeys" && <HotkeysTab />}
        {activeTab === "history" && <HistoryTab />}
      </main>
    </div>
  );
}

// ─── Tab Components ─────────────────────────────────────────────────────────

interface TabProps {
  settings: Settings;
  update: <K extends keyof Settings>(key: K, value: Settings[K]) => void;
}

function GeneralTab({ settings, update }: TabProps) {
  return (
    <div className="space-y-5">
      <Section title="Activation Mode">
        <div className="flex gap-3">
          <ToggleButton
            active={settings.activationMode === "push_to_talk"}
            onClick={() => update("activationMode", "push_to_talk")}
          >
            Push to Talk
          </ToggleButton>
          <ToggleButton
            active={settings.activationMode === "toggle"}
            onClick={() => update("activationMode", "toggle")}
          >
            Toggle
          </ToggleButton>
        </div>
        <p className="mt-2 text-xs text-[var(--text-secondary)]">
          {settings.activationMode === "push_to_talk"
            ? "Hold the hotkey to record, release to stop."
            : "Press once to start recording, press again to stop."}
        </p>
      </Section>

      <Section title="Text Injection">
        <Select
          value={settings.injectionMethod}
          onChange={(v) =>
            update("injectionMethod", v as Settings["injectionMethod"])
          }
          options={[
            { value: "clipboard", label: "Clipboard (paste)" },
            { value: "native", label: "Native (keystroke sim)" },
          ]}
        />
      </Section>
    </div>
  );
}

// ─── Keychain-backed API Key Field ───────────────────────────────────────────

function ApiKeyField({ provider, placeholder }: { provider: string; placeholder: string }) {
  const [masked, setMasked] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);
  const [testResult, setTestResult] = useState<"idle" | "testing" | "valid" | "invalid">("idle");

  const refresh = useCallback(async () => {
    try {
      const m = await invoke<string>("get_api_key_masked", { provider });
      setMasked(m);
      setEditing(false);
    } catch {
      setMasked(null);
    }
  }, [provider]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleSave = async () => {
    if (!value.trim()) return;
    setSaving(true);
    try {
      await invoke("save_api_key", { provider, key: value.trim() });
      setValue("");
      setTestResult("idle");
      await refresh();
    } catch (e) {
      console.error("Failed to save key:", e);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    try {
      await invoke("delete_api_key", { provider });
      setMasked(null);
      setEditing(false);
      setTestResult("idle");
    } catch {
      // ignore
    }
  };

  const handleTest = async () => {
    setTestResult("testing");
    try {
      await invoke<string>("test_api_key", { provider });
      setTestResult("valid");
    } catch {
      setTestResult("invalid");
    }
  };

  // State B: key exists
  if (masked && !editing) {
    return (
      <div className="flex items-center gap-2">
        <code className="flex-1 px-3 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-secondary)] font-mono">
          {masked}
        </code>
        <span className="text-xs text-green-400 font-medium whitespace-nowrap">Configured &#10003;</span>
        <button
          onClick={handleTest}
          disabled={testResult === "testing"}
          className="px-3 py-1.5 rounded text-xs font-medium bg-[var(--input-bg)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors cursor-pointer disabled:opacity-50"
        >
          {testResult === "testing" ? "..." : testResult === "valid" ? "Valid !" : testResult === "invalid" ? "Invalid" : "Test"}
        </button>
        <button
          onClick={() => { setEditing(true); setValue(""); setTestResult("idle"); }}
          className="px-3 py-1.5 rounded text-xs font-medium bg-[var(--input-bg)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors cursor-pointer"
        >
          Change
        </button>
        <button
          onClick={handleDelete}
          className="px-3 py-1.5 rounded text-xs font-medium bg-[var(--input-bg)] border border-red-500/30 text-red-400 hover:border-red-500 transition-colors cursor-pointer"
        >
          Delete
        </button>
      </div>
    );
  }

  // State A: no key or editing
  return (
    <div className="flex items-center gap-2">
      <input
        type="password"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        placeholder={placeholder}
        className="flex-1 px-3 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
        onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
      />
      <button
        onClick={handleSave}
        disabled={saving || !value.trim()}
        className="px-4 py-2 rounded text-xs font-semibold bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer disabled:opacity-50"
      >
        {saving ? "..." : "Save"}
      </button>
      {masked && (
        <button
          onClick={() => { setEditing(false); setTestResult("idle"); }}
          className="px-3 py-2 rounded text-xs font-medium bg-[var(--input-bg)] border border-[var(--border)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors cursor-pointer"
        >
          Cancel
        </button>
      )}
    </div>
  );
}

function InferenceTab({ settings, update }: TabProps) {
  return (
    <div className="space-y-5">
      <Section title="Speech-to-Text Engine">
        <Select
          value={settings.sttEngine}
          onChange={(v) => update("sttEngine", v as Settings["sttEngine"])}
          options={[
            { value: "local", label: "Local Whisper — private, audio stays on device" },
            { value: "groq", label: "Groq Cloud Whisper — fastest, most accurate" },
            { value: "deepgram", label: "Deepgram Nova-2 — most accurate" },
          ]}
        />
      </Section>

      {settings.sttEngine === "deepgram" && (
        <Section title="Deepgram API Key">
          <ApiKeyField provider="deepgram" placeholder="dg_..." />
        </Section>
      )}

      <Section title="LLM Provider (text cleanup)">
        <Select
          value={settings.llmProvider}
          onChange={(v) => update("llmProvider", v as Settings["llmProvider"])}
          options={[
            { value: "groq", label: "Groq" },
            { value: "cerebras", label: "Cerebras" },
            { value: "local", label: "None (raw transcription)" },
          ]}
        />
      </Section>

      {settings.llmProvider === "cerebras" && (
        <Section title="Cerebras API Key">
          <ApiKeyField provider="cerebras" placeholder="csk-..." />
        </Section>
      )}

      {(settings.llmProvider === "groq" || settings.sttEngine === "groq") && (
        <Section title="Groq API Key">
          <ApiKeyField provider="groq" placeholder="gsk_..." />
        </Section>
      )}

      {settings.sttEngine === "local" && (
        <Section title="Local Model">
          <Select
            value={settings.whisperModel}
            onChange={(v) =>
              update("whisperModel", v as Settings["whisperModel"])
            }
            options={[
              { value: "tiny", label: "Whisper Tiny (75 MB) — fastest" },
              { value: "small", label: "Whisper Small (250 MB) — balanced" },
              { value: "medium", label: "Whisper Medium (750 MB) — accurate" },
              { value: "large", label: "Whisper Large (1.5 GB) — most accurate" },
            ]}
          />
          <p className="mt-2 text-xs text-[var(--text-secondary)]">
            Runs on-device with Metal GPU. Audio never leaves your Mac.
          </p>
        </Section>
      )}
    </div>
  );
}

function AudioTab({ settings, update }: TabProps) {
  return (
    <div className="space-y-5">
      <Section title="VAD Sensitivity">
        <div className="flex items-center gap-3">
          <input
            type="range"
            min="0"
            max="1"
            step="0.05"
            value={settings.vadThreshold}
            onChange={(e) => update("vadThreshold", parseFloat(e.target.value))}
            className="flex-1 accent-[var(--accent)]"
          />
          <span className="text-sm font-mono w-10 text-right">
            {settings.vadThreshold.toFixed(2)}
          </span>
        </div>
        <p className="mt-1 text-xs text-[var(--text-secondary)]">
          Lower = more sensitive (picks up quieter speech). Higher = less
          sensitive (ignores background noise).
        </p>
      </Section>
    </div>
  );
}

function HotkeysTab() {
  return (
    <div className="space-y-5">
      <Section title="Dictation Hotkey">
        <div className="flex items-center gap-3">
          <kbd className="px-4 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm font-mono text-[var(--accent)]">
            Ctrl + Shift + Space
          </kbd>
          <button className="px-3 py-1.5 text-xs rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors cursor-pointer">
            Change
          </button>
        </div>
        <p className="mt-2 text-xs text-[var(--text-secondary)]">
          Hotkey customization coming soon.
        </p>
      </Section>
    </div>
  );
}

function formatRelativeTime(timestamp: string): string {
  const now = Date.now();
  const then = new Date(timestamp).getTime();
  const diffMs = now - then;
  const diffSec = Math.floor(diffMs / 1000);
  const diffMin = Math.floor(diffSec / 60);
  const diffHr = Math.floor(diffMin / 60);
  const diffDay = Math.floor(diffHr / 24);

  if (diffSec < 60) return "just now";
  if (diffMin < 60) return `${diffMin} min ago`;
  if (diffHr < 24) return `${diffHr} hr ago`;
  if (diffDay < 7) return `${diffDay}d ago`;
  return new Date(timestamp).toLocaleDateString();
}

async function copyToClipboard(text: string) {
  try {
    await invoke("copy_to_clipboard", { text });
  } catch {
    await navigator.clipboard.writeText(text);
  }
}

function HistoryTab() {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [search, setSearch] = useState("");
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);

  const loadHistory = useCallback(async () => {
    setLoading(true);
    try {
      const raw = await invoke<string>("get_history");
      const parsed: HistoryEntry[] = JSON.parse(raw);
      parsed.reverse();
      setEntries(parsed);
    } catch {
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadHistory();
  }, [loadHistory]);

  const handleClear = async () => {
    try {
      await invoke("clear_history");
      setEntries([]);
    } catch {
      // ignore
    }
  };

  const handleCopy = async (text: string, idx: number) => {
    await copyToClipboard(text);
    setCopiedIdx(idx);
    setTimeout(() => setCopiedIdx(null), 1500);
  };

  const filtered = search.trim()
    ? entries.filter(
        (e) =>
          e.text.toLowerCase().includes(search.toLowerCase()) ||
          e.provider.toLowerCase().includes(search.toLowerCase()) ||
          e.app_context.toLowerCase().includes(search.toLowerCase())
      )
    : entries;

  if (loading) {
    return (
      <div className="flex items-center justify-center h-48 text-sm text-[var(--text-secondary)]">
        Loading history...
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Toolbar */}
      <div className="flex items-center gap-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search dictations..."
          className="flex-1 px-3 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
        />
        {entries.length > 0 && (
          <button
            onClick={handleClear}
            className="px-3 py-2 text-xs rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-red-400 hover:text-red-300 hover:border-red-400 transition-colors cursor-pointer whitespace-nowrap"
          >
            Clear History
          </button>
        )}
      </div>

      {/* Entries */}
      {filtered.length === 0 ? (
        <div className="flex flex-col items-center justify-center h-48 text-center">
          <p className="text-sm text-[var(--text-secondary)]">
            {entries.length === 0
              ? "No dictations yet. Hold Fn and speak to get started."
              : "No results match your search."}
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          {filtered.map((entry, idx) => (
            <div
              key={idx}
              className="p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--border)] group"
            >
              <div className="flex items-start justify-between gap-3">
                <p className="text-sm text-[var(--text-primary)] leading-relaxed flex-1">
                  {entry.text}
                </p>
                <button
                  onClick={() => handleCopy(entry.text, idx)}
                  className="px-2.5 py-1 text-xs rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--text-secondary)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors cursor-pointer opacity-0 group-hover:opacity-100 shrink-0"
                >
                  {copiedIdx === idx ? "Copied!" : "Copy"}
                </button>
              </div>
              <div className="flex items-center gap-3 mt-2 text-xs text-[var(--text-secondary)]">
                <span>{formatRelativeTime(entry.timestamp)}</span>
                <span className="opacity-40">|</span>
                <span className="opacity-60">
                  STT {entry.stt_ms}ms + LLM {entry.llm_ms}ms
                </span>
                <span className="opacity-40">|</span>
                <span className="opacity-60">{entry.provider}</span>
                {entry.app_context && (
                  <>
                    <span className="opacity-40">|</span>
                    <span className="opacity-60">{entry.app_context}</span>
                  </>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ─── Shared UI Components ───────────────────────────────────────────────────

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="p-4 rounded-lg bg-[var(--bg-card)] border border-[var(--border)]">
      <h3 className="text-sm font-semibold mb-3 text-[var(--text-primary)]">
        {title}
      </h3>
      {children}
    </div>
  );
}

function Select({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (value: string) => void;
  options: { value: string; label: string }[];
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="w-full px-3 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] cursor-pointer"
    >
      {options.map((opt) => (
        <option key={opt.value} value={opt.value}>
          {opt.label}
        </option>
      ))}
    </select>
  );
}

function ToggleButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`px-4 py-2 text-sm rounded border transition-colors cursor-pointer ${
        active
          ? "bg-[var(--accent)] border-[var(--accent)] text-white"
          : "bg-[var(--input-bg)] border-[var(--border)] text-[var(--text-secondary)] hover:border-[var(--accent)]"
      }`}
    >
      {children}
    </button>
  );
}

export default App;
