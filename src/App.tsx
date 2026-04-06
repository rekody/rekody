import React, { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { motion, AnimatePresence } from "motion/react";

// ─── Types ───────────────────────────────────────────────────────────────────

type SettingsPage =
  | "general"
  | "inference"
  | "audio"
  | "hotkeys"
  | "privacy"
  | "history"
  | "about";

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
  llmProvider: string;
  groqApiKey: string;
  cerebrasApiKey: string;
  deepgramApiKey: string;
  whisperModel: "tiny" | "small" | "medium" | "large";
  vadThreshold: number;
  injectionMethod: "clipboard" | "native";
  privacyMode: boolean;
}

const defaultSettings: Settings = {
  activationMode: "push_to_talk",
  sttEngine: "local",
  llmProvider: "groq",
  groqApiKey: "",
  deepgramApiKey: "",
  cerebrasApiKey: "",
  whisperModel: "tiny",
  vadThreshold: 0.01,
  injectionMethod: "clipboard",
  privacyMode: false,
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
  inputMonitoringGranted: boolean;
  micWorking: boolean;
  firstDictationDone: boolean;
}

const TOTAL_STEPS = 7;

// ─── Icons ──────────────────────────────────────────────────────────────────

function IconMic({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 18.75a6 6 0 006-6v-1.5m-6 7.5a6 6 0 01-6-6v-1.5m6 7.5v3.75m-3.75 0h7.5M12 15.75a3 3 0 01-3-3V4.5a3 3 0 116 0v8.25a3 3 0 01-3 3z" />
    </svg>
  );
}

function IconCpu({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M8.25 3v1.5M4.5 8.25H3m18 0h-1.5M4.5 12H3m18 0h-1.5m-15 3.75H3m18 0h-1.5M8.25 19.5V21M12 3v1.5m0 15V21m3.75-18v1.5m0 15V21m-9-1.5h10.5a2.25 2.25 0 002.25-2.25V6.75a2.25 2.25 0 00-2.25-2.25H6.75A2.25 2.25 0 004.5 6.75v10.5a2.25 2.25 0 002.25 2.25z" />
    </svg>
  );
}

function IconAudio({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M19.114 5.636a9 9 0 010 12.728M16.463 8.288a5.25 5.25 0 010 7.424M6.75 8.25l4.72-4.72a.75.75 0 011.28.53v15.88a.75.75 0 01-1.28.53l-4.72-4.72H4.51c-.88 0-1.704-.507-1.938-1.354A9.01 9.01 0 012.25 12c0-.83.112-1.633.322-2.396C2.806 8.756 3.63 8.25 4.51 8.25H6.75z" />
    </svg>
  );
}

function IconKeyboard({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M6.75 7.5l3 2.25-3 2.25m4.5 0h3m-9 8.25h13.5A2.25 2.25 0 0021 18V6a2.25 2.25 0 00-2.25-2.25H5.25A2.25 2.25 0 003 6v12a2.25 2.25 0 002.25 2.25z" />
    </svg>
  );
}

function IconShield({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z" />
    </svg>
  );
}

function IconClock({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 6v6h4.5m4.5 0a9 9 0 11-18 0 9 9 0 0118 0z" />
    </svg>
  );
}

function IconInfo({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M11.25 11.25l.041-.02a.75.75 0 011.063.852l-.708 2.836a.75.75 0 001.063.853l.041-.021M21 12a9 9 0 11-18 0 9 9 0 0118 0zm-9-3.75h.008v.008H12V8.25z" />
    </svg>
  );
}

function IconGear({ className = "w-5 h-5" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.324.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.24-.438.613-.431.992a6.759 6.759 0 010 .255c-.007.378.138.75.43.99l1.005.828c.424.35.534.954.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.57 6.57 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.28c-.09.543-.56.941-1.11.941h-2.594c-.55 0-1.02-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.992a6.932 6.932 0 010-.255c.007-.378-.138-.75-.43-.99l-1.004-.828a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.087.22-.128.332-.183.582-.495.644-.869l.214-1.281z" />
      <path strokeLinecap="round" strokeLinejoin="round" d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
    </svg>
  );
}

function IconCheck({ className = "w-4 h-4" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={3}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M4.5 12.75l6 6 9-13.5" />
    </svg>
  );
}

function IconArrowRight({ className = "w-4 h-4" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M13.5 4.5L21 12m0 0l-7.5 7.5M21 12H3" />
    </svg>
  );
}

function IconLightning({ className = "w-6 h-6" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M3.75 13.5l10.5-11.25L12 10.5h8.25L9.75 21.75 12 13.5H3.75z" />
    </svg>
  );
}

function IconTarget({ className = "w-6 h-6" }: { className?: string }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
      <circle cx="12" cy="12" r="10" />
      <circle cx="12" cy="12" r="6" />
      <circle cx="12" cy="12" r="2" />
    </svg>
  );
}

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
        <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="flex flex-col items-center gap-3">
          <div className="w-8 h-8 rounded-full border-2 border-[var(--accent)] border-t-transparent animate-spin" />
          <span>Loading...</span>
        </motion.div>
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
    sttEngine: "deepgram",
    sttApiKey: "",
    whisperModel: "small",
    llmProvider: "groq",
    llmApiKey: "",
    llmModel: "llama-3.3-70b-versatile",
    micGranted: false,
    accessibilityGranted: false,
    inputMonitoringGranted: false,
    micWorking: false,
    firstDictationDone: false,
  });

  const update = <K extends keyof OnboardingState>(
    key: K,
    value: OnboardingState[K]
  ) => {
    setState((prev) => ({ ...prev, [key]: value }));
  };

  const goTo = (step: number) => update("step", step);
  const next = () => goTo(state.step + 1);
  const back = () => goTo(state.step - 1);

  const renderStep = () => {
    switch (state.step) {
      case 1:
        return <WelcomeStep onNext={next} />;
      case 2:
        return <SttStep state={state} update={update} onNext={next} onBack={back} />;
      case 3:
        return <LlmStep state={state} update={update} onNext={next} onBack={back} />;
      case 4:
        return <PermissionsStep state={state} update={update} onNext={next} onBack={back} />;
      case 5:
        return <MicTestStep state={state} update={update} onNext={next} onBack={back} />;
      case 6:
        return <TryItStep state={state} update={update} onNext={next} onBack={back} />;
      case 7:
        return <AllSetStep state={state} onComplete={onComplete} onBack={back} />;
      default:
        return null;
    }
  };

  return (
    <div className="flex flex-col h-screen select-none bg-[var(--bg-primary)]">
      <div className="flex-1 overflow-y-auto">
        <AnimatePresence mode="wait">
          <motion.div
            key={state.step}
            initial={{ opacity: 0, x: 20 }}
            animate={{ opacity: 1, x: 0 }}
            exit={{ opacity: 0, x: -20 }}
            transition={{ duration: 0.25, ease: "easeInOut" }}
          >
            {renderStep()}
          </motion.div>
        </AnimatePresence>
      </div>

      {/* Step progress bar */}
      <div className="px-8 pb-5 pt-3">
        <div className="flex items-center gap-1.5">
          {Array.from({ length: TOTAL_STEPS }, (_, i) => (
            <div key={i} className="flex-1 h-1 rounded-full overflow-hidden bg-[var(--border)]">
              <motion.div
                className="h-full bg-[var(--accent)]"
                initial={false}
                animate={{ width: i + 1 <= state.step ? "100%" : "0%" }}
                transition={{ duration: 0.3 }}
              />
            </div>
          ))}
        </div>
        <div className="flex justify-between mt-2">
          <span className="text-[10px] text-[var(--text-secondary)]">
            Step {state.step} of {TOTAL_STEPS}
          </span>
          <span className="text-[10px] text-[var(--text-secondary)]">
            {["Welcome", "Speech Engine", "LLM", "Permissions", "Mic Test", "Try It", "Ready"][state.step - 1]}
          </span>
        </div>
      </div>
    </div>
  );
}

// ─── Step 1: Welcome ────────────────────────────────────────────────────────

function WelcomeStep({ onNext }: { onNext: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center min-h-[80vh] px-8 text-center">
      <motion.div
        initial={{ scale: 0.8, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        transition={{ type: "spring", stiffness: 300, damping: 20 }}
        className="w-20 h-20 rounded-2xl bg-gradient-to-br from-teal-500 to-teal-700 flex items-center justify-center mb-6 shadow-lg shadow-teal-500/20"
      >
        <IconMic className="w-10 h-10 text-white" />
      </motion.div>

      <motion.h1
        initial={{ y: 10, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        transition={{ delay: 0.1 }}
        className="text-3xl font-bold text-[var(--text-primary)] mb-2"
      >
        Chamgei
      </motion.h1>
      <motion.p
        initial={{ y: 10, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        transition={{ delay: 0.2 }}
        className="text-lg text-[var(--text-secondary)] mb-8"
      >
        Privacy-first voice dictation
      </motion.p>

      <motion.button
        initial={{ y: 10, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        transition={{ delay: 0.3 }}
        whileHover={{ scale: 1.02 }}
        whileTap={{ scale: 0.98 }}
        onClick={onNext}
        className="px-8 py-3 text-base font-semibold rounded-xl bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer shadow-lg shadow-teal-500/20"
      >
        Get Started
      </motion.button>

      <motion.span
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ delay: 0.5 }}
        className="mt-4 text-xs text-[var(--text-secondary)] bg-[var(--bg-card)] border border-[var(--border)] rounded-full px-3 py-1"
      >
        No account needed
      </motion.span>
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
    icon: React.ReactNode;
    desc: string;
    note: string;
    badge?: string;
  }[] = [
    {
      id: "local",
      name: "Local Whisper",
      icon: <IconShield className="w-6 h-6" />,
      desc: "Audio stays on your device",
      note: "Requires ~75MB download",
      badge: "Private",
    },
    {
      id: "deepgram",
      name: "Deepgram Nova-2",
      icon: <IconTarget className="w-6 h-6" />,
      desc: "Fastest, best punctuation",
      note: "Audio sent to Deepgram",
      badge: "Recommended",
    },
    {
      id: "groq",
      name: "Groq Whisper",
      icon: <IconLightning className="w-6 h-6" />,
      desc: "Fast cloud transcription",
      note: "Audio sent to Groq",
    },
  ];

  return (
    <div className="px-8 py-6 max-w-lg mx-auto">
      <BackButton onClick={onBack} />
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">
        Speech-to-Text Engine
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-5">
        Choose how your voice is transcribed
      </p>

      <div className="space-y-3 mb-5">
        {engines.map((eng) => {
          const selected = state.sttEngine === eng.id;
          return (
            <motion.button
              key={eng.id}
              whileHover={{ scale: 1.01 }}
              whileTap={{ scale: 0.99 }}
              onClick={() => update("sttEngine", eng.id)}
              className={`relative w-full p-4 rounded-xl border text-left transition-all cursor-pointer flex items-start gap-4 ${
                selected
                  ? "border-[var(--accent)] bg-[var(--accent)]/5"
                  : "border-[var(--border)] bg-[var(--bg-card)] hover:border-[var(--accent)]/50"
              }`}
            >
              {eng.badge && (
                <span className={`absolute -top-2 right-3 text-[10px] px-2 py-0.5 rounded-full whitespace-nowrap ${
                  eng.badge === "Recommended"
                    ? "bg-[var(--accent)] text-white"
                    : "bg-green-500/20 text-green-400 border border-green-500/30"
                }`}>
                  {eng.badge}
                </span>
              )}
              <div className={`w-10 h-10 rounded-lg flex items-center justify-center shrink-0 ${
                selected ? "bg-[var(--accent)]/20 text-[var(--accent)]" : "bg-[var(--bg-secondary)] text-[var(--text-secondary)]"
              }`}>
                {eng.icon}
              </div>
              <div className="flex-1 min-w-0">
                <div className="text-sm font-semibold text-[var(--text-primary)]">{eng.name}</div>
                <div className="text-xs text-[var(--text-secondary)]">{eng.desc}</div>
                <div className="text-[10px] text-[var(--text-secondary)] opacity-60 mt-0.5">{eng.note}</div>
              </div>
              {selected && (
                <div className="w-5 h-5 rounded-full bg-[var(--accent)] flex items-center justify-center shrink-0 mt-1">
                  <IconCheck className="w-3 h-3 text-white" />
                </div>
              )}
            </motion.button>
          );
        })}
      </div>

      {/* Cloud API key input */}
      {(state.sttEngine === "groq" || state.sttEngine === "deepgram") && (
        <motion.div initial={{ height: 0, opacity: 0 }} animate={{ height: "auto", opacity: 1 }} className="mb-5">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            {state.sttEngine === "groq" ? "Groq" : "Deepgram"} API Key
          </label>
          <input
            type="password"
            value={state.sttApiKey}
            onChange={(e) => update("sttApiKey", e.target.value)}
            placeholder={state.sttEngine === "groq" ? "gsk_..." : "dg_..."}
            className="w-full px-3 py-2.5 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)] transition-colors"
          />
        </motion.div>
      )}

      {/* Local model selector */}
      {state.sttEngine === "local" && (
        <motion.div initial={{ height: 0, opacity: 0 }} animate={{ height: "auto", opacity: 1 }} className="mb-5">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">
            Whisper Model Size
          </label>
          <div className="grid grid-cols-4 gap-2">
            {(["tiny", "small", "medium", "large"] as const).map((size) => (
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
        </motion.div>
      )}

      <PrimaryButton onClick={onNext}>Continue</PrimaryButton>
    </div>
  );
}

// ─── Step 3: LLM Provider ───────────────────────────────────────────────────

const LLM_PROVIDERS = [
  { id: "groq", name: "Groq", desc: "Fast cloud inference", defaultModel: "llama-3.3-70b-versatile", recommended: true, isLocal: false },
  { id: "ollama", name: "Ollama", desc: "Local, free, private", defaultModel: "", recommended: false, isLocal: true },
  { id: "openai", name: "OpenAI", desc: "GPT-4o & more", defaultModel: "gpt-4o-mini", recommended: false, isLocal: false },
  { id: "anthropic", name: "Anthropic", desc: "Claude models", defaultModel: "claude-sonnet-4-20250514", recommended: false, isLocal: false },
  { id: "gemini", name: "Gemini", desc: "Google AI models", defaultModel: "gemini-2.0-flash", recommended: false, isLocal: false },
  { id: "cerebras", name: "Cerebras", desc: "Ultra-fast inference", defaultModel: "llama-3.3-70b", recommended: false, isLocal: false },
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
      <BackButton onClick={onBack} />
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
              className={`relative p-3 rounded-xl border text-left transition-all cursor-pointer ${
                selected
                  ? "border-[var(--accent)] bg-[var(--accent)]/5"
                  : "border-[var(--border)] bg-[var(--bg-card)] hover:border-[var(--accent)]/50"
              }`}
            >
              {p.recommended && (
                <span className="absolute -top-2 right-2 text-[10px] bg-[var(--accent)] text-white px-2 py-0.5 rounded-full">
                  Recommended
                </span>
              )}
              {selected && (
                <span className="absolute top-2 right-2 w-4 h-4 rounded-full bg-[var(--accent)] flex items-center justify-center">
                  <IconCheck className="w-2.5 h-2.5 text-white" />
                </span>
              )}
              <div className="w-8 h-8 rounded-lg bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-xs font-bold text-[var(--accent)] mb-2">
                {p.name[0]}
              </div>
              <div className="text-sm font-semibold text-[var(--text-primary)]">{p.name}</div>
              <div className="text-xs text-[var(--text-secondary)]">{p.desc}</div>
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
            className="w-full px-3 py-2.5 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      )}

      {/* Model input or Ollama dropdown */}
      {state.llmProvider === "ollama" ? (
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">Ollama Model</label>
          {ollamaModels.length > 0 ? (
            <select
              value={state.llmModel}
              onChange={(e) => update("llmModel", e.target.value)}
              className="w-full px-3 py-2.5 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] cursor-pointer"
            >
              {ollamaModels.map((m) => (
                <option key={m} value={m}>{m}</option>
              ))}
            </select>
          ) : (
            <p className="text-xs text-[var(--text-secondary)]">No Ollama models found. Make sure Ollama is running.</p>
          )}
        </div>
      ) : (
        <div className="mb-4">
          <label className="block text-sm font-medium text-[var(--text-primary)] mb-1.5">Model Name</label>
          <input
            type="text"
            value={state.llmModel}
            onChange={(e) => update("llmModel", e.target.value)}
            className="w-full px-3 py-2.5 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </div>
      )}

      <PrimaryButton onClick={onNext}>Continue</PrimaryButton>

      <button
        onClick={() => {
          update("llmProvider", "none");
          update("llmApiKey", "");
          update("llmModel", "");
          onNext();
        }}
        className="w-full text-center text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer py-2 mt-1"
      >
        Skip — no LLM cleanup
      </button>
    </div>
  );
}

// ─── Step 4: Permissions ────────────────────────────────────────────────────

function PermissionsStep({ state, update, onNext, onBack }: StepProps) {
  useEffect(() => {
    const poll = setInterval(() => {
      invoke<{ mic: boolean; accessibility: boolean; input_monitoring?: boolean }>("check_permissions")
        .then((perms) => {
          update("micGranted", perms.mic);
          update("accessibilityGranted", perms.accessibility);
          if (perms.input_monitoring !== undefined) {
            update("inputMonitoringGranted", perms.input_monitoring);
          }
        })
        .catch(() => {});
    }, 2000);

    invoke<{ mic: boolean; accessibility: boolean; input_monitoring?: boolean }>("check_permissions")
      .then((perms) => {
        update("micGranted", perms.mic);
        update("accessibilityGranted", perms.accessibility);
        if (perms.input_monitoring !== undefined) {
          update("inputMonitoringGranted", perms.input_monitoring);
        }
      })
      .catch(() => {});

    return () => clearInterval(poll);
  }, [update]);

  const allGranted = state.micGranted && state.accessibilityGranted;

  const permissions = [
    {
      name: "Microphone",
      desc: "Required to capture your voice",
      granted: state.micGranted,
      action: () => invoke("open_mic_settings").catch(() => {}),
      actionLabel: "Open Microphone Settings",
      icon: <IconMic className="w-5 h-5" />,
    },
    {
      name: "Accessibility",
      desc: "Required to type text at your cursor",
      granted: state.accessibilityGranted,
      action: () => invoke("open_accessibility_settings").catch(() => {}),
      actionLabel: "Grant Access",
      icon: <IconKeyboard className="w-5 h-5" />,
    },
    {
      name: "Input Monitoring",
      desc: "Required for global hotkey (Option+Space)",
      granted: state.inputMonitoringGranted,
      action: () => invoke("open_input_monitoring_settings").catch(() => {}),
      actionLabel: "Open Input Monitoring",
      icon: <IconGear className="w-5 h-5" />,
    },
  ];

  return (
    <div className="px-8 py-6 max-w-lg mx-auto">
      <BackButton onClick={onBack} />
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">
        Permissions
      </h2>
      <p className="text-sm text-[var(--text-secondary)] mb-5">
        Chamgei needs a few macOS permissions to work
      </p>

      <div className="space-y-3 mb-6">
        {permissions.map((perm) => (
          <motion.div
            key={perm.name}
            layout
            className="p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--border)]"
          >
            <div className="flex items-start gap-3">
              <div className={`w-10 h-10 rounded-lg flex items-center justify-center shrink-0 ${
                perm.granted
                  ? "bg-green-500/10 text-green-400"
                  : "bg-[var(--bg-secondary)] text-[var(--accent)]"
              }`}>
                {perm.icon}
              </div>
              <div className="flex-1">
                <div className="flex items-center gap-2 mb-1">
                  <span className="text-sm font-semibold text-[var(--text-primary)]">{perm.name}</span>
                  <StatusBadge granted={perm.granted} />
                </div>
                <p className="text-xs text-[var(--text-secondary)] mb-2">{perm.desc}</p>
                {!perm.granted && (
                  <button
                    onClick={perm.action}
                    className="px-3 py-1.5 text-xs rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer"
                  >
                    {perm.actionLabel}
                  </button>
                )}
              </div>
            </div>
          </motion.div>
        ))}
      </div>

      <PrimaryButton onClick={onNext} disabled={!allGranted}>
        Continue
      </PrimaryButton>

      {!allGranted && (
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
        <IconCheck className="w-3 h-3" />
        Granted
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1 text-[10px] text-yellow-400 bg-yellow-400/10 px-1.5 py-0.5 rounded-full">
      <svg className="w-3 h-3" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
        <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z" />
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
          setLevels((prev) => [...prev.slice(1), level]);
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
      <BackButton onClick={onBack} />
      <h2 className="text-xl font-bold text-[var(--text-primary)] mb-1">Mic Test</h2>
      <p className="text-sm text-[var(--text-secondary)] mb-6">Speak something to test your microphone</p>

      <div className="flex items-end justify-center gap-1 h-32 mb-6 p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--border)]">
        {levels.map((level, i) => (
          <motion.div
            key={i}
            className="w-2 rounded-full bg-[var(--accent)]"
            animate={{
              height: `${Math.max(4, level * 100)}%`,
              opacity: 0.4 + level * 0.6,
            }}
            transition={{ duration: 0.1 }}
          />
        ))}
      </div>

      {detected ? (
        <motion.div
          initial={{ scale: 0.9, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          className="mb-6 p-3 rounded-xl bg-green-400/10 border border-green-400/30 text-center"
        >
          <span className="text-sm font-semibold text-green-400">Microphone working!</span>
        </motion.div>
      ) : (
        <p className="text-center text-xs text-[var(--text-secondary)] mb-6">Waiting for audio input...</p>
      )}

      <PrimaryButton onClick={onNext}>Continue</PrimaryButton>
      {!detected && (
        <p className="text-center text-xs text-[var(--text-secondary)] mt-2">
          Mic not working? You can skip — the CLI uses your system mic via Terminal.
        </p>
      )}
    </div>
  );
}

// ─── Step 6: Try It ─────────────────────────────────────────────────────────

function TryItStep({ state, update, onNext, onBack }: StepProps) {
  const [phase, setPhase] = useState<"idle" | "recording" | "processing" | "done">("idle");
  const [result, setResult] = useState("");

  useEffect(() => {
    if (phase === "done") return;
    const interval = setInterval(() => {
      invoke<string>("get_pipeline_status")
        .then((status) => {
          if (status === "recording" && phase !== "recording") setPhase("recording");
          else if (status === "processing" && phase !== "processing") setPhase("processing");
          else if (status.startsWith("done:")) {
            setResult(status.slice(5));
            setPhase("done");
            update("firstDictationDone", true);
          }
        })
        .catch(() => {});
    }, 200);
    return () => clearInterval(interval);
  }, [phase, update]);

  return (
    <div className="px-8 py-6 max-w-lg mx-auto flex flex-col items-center min-h-[70vh] justify-center">
      <BackButton onClick={onBack} className="self-start" />

      <AnimatePresence mode="wait">
        {phase === "idle" && (
          <motion.div key="idle" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="text-center">
            <h2 className="text-2xl font-bold text-[var(--text-primary)] mb-2">
              Press Option+Space and speak
            </h2>
            <p className="text-sm text-[var(--text-secondary)] mb-8">Try your first dictation</p>
            <motion.div
              animate={{ scale: [0.95, 1.05, 0.95] }}
              transition={{ duration: 2, repeat: Infinity, ease: "easeInOut" }}
              className="inline-flex items-center gap-2 px-6 py-4 rounded-2xl bg-[var(--bg-card)] border-2 border-[var(--accent)] mb-6"
            >
              <kbd className="text-lg font-bold text-[var(--accent)]">&#x2325;</kbd>
              <span className="text-[var(--text-secondary)]">+</span>
              <kbd className="text-lg font-bold text-[var(--accent)]">Space</kbd>
            </motion.div>
          </motion.div>
        )}

        {phase === "recording" && (
          <motion.div key="rec" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="text-center">
            <div className="flex items-center gap-2 mb-4">
              <motion.div
                animate={{ scale: [1, 1.3, 1], opacity: [1, 0.6, 1] }}
                transition={{ duration: 1, repeat: Infinity }}
                className="w-3 h-3 rounded-full bg-red-500"
              />
              <span className="text-lg font-semibold text-red-400">Recording...</span>
            </div>
            <p className="text-sm text-[var(--text-secondary)]">Release Option+Space when done</p>
          </motion.div>
        )}

        {phase === "processing" && (
          <motion.div key="proc" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="text-center">
            <div className="flex items-center gap-2 mb-4">
              <div className="w-5 h-5 rounded-full border-2 border-[var(--accent)] border-t-transparent animate-spin" />
              <span className="text-lg font-semibold text-[var(--text-primary)]">Processing...</span>
            </div>
          </motion.div>
        )}

        {phase === "done" && (
          <motion.div key="done" initial={{ scale: 0.8, opacity: 0 }} animate={{ scale: 1, opacity: 1 }} className="w-full">
            <div className="text-center mb-4">
              <motion.span
                initial={{ scale: 0 }}
                animate={{ scale: 1 }}
                transition={{ type: "spring", stiffness: 400, damping: 15 }}
                className="inline-flex items-center justify-center w-12 h-12 rounded-full bg-green-400/20 mb-3"
              >
                <IconCheck className="w-6 h-6 text-green-400" />
              </motion.span>
              <p className="text-lg font-bold text-[var(--text-primary)]">Dictation works!</p>
            </div>
            <div className="p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--accent)] text-sm text-[var(--text-primary)] leading-relaxed">
              {result}
            </div>
          </motion.div>
        )}
      </AnimatePresence>

      {(phase === "done" || state.firstDictationDone) && (
        <PrimaryButton onClick={onNext} className="mt-6">Continue</PrimaryButton>
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
        : "Deepgram Nova-2";

  const llmLabel =
    state.llmProvider === "none"
      ? "None (raw transcription)"
      : `${state.llmProvider}${state.llmModel ? ` / ${state.llmModel}` : ""}`;

  return (
    <div className="px-8 py-6 max-w-lg mx-auto flex flex-col items-center min-h-[70vh] justify-center">
      <BackButton onClick={onBack} className="self-start" />

      <motion.div
        initial={{ scale: 0 }}
        animate={{ scale: 1 }}
        transition={{ type: "spring", stiffness: 300, damping: 20 }}
        className="w-16 h-16 rounded-full bg-green-400/20 flex items-center justify-center mb-4"
      >
        <IconCheck className="w-8 h-8 text-green-400" />
      </motion.div>

      <h2 className="text-2xl font-bold text-[var(--text-primary)] mb-1">All Set!</h2>
      <p className="text-sm text-[var(--text-secondary)] mb-6">Chamgei is ready to go</p>

      <div className="w-full p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--border)] mb-6 text-sm space-y-2">
        <div className="flex justify-between">
          <span className="text-[var(--text-secondary)]">STT</span>
          <span className="text-[var(--text-primary)] font-medium">{sttLabel}</span>
        </div>
        <div className="flex justify-between">
          <span className="text-[var(--text-secondary)]">LLM</span>
          <span className="text-[var(--text-primary)] font-medium">{llmLabel}</span>
        </div>
        <div className="border-t border-[var(--border)] pt-2 mt-2 space-y-1">
          <div className="flex justify-between text-xs">
            <span className="text-[var(--text-secondary)]">Dictate</span>
            <kbd className="px-1.5 py-0.5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--accent)] font-mono">
              &#x2325; Space
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

      <motion.button
        whileHover={{ scale: 1.02 }}
        whileTap={{ scale: 0.98 }}
        onClick={handleFinish}
        className="w-full py-3 rounded-xl bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white font-semibold text-base transition-colors cursor-pointer shadow-lg shadow-teal-500/20"
      >
        Start Chamgei
      </motion.button>
    </div>
  );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Settings App — Sidebar Navigation
// ═══════════════════════════════════════════════════════════════════════════════

function SettingsApp() {
  const [activePage, setActivePage] = useState<SettingsPage>("general");
  const [settings, setSettings] = useState<Settings>(defaultSettings);
  const [saving, setSaving] = useState(false);
  const [saveStatus, setSaveStatus] = useState<"idle" | "saved" | "error">("idle");

  const pages: { id: SettingsPage; label: string; icon: React.ReactNode; section?: string }[] = [
    { id: "general", label: "General", icon: <IconGear />, section: "Settings" },
    { id: "inference", label: "Inference", icon: <IconCpu /> },
    { id: "audio", label: "Audio", icon: <IconAudio /> },
    { id: "hotkeys", label: "Hotkeys", icon: <IconKeyboard /> },
    { id: "privacy", label: "Privacy", icon: <IconShield /> },
    { id: "history", label: "History", icon: <IconClock />, section: "Data" },
    { id: "about", label: "About", icon: <IconInfo />, section: "App" },
  ];

  useEffect(() => {
    invoke<string>("load_config_json").then((json) => {
      try {
        const config = JSON.parse(json);
        setSettings({
          activationMode: config.activation_mode || "push_to_talk",
          sttEngine: config.stt_engine || "local",
          llmProvider: config.providers?.[0]?.name || config.llm_provider || "local",
          groqApiKey: "",
          cerebrasApiKey: "",
          deepgramApiKey: "",
          whisperModel: config.whisper_model || "tiny",
          vadThreshold: config.vad_threshold ?? 0.01,
          injectionMethod: config.injection_method || "clipboard",
          privacyMode: config.privacy_mode ?? false,
        });
      } catch {
        // keep defaults
      }
    });
  }, []);

  const update = <K extends keyof Settings>(key: K, value: Settings[K]) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
    setSaveStatus("idle");
  };

  const saveSettings = async () => {
    setSaving(true);
    setSaveStatus("idle");
    try {
      const toml = `activation_mode = "${settings.activationMode}"
whisper_model = "${settings.whisperModel}"
vad_threshold = ${settings.vadThreshold}
injection_method = "${settings.injectionMethod}"
stt_engine = "${settings.sttEngine}"
llm_provider = "${settings.llmProvider}"
privacy_mode = ${settings.privacyMode}
`;
      await invoke("save_config", { config: toml });
      setSaveStatus("saved");
      setTimeout(() => setSaveStatus("idle"), 2000);
    } catch {
      setSaveStatus("error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex h-screen select-none">
      {/* Sidebar */}
      <aside className="w-52 bg-[var(--bg-secondary)] border-r border-[var(--border)] flex flex-col">
        {/* App header */}
        <div className="px-4 py-4 border-b border-[var(--border)]" data-tauri-drag-region>
          <div className="flex items-center gap-2">
            <div className="w-7 h-7 rounded-lg bg-gradient-to-br from-teal-500 to-teal-700 flex items-center justify-center">
              <IconMic className="w-3.5 h-3.5 text-white" />
            </div>
            <div>
              <span className="text-sm font-bold text-[var(--text-primary)]">Chamgei</span>
              <span className="text-[10px] text-[var(--text-secondary)] ml-1.5">v0.1.0</span>
            </div>
          </div>
        </div>

        {/* Navigation */}
        <nav className="flex-1 py-2 overflow-y-auto">
          {pages.map((page) => (
            <React.Fragment key={page.id}>
              {page.section && (
                <div className="px-4 pt-4 pb-1 text-[10px] font-semibold uppercase tracking-wider text-[var(--text-secondary)]">
                  {page.section}
                </div>
              )}
              <button
                onClick={() => setActivePage(page.id)}
                className={`w-full flex items-center gap-2.5 px-4 py-2 text-sm transition-colors cursor-pointer ${
                  activePage === page.id
                    ? "bg-[var(--accent)]/10 text-[var(--accent)] font-medium"
                    : "text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-white/5"
                }`}
              >
                <span className={activePage === page.id ? "text-[var(--accent)]" : "text-[var(--text-secondary)]"}>
                  {page.icon}
                </span>
                {page.label}
              </button>
            </React.Fragment>
          ))}
        </nav>

        {/* Status indicator */}
        <div className="p-4 border-t border-[var(--border)]">
          <div className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
            <div className="w-2 h-2 rounded-full bg-green-400" />
            <span>Ready</span>
          </div>
          <div className="mt-1 text-[10px] text-[var(--text-secondary)] opacity-60">
            &#x2325; Space to dictate
          </div>
        </div>
      </aside>

      {/* Main content area */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Content header */}
        <header className="px-6 py-4 border-b border-[var(--border)] bg-[var(--bg-primary)]" data-tauri-drag-region>
          <h1 className="text-lg font-bold text-[var(--text-primary)]">
            {pages.find((p) => p.id === activePage)?.label}
          </h1>
        </header>

        {/* Scrollable content */}
        <main className="flex-1 overflow-y-auto p-6">
          <AnimatePresence mode="wait">
            <motion.div
              key={activePage}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -8 }}
              transition={{ duration: 0.15 }}
            >
              {activePage === "general" && <GeneralTab settings={settings} update={update} />}
              {activePage === "inference" && <InferenceTab settings={settings} update={update} />}
              {activePage === "audio" && <AudioTab settings={settings} update={update} />}
              {activePage === "hotkeys" && <HotkeysTab />}
              {activePage === "privacy" && <PrivacyTab settings={settings} update={update} />}
              {activePage === "history" && <HistoryTab />}
              {activePage === "about" && <AboutTab />}
            </motion.div>
          </AnimatePresence>
        </main>

        {/* Save footer */}
        {activePage !== "history" && activePage !== "about" && (
          <footer className="px-6 py-3 bg-[var(--bg-secondary)] border-t border-[var(--border)] flex items-center gap-3">
            <motion.button
              whileHover={{ scale: 1.02 }}
              whileTap={{ scale: 0.98 }}
              onClick={saveSettings}
              disabled={saving}
              className="px-5 py-2 text-sm font-medium rounded-lg bg-[var(--accent)] text-white hover:bg-[var(--accent-hover)] transition-colors cursor-pointer disabled:opacity-50"
            >
              {saving ? "Saving..." : "Save"}
            </motion.button>
            {saveStatus === "saved" && (
              <motion.span initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="text-xs text-green-400">
                Settings saved
              </motion.span>
            )}
            {saveStatus === "error" && (
              <span className="text-xs text-red-400">Failed to save</span>
            )}
          </footer>
        )}
      </div>
    </div>
  );
}

// ─── Data Flow Indicator ────────────────────────────────────────────────────

function DataFlowIndicator({ settings }: { settings: Settings }) {
  const sttLabel = settings.sttEngine === "local" ? "Local Whisper" : settings.sttEngine === "deepgram" ? "Deepgram" : "Groq";
  const sttIsLocal = settings.sttEngine === "local";
  const llmLabel = settings.llmProvider === "ollama" || settings.llmProvider === "local" ? "Local LLM" : settings.llmProvider;
  const llmIsLocal = settings.llmProvider === "ollama" || settings.llmProvider === "local";

  const steps = [
    { label: "Voice", icon: <IconMic className="w-3.5 h-3.5" />, local: true },
    { label: sttLabel, icon: <IconCpu className="w-3.5 h-3.5" />, local: sttIsLocal },
    { label: llmLabel === "local" ? "Raw Text" : llmLabel, icon: <IconLightning className="w-3.5 h-3.5" />, local: llmIsLocal },
    { label: "Text", icon: <IconKeyboard className="w-3.5 h-3.5" />, local: true },
  ];

  return (
    <div className="p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--border)]">
      <div className="flex items-center justify-between">
        {steps.map((step, i) => (
          <React.Fragment key={i}>
            <div className="flex flex-col items-center gap-1.5">
              <div className={`w-8 h-8 rounded-lg flex items-center justify-center ${
                step.local ? "bg-green-500/10 text-green-400" : "bg-blue-500/10 text-blue-400"
              }`}>
                {step.icon}
              </div>
              <span className="text-[10px] text-[var(--text-secondary)] text-center leading-tight max-w-[60px]">
                {step.label}
              </span>
              <span className={`text-[9px] px-1.5 py-0.5 rounded-full ${
                step.local ? "bg-green-500/10 text-green-400" : "bg-blue-500/10 text-blue-400"
              }`}>
                {step.local ? "Local" : "Cloud"}
              </span>
            </div>
            {i < steps.length - 1 && (
              <IconArrowRight className="w-3.5 h-3.5 text-[var(--text-secondary)] opacity-40 shrink-0" />
            )}
          </React.Fragment>
        ))}
      </div>
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
    <div className="space-y-5 max-w-xl">
      {/* Data flow indicator */}
      <div>
        <h3 className="text-xs font-semibold uppercase tracking-wider text-[var(--text-secondary)] mb-3">
          Data Flow
        </h3>
        <DataFlowIndicator settings={settings} />
      </div>

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
          onChange={(v) => update("injectionMethod", v as Settings["injectionMethod"])}
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

  useEffect(() => { refresh(); }, [refresh]);

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
    } catch { /* ignore */ }
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

  if (masked && !editing) {
    return (
      <div className="flex items-center gap-2">
        <code className="flex-1 px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-secondary)] font-mono">
          {masked}
        </code>
        <span className="text-xs text-green-400 font-medium whitespace-nowrap">Configured &#10003;</span>
        <button
          onClick={handleTest}
          disabled={testResult === "testing"}
          className="px-3 py-1.5 rounded-lg text-xs font-medium bg-[var(--input-bg)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors cursor-pointer disabled:opacity-50"
        >
          {testResult === "testing" ? "..." : testResult === "valid" ? "Valid !" : testResult === "invalid" ? "Invalid" : "Test"}
        </button>
        <button
          onClick={() => { setEditing(true); setValue(""); setTestResult("idle"); }}
          className="px-3 py-1.5 rounded-lg text-xs font-medium bg-[var(--input-bg)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors cursor-pointer"
        >
          Change
        </button>
        <button
          onClick={handleDelete}
          className="px-3 py-1.5 rounded-lg text-xs font-medium bg-[var(--input-bg)] border border-red-500/30 text-red-400 hover:border-red-500 transition-colors cursor-pointer"
        >
          Delete
        </button>
      </div>
    );
  }

  return (
    <div className="flex items-center gap-2">
      <input
        type="password"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        placeholder={placeholder}
        className="flex-1 px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
        onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
      />
      <button
        onClick={handleSave}
        disabled={saving || !value.trim()}
        className="px-4 py-2 rounded-lg text-xs font-semibold bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors cursor-pointer disabled:opacity-50"
      >
        {saving ? "..." : "Save"}
      </button>
      {masked && (
        <button
          onClick={() => { setEditing(false); setTestResult("idle"); }}
          className="px-3 py-2 rounded-lg text-xs font-medium bg-[var(--input-bg)] border border-[var(--border)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors cursor-pointer"
        >
          Cancel
        </button>
      )}
    </div>
  );
}

function InferenceTab({ settings, update }: TabProps) {
  return (
    <div className="space-y-5 max-w-xl">
      <Section title="Speech-to-Text Engine">
        <Select
          value={settings.sttEngine}
          onChange={(v) => update("sttEngine", v as Settings["sttEngine"])}
          options={[
            { value: "local", label: "[Local] Whisper — audio stays on device" },
            { value: "groq", label: "[Cloud] Groq Whisper — audio sent to Groq" },
            { value: "deepgram", label: "[Cloud] Deepgram Nova-2 — audio sent to Deepgram" },
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
            { value: "together", label: "Together AI" },
            { value: "openrouter", label: "OpenRouter" },
            { value: "openai", label: "OpenAI" },
            { value: "anthropic", label: "Anthropic" },
            { value: "gemini", label: "Google Gemini" },
            { value: "ollama", label: "Ollama (local)" },
            { value: "local", label: "None (raw transcription)" },
          ]}
        />
      </Section>

      {settings.llmProvider === "cerebras" && (
        <Section title="Cerebras API Key"><ApiKeyField provider="cerebras" placeholder="csk-..." /></Section>
      )}
      {(settings.llmProvider === "groq" || settings.sttEngine === "groq") && (
        <Section title="Groq API Key"><ApiKeyField provider="groq" placeholder="gsk_..." /></Section>
      )}
      {settings.llmProvider === "openai" && (
        <Section title="OpenAI API Key"><ApiKeyField provider="openai" placeholder="sk-..." /></Section>
      )}
      {settings.llmProvider === "anthropic" && (
        <Section title="Anthropic API Key"><ApiKeyField provider="anthropic" placeholder="sk-ant-..." /></Section>
      )}
      {settings.llmProvider === "together" && (
        <Section title="Together AI API Key"><ApiKeyField provider="together" placeholder="tok-..." /></Section>
      )}
      {settings.llmProvider === "openrouter" && (
        <Section title="OpenRouter API Key"><ApiKeyField provider="openrouter" placeholder="sk-or-..." /></Section>
      )}
      {settings.llmProvider === "gemini" && (
        <Section title="Google Gemini API Key"><ApiKeyField provider="gemini" placeholder="AI..." /></Section>
      )}

      {settings.sttEngine === "local" && (
        <Section title="Local Model">
          <Select
            value={settings.whisperModel}
            onChange={(v) => update("whisperModel", v as Settings["whisperModel"])}
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
    <div className="space-y-5 max-w-xl">
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
          Lower = more sensitive (picks up quieter speech). Higher = less sensitive (ignores background noise).
        </p>
      </Section>
    </div>
  );
}

function HotkeysTab() {
  return (
    <div className="space-y-5 max-w-xl">
      <Section title="Dictation Hotkey">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-1.5">
            <kbd className="px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm font-mono text-[var(--accent)]">
              &#x2325; Option
            </kbd>
            <span className="text-[var(--text-secondary)]">+</span>
            <kbd className="px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm font-mono text-[var(--accent)]">
              Space
            </kbd>
          </div>
        </div>
        <p className="mt-2 text-xs text-[var(--text-secondary)]">
          {`Hold Option+Space to record (push-to-talk mode) or press once to toggle recording.`}
        </p>
      </Section>

      <Section title="How It Works">
        <div className="space-y-2 text-xs text-[var(--text-secondary)]">
          <div className="flex items-center gap-2">
            <span className="w-5 h-5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-[10px] font-bold text-[var(--accent)]">1</span>
            <span>Hold Option+Space — recording starts</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="w-5 h-5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-[10px] font-bold text-[var(--accent)]">2</span>
            <span>Speak your text naturally</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="w-5 h-5 rounded bg-[var(--bg-secondary)] border border-[var(--border)] flex items-center justify-center text-[10px] font-bold text-[var(--accent)]">3</span>
            <span>Release — text is transcribed and typed at your cursor</span>
          </div>
        </div>
      </Section>
    </div>
  );
}

function PrivacyTab({ settings, update }: TabProps) {
  return (
    <div className="space-y-5 max-w-xl">
      <Section title="Privacy Mode">
        <div className="flex items-center justify-between">
          <div>
            <p className="text-sm text-[var(--text-primary)] font-medium">Enhanced Privacy</p>
            <p className="text-xs text-[var(--text-secondary)] mt-0.5">
              When enabled, disables history logging and forces local-only processing
            </p>
          </div>
          <button
            onClick={() => update("privacyMode", !settings.privacyMode)}
            className={`relative w-11 h-6 rounded-full transition-colors cursor-pointer ${
              settings.privacyMode ? "bg-[var(--accent)]" : "bg-[var(--border)]"
            }`}
          >
            <motion.div
              animate={{ x: settings.privacyMode ? 20 : 2 }}
              transition={{ type: "spring", stiffness: 500, damping: 30 }}
              className="absolute top-1 w-4 h-4 rounded-full bg-white"
            />
          </button>
        </div>
      </Section>

      <Section title="Data Handling">
        <div className="space-y-3 text-xs text-[var(--text-secondary)]">
          <div className="flex items-start gap-3">
            <div className="w-6 h-6 rounded bg-green-500/10 flex items-center justify-center shrink-0 mt-0.5">
              <IconShield className="w-3.5 h-3.5 text-green-400" />
            </div>
            <div>
              <p className="text-[var(--text-primary)] font-medium text-sm">Audio processing</p>
              <p className="mt-0.5">
                {settings.sttEngine === "local"
                  ? "Audio is processed locally and never leaves your device."
                  : `Audio is sent to ${settings.sttEngine === "deepgram" ? "Deepgram" : "Groq"} for transcription. Switch to Local Whisper for full privacy.`}
              </p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="w-6 h-6 rounded bg-green-500/10 flex items-center justify-center shrink-0 mt-0.5">
              <IconShield className="w-3.5 h-3.5 text-green-400" />
            </div>
            <div>
              <p className="text-[var(--text-primary)] font-medium text-sm">API keys</p>
              <p className="mt-0.5">Stored securely in macOS Keychain. Never transmitted or logged.</p>
            </div>
          </div>
          <div className="flex items-start gap-3">
            <div className="w-6 h-6 rounded bg-green-500/10 flex items-center justify-center shrink-0 mt-0.5">
              <IconShield className="w-3.5 h-3.5 text-green-400" />
            </div>
            <div>
              <p className="text-[var(--text-primary)] font-medium text-sm">No telemetry</p>
              <p className="mt-0.5">Chamgei collects zero analytics or usage data. Fully open source.</p>
            </div>
          </div>
        </div>
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

  useEffect(() => { loadHistory(); }, [loadHistory]);

  const handleClear = async () => {
    try {
      await invoke("clear_history");
      setEntries([]);
    } catch { /* ignore */ }
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
      <div className="flex items-center gap-3">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search dictations..."
          className="flex-1 px-3 py-2 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
        />
        {entries.length > 0 && (
          <button
            onClick={handleClear}
            className="px-3 py-2 text-xs rounded-lg bg-[var(--bg-secondary)] border border-[var(--border)] text-red-400 hover:text-red-300 hover:border-red-400 transition-colors cursor-pointer whitespace-nowrap"
          >
            Clear History
          </button>
        )}
      </div>

      {filtered.length === 0 ? (
        <div className="flex flex-col items-center justify-center h-48 text-center">
          <p className="text-sm text-[var(--text-secondary)]">
            {entries.length === 0
              ? "No dictations yet. Hold Option+Space and speak to get started."
              : "No results match your search."}
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          {filtered.map((entry, idx) => (
            <div
              key={idx}
              className="p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--border)] group"
            >
              <div className="flex items-start justify-between gap-3">
                <p className="text-sm text-[var(--text-primary)] leading-relaxed flex-1">
                  {entry.text}
                </p>
                <button
                  onClick={() => handleCopy(entry.text, idx)}
                  className="px-2.5 py-1 text-xs rounded-lg bg-[var(--bg-secondary)] border border-[var(--border)] text-[var(--text-secondary)] hover:text-[var(--accent)] hover:border-[var(--accent)] transition-colors cursor-pointer opacity-0 group-hover:opacity-100 shrink-0"
                >
                  {copiedIdx === idx ? "Copied!" : "Copy"}
                </button>
              </div>
              <div className="flex items-center gap-3 mt-2 text-xs text-[var(--text-secondary)]">
                <span>{formatRelativeTime(entry.timestamp)}</span>
                <span className="opacity-40">|</span>
                <span className="opacity-60">STT {entry.stt_ms}ms + LLM {entry.llm_ms}ms</span>
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

function AboutTab() {
  return (
    <div className="space-y-5 max-w-xl">
      <div className="flex items-center gap-4 p-5 rounded-xl bg-[var(--bg-card)] border border-[var(--border)]">
        <div className="w-14 h-14 rounded-2xl bg-gradient-to-br from-teal-500 to-teal-700 flex items-center justify-center shadow-lg shadow-teal-500/20">
          <IconMic className="w-7 h-7 text-white" />
        </div>
        <div>
          <h3 className="text-lg font-bold text-[var(--text-primary)]">Chamgei</h3>
          <p className="text-xs text-[var(--text-secondary)]">Version 0.1.0</p>
          <p className="text-xs text-[var(--text-secondary)] mt-1">Privacy-first voice dictation for macOS</p>
        </div>
      </div>

      <Section title="Links">
        <div className="space-y-2">
          <button
            onClick={() => invoke("open_url", { url: "https://github.com/tonykipkemboi/chamgei" }).catch(() => {})}
            className="w-full flex items-center justify-between p-3 rounded-lg bg-[var(--bg-secondary)] hover:bg-white/5 transition-colors cursor-pointer text-sm text-[var(--text-primary)]"
          >
            <span>GitHub Repository</span>
            <IconArrowRight className="w-4 h-4 text-[var(--text-secondary)]" />
          </button>
          <button
            onClick={() => invoke("open_url", { url: "https://github.com/tonykipkemboi/chamgei/issues" }).catch(() => {})}
            className="w-full flex items-center justify-between p-3 rounded-lg bg-[var(--bg-secondary)] hover:bg-white/5 transition-colors cursor-pointer text-sm text-[var(--text-primary)]"
          >
            <span>Report an Issue</span>
            <IconArrowRight className="w-4 h-4 text-[var(--text-secondary)]" />
          </button>
        </div>
      </Section>

      <Section title="Open Source">
        <p className="text-xs text-[var(--text-secondary)]">
          Chamgei is open source under the MIT License. Built with Rust, Tauri, and React.
        </p>
      </Section>
    </div>
  );
}

// ─── Shared UI Components ───────────────────────────────────────────────────

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="p-4 rounded-xl bg-[var(--bg-card)] border border-[var(--border)]">
      <h3 className="text-sm font-semibold mb-3 text-[var(--text-primary)]">{title}</h3>
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
      className="w-full px-3 py-2.5 rounded-lg bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] cursor-pointer"
    >
      {options.map((opt) => (
        <option key={opt.value} value={opt.value}>{opt.label}</option>
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
      className={`px-4 py-2 text-sm rounded-lg border transition-colors cursor-pointer ${
        active
          ? "bg-[var(--accent)] border-[var(--accent)] text-white"
          : "bg-[var(--input-bg)] border-[var(--border)] text-[var(--text-secondary)] hover:border-[var(--accent)]"
      }`}
    >
      {children}
    </button>
  );
}

function BackButton({ onClick, className = "" }: { onClick: () => void; className?: string }) {
  return (
    <button
      onClick={onClick}
      className={`text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] cursor-pointer mb-4 ${className}`}
    >
      &larr; Back
    </button>
  );
}

function PrimaryButton({
  onClick,
  disabled = false,
  children,
  className = "",
}: {
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <motion.button
      whileHover={disabled ? {} : { scale: 1.01 }}
      whileTap={disabled ? {} : { scale: 0.99 }}
      onClick={onClick}
      disabled={disabled}
      className={`w-full py-2.5 rounded-xl font-semibold transition-colors cursor-pointer ${
        disabled
          ? "bg-[var(--bg-secondary)] text-[var(--text-secondary)] cursor-not-allowed"
          : "bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white"
      } ${className}`}
    >
      {children}
    </motion.button>
  );
}

export default App;
