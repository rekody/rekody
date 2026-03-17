import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

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
  llmProvider: "cerebras" | "groq" | "local";
  groqApiKey: string;
  cerebrasApiKey: string;
  whisperModel: "tiny" | "small" | "medium" | "large";
  vadThreshold: number;
  injectionMethod: "clipboard" | "native";
}

const defaultSettings: Settings = {
  activationMode: "push_to_talk",
  llmProvider: "cerebras",
  groqApiKey: "",
  cerebrasApiKey: "",
  whisperModel: "small",
  vadThreshold: 0.5,
  injectionMethod: "clipboard",
};

function App() {
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
        {activeTab === "hotkeys" && <HotkeysTab settings={settings} />}
        {activeTab === "history" && <HistoryTab />}
      </main>
    </div>
  );
}

// --- Tab Components ---

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

function InferenceTab({ settings, update }: TabProps) {
  return (
    <div className="space-y-5">
      <Section title="LLM Provider">
        <Select
          value={settings.llmProvider}
          onChange={(v) => update("llmProvider", v as Settings["llmProvider"])}
          options={[
            { value: "cerebras", label: "Cerebras" },
            { value: "groq", label: "Groq" },
            { value: "local", label: "Local (no LLM)" },
          ]}
        />
      </Section>

      {settings.llmProvider === "cerebras" && (
        <Section title="Cerebras API Key">
          <input
            type="password"
            value={settings.cerebrasApiKey}
            onChange={(e) => update("cerebrasApiKey", e.target.value)}
            placeholder="csk-..."
            className="w-full px-3 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </Section>
      )}

      {settings.llmProvider === "groq" && (
        <Section title="Groq API Key">
          <input
            type="password"
            value={settings.groqApiKey}
            onChange={(e) => update("groqApiKey", e.target.value)}
            placeholder="gsk_..."
            className="w-full px-3 py-2 rounded bg-[var(--input-bg)] border border-[var(--border)] text-sm text-[var(--text-primary)] placeholder:text-[var(--text-secondary)] focus:outline-none focus:border-[var(--accent)]"
          />
        </Section>
      )}

      <Section title="Whisper Model">
        <Select
          value={settings.whisperModel}
          onChange={(v) =>
            update("whisperModel", v as Settings["whisperModel"])
          }
          options={[
            { value: "tiny", label: "Tiny — fastest, least accurate" },
            { value: "small", label: "Small — balanced (recommended)" },
            { value: "medium", label: "Medium — slower, more accurate" },
            { value: "large", label: "Large — slowest, most accurate" },
          ]}
        />
      </Section>
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

function HotkeysTab({ settings: _settings }: { settings: Settings }) {
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
    // Fallback to navigator clipboard
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
      // Newest first
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

// --- Shared UI Components ---

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
