//! Usage statistics tracking for rekody voice dictation.
//!
//! Tracks dictation counts, latency averages, estimated costs, and per-provider
//! usage. Stats are persisted to `~/.config/rekody/stats.json` so they survive
//! across sessions.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── Constants ───────────────────────────────────────────────────────────────

/// Estimated input tokens per dictation (for cost calculation).
const ESTIMATED_INPUT_TOKENS: f64 = 130.0;
/// Estimated output tokens per dictation (for cost calculation).
const ESTIMATED_OUTPUT_TOKENS: f64 = 80.0;

/// Groq pricing: input tokens per million.
const GROQ_INPUT_PRICE_PER_M: f64 = 0.05;
/// Groq pricing: output tokens per million.
const GROQ_OUTPUT_PRICE_PER_M: f64 = 0.08;

/// Cost per dictation at Groq pricing.
const COST_PER_DICTATION_USD: f64 = (ESTIMATED_INPUT_TOKENS * GROQ_INPUT_PRICE_PER_M
    + ESTIMATED_OUTPUT_TOKENS * GROQ_OUTPUT_PRICE_PER_M)
    / 1_000_000.0;

// ── Stats file path ─────────────────────────────────────────────────────────

/// Returns the path to the stats JSON file: `~/.config/rekody/stats.json`.
fn stats_file_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("rekody")
            .join("stats.json"),
    )
}

// ── UsageStats ──────────────────────────────────────────────────────────────

/// Aggregated usage statistics for the rekody dictation system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    /// Total number of completed dictations.
    pub total_dictations: u64,
    /// Total audio duration processed (seconds).
    pub total_duration_secs: f64,
    /// Running average STT latency (milliseconds).
    pub avg_stt_latency_ms: f64,
    /// Running average LLM latency (milliseconds).
    pub avg_llm_latency_ms: f64,
    /// Running average end-to-end latency (milliseconds).
    pub avg_total_latency_ms: f64,
    /// Estimated cumulative API cost (USD), based on ~130 input + 80 output
    /// tokens per dictation at Groq pricing.
    pub estimated_cost_usd: f64,
    /// Number of dictations processed by each provider.
    pub provider_usage: HashMap<String, u64>,
}

impl Default for UsageStats {
    fn default() -> Self {
        Self {
            total_dictations: 0,
            total_duration_secs: 0.0,
            avg_stt_latency_ms: 0.0,
            avg_llm_latency_ms: 0.0,
            avg_total_latency_ms: 0.0,
            estimated_cost_usd: 0.0,
            provider_usage: HashMap::new(),
        }
    }
}

impl UsageStats {
    /// Record a completed dictation, updating all running statistics.
    ///
    /// The running averages use a cumulative moving average so we never need to
    /// store individual measurements.
    pub fn record_dictation(
        &mut self,
        stt_latency_ms: u64,
        llm_latency_ms: u64,
        total_latency_ms: u64,
        audio_duration_secs: f32,
        provider: &str,
    ) {
        self.total_dictations += 1;
        self.total_duration_secs += audio_duration_secs as f64;

        let n = self.total_dictations as f64;

        // Cumulative moving average: avg_new = avg_old + (value - avg_old) / n
        self.avg_stt_latency_ms += (stt_latency_ms as f64 - self.avg_stt_latency_ms) / n;
        self.avg_llm_latency_ms += (llm_latency_ms as f64 - self.avg_llm_latency_ms) / n;
        self.avg_total_latency_ms += (total_latency_ms as f64 - self.avg_total_latency_ms) / n;

        self.estimated_cost_usd += COST_PER_DICTATION_USD;

        *self.provider_usage.entry(provider.to_string()).or_insert(0) += 1;
    }

    /// Reset all statistics to their default (zero) values.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Serialize the stats to a JSON string for the Tauri frontend.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Load stats from `~/.config/rekody/stats.json`.
    ///
    /// Returns default stats if the file does not exist or cannot be parsed.
    pub fn load() -> Self {
        let Some(path) = stats_file_path() else {
            tracing::debug!("could not determine stats file path, using defaults");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match Self::parse_json(&contents) {
                Some(stats) => {
                    tracing::debug!(?path, "loaded usage stats");
                    stats
                }
                None => {
                    tracing::warn!(?path, "failed to parse stats file, using defaults");
                    Self::default()
                }
            },
            Err(_) => {
                tracing::debug!(?path, "no stats file found, using defaults");
                Self::default()
            }
        }
    }

    /// Save stats to `~/.config/rekody/stats.json`.
    pub fn save(&self) {
        let Some(path) = stats_file_path() else {
            tracing::warn!("could not determine stats file path, skipping save");
            return;
        };

        // Ensure the parent directory exists.
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(error = %e, "failed to create stats directory");
            return;
        }

        let json = self.to_json();
        if let Err(e) = std::fs::write(&path, &json) {
            tracing::warn!(error = %e, "failed to save stats");
        } else {
            tracing::debug!(?path, "saved usage stats");
        }
    }

    /// Parse stats from a JSON string.
    fn parse_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stats_are_zero() {
        let stats = UsageStats::default();
        assert_eq!(stats.total_dictations, 0);
        assert_eq!(stats.total_duration_secs, 0.0);
        assert_eq!(stats.avg_stt_latency_ms, 0.0);
        assert!(stats.provider_usage.is_empty());
    }

    #[test]
    fn record_single_dictation() {
        let mut stats = UsageStats::default();
        stats.record_dictation(100, 200, 350, 2.5, "groq");

        assert_eq!(stats.total_dictations, 1);
        assert!((stats.total_duration_secs - 2.5).abs() < f64::EPSILON);
        assert!((stats.avg_stt_latency_ms - 100.0).abs() < f64::EPSILON);
        assert!((stats.avg_llm_latency_ms - 200.0).abs() < f64::EPSILON);
        assert!((stats.avg_total_latency_ms - 350.0).abs() < f64::EPSILON);
        assert_eq!(stats.provider_usage.get("groq"), Some(&1));
    }

    #[test]
    fn running_average_is_correct() {
        let mut stats = UsageStats::default();
        stats.record_dictation(100, 200, 300, 1.0, "cerebras");
        stats.record_dictation(200, 400, 600, 3.0, "cerebras");

        assert_eq!(stats.total_dictations, 2);
        assert!((stats.avg_stt_latency_ms - 150.0).abs() < f64::EPSILON);
        assert!((stats.avg_llm_latency_ms - 300.0).abs() < f64::EPSILON);
        assert!((stats.avg_total_latency_ms - 450.0).abs() < f64::EPSILON);
        assert!((stats.total_duration_secs - 4.0).abs() < f64::EPSILON);
        assert_eq!(stats.provider_usage.get("cerebras"), Some(&2));
    }

    #[test]
    fn reset_clears_all() {
        let mut stats = UsageStats::default();
        stats.record_dictation(100, 200, 300, 1.0, "groq");
        stats.reset();

        assert_eq!(stats.total_dictations, 0);
        assert_eq!(stats.total_duration_secs, 0.0);
        assert!(stats.provider_usage.is_empty());
    }

    #[test]
    fn to_json_roundtrips() {
        let mut stats = UsageStats::default();
        stats.record_dictation(120, 250, 400, 3.2, "groq");
        stats.record_dictation(80, 150, 260, 1.8, "cerebras");

        let json = stats.to_json();
        let parsed = UsageStats::parse_json(&json).expect("should parse");

        assert_eq!(parsed.total_dictations, stats.total_dictations);
        assert!((parsed.avg_stt_latency_ms - stats.avg_stt_latency_ms).abs() < 0.01);
        assert!((parsed.avg_llm_latency_ms - stats.avg_llm_latency_ms).abs() < 0.01);
        assert_eq!(parsed.provider_usage.get("groq"), Some(&1));
        assert_eq!(parsed.provider_usage.get("cerebras"), Some(&1));
    }

    #[test]
    fn multiple_providers_tracked() {
        let mut stats = UsageStats::default();
        stats.record_dictation(100, 200, 300, 1.0, "groq");
        stats.record_dictation(100, 200, 300, 1.0, "groq");
        stats.record_dictation(100, 200, 300, 1.0, "cerebras");

        assert_eq!(stats.provider_usage.get("groq"), Some(&2));
        assert_eq!(stats.provider_usage.get("cerebras"), Some(&1));
        assert_eq!(stats.total_dictations, 3);
    }
}
