import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { motion, AnimatePresence } from "motion/react";

type PipelineStatus =
  | { kind: "Idle" }
  | { kind: "Recording" }
  | { kind: "Processing" }
  | { kind: "Injecting" }
  | { kind: "Error"; message: string };

/** Audio-reactive waveform bars. */
function Waveform() {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 2, height: 20 }}>
      {Array.from({ length: 5 }).map((_, i) => (
        <motion.div
          key={i}
          style={{ width: 3, borderRadius: 99, background: "white" }}
          animate={{
            height: [4, 14 + Math.random() * 6, 6, 16 + Math.random() * 4, 4],
          }}
          transition={{
            duration: 0.55 + Math.random() * 0.25,
            repeat: Infinity,
            ease: "easeInOut",
            delay: i * 0.06,
          }}
        />
      ))}
    </div>
  );
}

/** Checkmark for success. */
function SuccessCheck() {
  return (
    <motion.svg
      width="18" height="18" viewBox="0 0 16 16" fill="none"
      initial={{ scale: 0 }} animate={{ scale: 1 }}
      transition={{ type: "spring", stiffness: 500, damping: 20 }}
    >
      <motion.path
        d="M3 8.5L6.5 12L13 4" stroke="white" strokeWidth="2.5"
        strokeLinecap="round" strokeLinejoin="round"
        initial={{ pathLength: 0 }} animate={{ pathLength: 1 }}
        transition={{ duration: 0.25, delay: 0.08 }}
      />
    </motion.svg>
  );
}

export function Bubble() {
  const [status, setStatus] = useState<PipelineStatus>({ kind: "Idle" });
  const appWindow = getCurrentWindow();

  useEffect(() => {
    const unlisten = listen<PipelineStatus>("pipeline-status", (event) => {
      setStatus(event.payload);
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  useEffect(() => { appWindow.show(); }, [appWindow]);

  const kind = status.kind;
  const isRecording = kind === "Recording";
  const isProcessing = kind === "Processing";
  const isInjecting = kind === "Injecting";
  const isError = kind === "Error";
  const isIdle = kind === "Idle";
  const isBusy = isProcessing || isInjecting;

  const handleToggle = () => {
    if (isBusy) return;
    invoke("toggle_recording").catch(() => {});
  };

  const handleStop = (e: React.MouseEvent) => {
    e.stopPropagation();
    invoke("stop_recording_cmd").catch(() => {});
  };

  // Pill background
  const bg = isRecording
    ? "#dc2626"
    : isError
      ? "#b91c1c"
      : isProcessing
        ? "#1e3a8a"
        : isInjecting
          ? "#065f46"
          : "#18181b";

  return (
    <div
      style={{
        width: "100vw",
        height: "100vh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: "transparent",
        userSelect: "none",
      }}
      data-tauri-drag-region
    >
      <motion.div
        layout
        data-tauri-drag-region
        style={{
          display: "flex",
          alignItems: "center",
          gap: 0,
          background: bg,
          borderRadius: 99,
          height: 44,
          paddingLeft: 4,
          paddingRight: isRecording ? 4 : 4,
          boxShadow: isRecording
            ? "0 2px 20px rgba(220,38,38,0.35)"
            : isIdle
              ? "0 1px 8px rgba(0,0,0,0.3)"
              : "0 2px 16px rgba(0,0,0,0.4)",
          cursor: "grab",
          overflow: "hidden",
        }}
        transition={{ type: "spring", stiffness: 400, damping: 28 }}
      >
        {/* ── Mic / Record button ── */}
        <motion.button
          onClick={handleToggle}
          disabled={isBusy}
          whileHover={isBusy ? {} : { scale: 1.08 }}
          whileTap={isBusy ? {} : { scale: 0.92 }}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            width: 36,
            height: 36,
            borderRadius: "50%",
            background: isIdle ? "rgba(255,255,255,0.08)" : "rgba(255,255,255,0.12)",
            border: "none",
            cursor: isBusy ? "wait" : "pointer",
            outline: "none",
            flexShrink: 0,
          }}
        >
          <AnimatePresence mode="wait">
            {isIdle && (
              <motion.svg
                key="mic"
                width="20" height="20" viewBox="0 0 24 24" fill="none"
                stroke="rgba(255,255,255,0.85)" strokeWidth={1.8}
                initial={{ scale: 0.5, opacity: 0 }}
                animate={{ scale: 1, opacity: 1 }}
                exit={{ scale: 0.5, opacity: 0 }}
                transition={{ duration: 0.12 }}
              >
                <path strokeLinecap="round" strokeLinejoin="round" d="M12 18.75a6 6 0 006-6v-1.5m-6 7.5a6 6 0 01-6-6v-1.5m6 7.5v3.75m-3.75 0h7.5M12 15.75a3 3 0 01-3-3V4.5a3 3 0 116 0v8.25a3 3 0 01-3 3z" />
              </motion.svg>
            )}
            {isRecording && (
              <motion.div
                key="rec-dot"
                initial={{ scale: 0 }} animate={{ scale: 1 }} exit={{ scale: 0 }}
              >
                <motion.div
                  style={{ width: 10, height: 10, borderRadius: "50%", background: "white" }}
                  animate={{ opacity: [1, 0.3, 1] }}
                  transition={{ duration: 1.2, repeat: Infinity, ease: "easeInOut" }}
                />
              </motion.div>
            )}
            {isProcessing && (
              <motion.div
                key="spin"
                initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }}
              >
                <motion.div
                  style={{
                    width: 18, height: 18, borderRadius: "50%",
                    border: "2px solid rgba(255,255,255,0.2)",
                    borderTopColor: "white",
                  }}
                  animate={{ rotate: 360 }}
                  transition={{ duration: 0.7, repeat: Infinity, ease: "linear" }}
                />
              </motion.div>
            )}
            {isInjecting && (
              <motion.div key="check" initial={{ scale: 0 }} animate={{ scale: 1 }} exit={{ scale: 0 }}>
                <SuccessCheck />
              </motion.div>
            )}
            {isError && (
              <motion.svg
                key="warn"
                width="20" height="20" viewBox="0 0 24 24" fill="none"
                stroke="white" strokeWidth={2}
                initial={{ scale: 0.5, opacity: 0 }}
                animate={{ scale: 1, opacity: 1 }}
                exit={{ scale: 0.5, opacity: 0 }}
              >
                <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z" />
              </motion.svg>
            )}
          </AnimatePresence>
        </motion.button>

        {/* ── Middle: waveform / status ── */}
        <AnimatePresence mode="wait">
          {isRecording && (
            <motion.div
              key="wave"
              style={{ display: "flex", alignItems: "center", gap: 8, paddingLeft: 6, paddingRight: 2 }}
              initial={{ opacity: 0, width: 0 }}
              animate={{ opacity: 1, width: "auto" }}
              exit={{ opacity: 0, width: 0 }}
              transition={{ duration: 0.2 }}
            >
              <Waveform />
              <span style={{ fontSize: 12, fontWeight: 600, color: "rgba(255,255,255,0.9)", whiteSpace: "nowrap" }}>
                Listening
              </span>
            </motion.div>
          )}

          {isProcessing && (
            <motion.div
              key="proc-label"
              style={{ paddingLeft: 6, paddingRight: 6 }}
              initial={{ opacity: 0, width: 0 }}
              animate={{ opacity: 1, width: "auto" }}
              exit={{ opacity: 0, width: 0 }}
            >
              <span style={{ fontSize: 11, fontWeight: 500, color: "rgba(191,219,254,0.85)", whiteSpace: "nowrap" }}>
                Processing...
              </span>
            </motion.div>
          )}

          {isError && (
            <motion.div
              key="err-label"
              style={{ paddingLeft: 6, paddingRight: 6, maxWidth: 160, overflow: "hidden" }}
              initial={{ opacity: 0, width: 0 }}
              animate={{ opacity: 1, width: "auto" }}
              exit={{ opacity: 0, width: 0 }}
            >
              <span style={{
                fontSize: 11, fontWeight: 500, color: "rgba(254,202,202,0.9)",
                whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", display: "block",
              }}>
                {status.kind === "Error" ? status.message || "Error" : "Error"}
              </span>
            </motion.div>
          )}
        </AnimatePresence>

        {/* ── Stop button (recording only) ── */}
        <AnimatePresence>
          {isRecording && (
            <motion.button
              key="stop"
              initial={{ scale: 0, width: 0 }}
              animate={{ scale: 1, width: 36 }}
              exit={{ scale: 0, width: 0 }}
              transition={{ type: "spring", stiffness: 500, damping: 25 }}
              onClick={handleStop}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                width: 36,
                height: 36,
                borderRadius: "50%",
                background: "rgba(255,255,255,0.15)",
                border: "none",
                cursor: "pointer",
                outline: "none",
                flexShrink: 0,
              }}
              whileHover={{ scale: 1.1, background: "rgba(255,255,255,0.25)" }}
              whileTap={{ scale: 0.9 }}
              title="Stop recording"
            >
              <svg width="12" height="12" viewBox="0 0 12 12">
                <rect x="1" y="1" width="10" height="10" rx="2" fill="white" fillOpacity="0.9" />
              </svg>
            </motion.button>
          )}
        </AnimatePresence>
      </motion.div>
    </div>
  );
}
