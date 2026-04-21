//! LLM post-processing providers for rekody.
//!
//! Transforms raw STT transcripts into polished, context-aware text
//! using cloud providers (Cerebras, Groq) or local llama.cpp.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
    #[error("API request failed: {0}")]
    ApiError(String),
    #[error("local model error: {0}")]
    LocalModelError(String),
}

/// Context about the active application for formatting decisions.
#[derive(Debug, Clone)]
pub struct AppContext {
    /// Name of the active application (e.g., "Visual Studio Code").
    pub app_name: String,
    /// Bundle identifier on macOS (e.g., "com.microsoft.VSCode").
    pub bundle_id: Option<String>,
}

/// Result of LLM post-processing.
#[derive(Debug, Clone)]
pub struct FormattedText {
    /// The cleaned, formatted text ready for injection.
    pub text: String,
    /// Which provider was used.
    pub provider: String,
    /// Processing latency in milliseconds.
    pub latency_ms: u64,
}

/// Trait for LLM post-processing providers.
pub trait LlmProvider: Send + Sync {
    /// Format a raw transcript into polished text.
    fn format(
        &self,
        raw_transcript: &str,
        context: &AppContext,
        system_prompt: &str,
    ) -> impl std::future::Future<Output = Result<FormattedText>> + Send;

    /// Check if this provider is currently available.
    fn is_available(&self) -> impl std::future::Future<Output = bool> + Send;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible request/response types (shared by Cerebras & Groq)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    error: Option<ApiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorDetail {
    message: Option<String>,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Helper: send a chat completion request to an OpenAI-compatible endpoint
// ---------------------------------------------------------------------------

async fn send_chat_completion(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_content: &str,
) -> Result<String> {
    let request_body = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_content.to_string(),
            },
        ],
        temperature: 0.1,
    };

    let response = client
        .post(base_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| LlmError::ApiError(format!("network error: {e}")))?;

    let status = response.status();

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();

        // Try to extract a structured error message.
        if let Ok(err_resp) = serde_json::from_str::<ApiErrorResponse>(&body)
            && let Some(detail) = err_resp.error
        {
            let msg = detail.message.unwrap_or_else(|| "unknown".to_string());
            let kind = detail.error_type.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(LlmError::ApiError(format!("rate limited: {msg}")).into());
            }
            return Err(LlmError::ApiError(format!("HTTP {status} ({kind}): {msg}")).into());
        }

        return Err(LlmError::ApiError(format!("HTTP {status}: {body}")).into());
    }

    let completion: ChatCompletionResponse = response
        .json()
        .await
        .map_err(|e| LlmError::ApiError(format!("failed to parse response: {e}")))?;

    let text = completion
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();

    Ok(text)
}

// ---------------------------------------------------------------------------
// Helper: send a chat completion to Gemini (x-goog-api-key header)
// ---------------------------------------------------------------------------

async fn send_gemini_completion(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_content: &str,
) -> Result<String> {
    let request_body = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_content.to_string(),
            },
        ],
        temperature: 0.1,
    };

    let response = client
        .post(base_url)
        .header("x-goog-api-key", api_key)
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
        .map_err(|e| LlmError::ApiError(format!("network error: {e}")))?;

    let status = response.status();

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();

        if let Ok(err_resp) = serde_json::from_str::<ApiErrorResponse>(&body)
            && let Some(detail) = err_resp.error
        {
            let msg = detail.message.unwrap_or_else(|| "unknown".to_string());
            let kind = detail.error_type.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(LlmError::ApiError(format!("rate limited: {msg}")).into());
            }
            return Err(LlmError::ApiError(format!("HTTP {status} ({kind}): {msg}")).into());
        }

        return Err(LlmError::ApiError(format!("HTTP {status}: {body}")).into());
    }

    let completion: ChatCompletionResponse = response
        .json()
        .await
        .map_err(|e| LlmError::ApiError(format!("failed to parse response: {e}")))?;

    let text = completion
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();

    Ok(text)
}

// ---------------------------------------------------------------------------
// Anthropic request/response types (Messages API)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    _block_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorResponse {
    error: Option<AnthropicErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    message: Option<String>,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Helper: send a message to Anthropic Messages API
// ---------------------------------------------------------------------------

async fn send_anthropic_message(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    max_tokens: u32,
    system_prompt: &str,
    user_content: &str,
) -> Result<String> {
    let request_body = AnthropicRequest {
        model: model.to_string(),
        max_tokens,
        system: system_prompt.to_string(),
        messages: vec![AnthropicMessage {
            role: "user".to_string(),
            content: user_content.to_string(),
        }],
    };

    let response = client
        .post(base_url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
        .map_err(|e| LlmError::ApiError(format!("network error: {e}")))?;

    let status = response.status();

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();

        if let Ok(err_resp) = serde_json::from_str::<AnthropicErrorResponse>(&body)
            && let Some(detail) = err_resp.error
        {
            let msg = detail.message.unwrap_or_else(|| "unknown".to_string());
            let kind = detail.error_type.unwrap_or_default();
            if status.as_u16() == 429 {
                return Err(LlmError::ApiError(format!("rate limited: {msg}")).into());
            }
            return Err(LlmError::ApiError(format!("HTTP {status} ({kind}): {msg}")).into());
        }

        return Err(LlmError::ApiError(format!("HTTP {status}: {body}")).into());
    }

    let resp: AnthropicResponse = response
        .json()
        .await
        .map_err(|e| LlmError::ApiError(format!("failed to parse response: {e}")))?;

    let text = resp
        .content
        .into_iter()
        .next()
        .map(|b| b.text)
        .unwrap_or_default();

    Ok(text)
}

// ---------------------------------------------------------------------------
// GeminiProvider — Google Gemini via OpenAI-compatible endpoint
// ---------------------------------------------------------------------------

/// LLM provider for Google Gemini using its OpenAI-compatible endpoint.
///
/// Uses `x-goog-api-key` header for authentication instead of Bearer token.
pub struct GeminiProvider {
    /// Display name for this provider.
    pub name: String,
    /// Full URL to the chat completions endpoint.
    pub base_url: String,
    /// API key for Gemini.
    pub api_key: String,
    /// Model identifier (e.g., "gemini-2.0-flash").
    pub model: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    /// Create a new Gemini provider with the given configuration.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: "gemini".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
                .to_string(),
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

impl LlmProvider for GeminiProvider {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        system_prompt: &str,
    ) -> Result<FormattedText> {
        if self.api_key.is_empty() {
            return Err(LlmError::ProviderUnavailable("Gemini API key not set".into()).into());
        }

        tracing::debug!(
            provider = %self.name,
            model = %self.model,
            url = %self.base_url,
            "sending transcript to Gemini"
        );

        let start = Instant::now();
        let text = send_gemini_completion(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.model,
            system_prompt,
            raw_transcript,
        )
        .await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        tracing::info!(
            provider = %self.name,
            latency_ms,
            "Gemini response received"
        );

        Ok(FormattedText {
            text,
            provider: format!("{}/{}", self.name, self.model),
            latency_ms,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

// ---------------------------------------------------------------------------
// AnthropicProvider — Anthropic Messages API
// ---------------------------------------------------------------------------

/// LLM provider for Anthropic's Messages API.
///
/// Uses a different request/response format from OpenAI-compatible providers.
/// Auth via `x-api-key` header and `anthropic-version` header.
pub struct AnthropicProvider {
    /// Display name for this provider.
    pub name: String,
    /// Full URL to the Messages API endpoint.
    pub base_url: String,
    /// API key for Anthropic.
    pub api_key: String,
    /// Model identifier (e.g., "claude-sonnet-4-20250514").
    pub model: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider with the given configuration.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: "anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1/messages".to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: 1024,
            client: reqwest::Client::new(),
        }
    }

    /// Override the max tokens setting.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

impl LlmProvider for AnthropicProvider {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        system_prompt: &str,
    ) -> Result<FormattedText> {
        if self.api_key.is_empty() {
            return Err(LlmError::ProviderUnavailable("Anthropic API key not set".into()).into());
        }

        tracing::debug!(
            provider = %self.name,
            model = %self.model,
            url = %self.base_url,
            "sending transcript to Anthropic"
        );

        let start = Instant::now();
        let text = send_anthropic_message(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.model,
            self.max_tokens,
            system_prompt,
            raw_transcript,
        )
        .await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        tracing::info!(
            provider = %self.name,
            latency_ms,
            "Anthropic response received"
        );

        Ok(FormattedText {
            text,
            provider: format!("{}/{}", self.name, self.model),
            latency_ms,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

// ---------------------------------------------------------------------------
// OpenAICompatibleProvider — works with ANY OpenAI-compatible API
// ---------------------------------------------------------------------------

/// A generic LLM provider that works with any OpenAI-compatible chat
/// completions API. This covers: Groq, Cerebras, Together, OpenRouter,
/// Fireworks, Ollama, vLLM, LM Studio, and any other provider that
/// implements the `/v1/chat/completions` endpoint.
///
/// # Example providers
///
/// | Provider | Base URL | Notes |
/// |----------|----------|-------|
/// | Groq | `https://api.groq.com/openai/v1/chat/completions` | Ultra-fast |
/// | Cerebras | `https://api.cerebras.ai/v1/chat/completions` | Wafer-scale |
/// | Together | `https://api.together.xyz/v1/chat/completions` | Wide model selection |
/// | OpenRouter | `https://openrouter.ai/api/v1/chat/completions` | Multi-provider routing |
/// | Fireworks | `https://api.fireworks.ai/inference/v1/chat/completions` | Fast OSS models |
/// | Ollama | `http://localhost:11434/v1/chat/completions` | Local, no API key needed |
/// | vLLM | `http://localhost:8000/v1/chat/completions` | Local, no API key needed |
/// | LM Studio | `http://localhost:1234/v1/chat/completions` | Local, no API key needed |
/// | OpenAI | `https://api.openai.com/v1/chat/completions` | GPT models |
pub struct OpenAICompatibleProvider {
    /// Display name for this provider (e.g., "groq", "ollama", "my-server").
    pub name: String,
    /// Full URL to the chat completions endpoint.
    pub base_url: String,
    /// API key (empty string for local providers that don't need auth).
    pub api_key: String,
    /// Model identifier (e.g., "llama-3.1-8b-instant", "gpt-4o-mini").
    pub model: String,
    client: reqwest::Client,
}

impl OpenAICompatibleProvider {
    /// Create a new provider with the given configuration.
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
}

impl LlmProvider for OpenAICompatibleProvider {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        system_prompt: &str,
    ) -> Result<FormattedText> {
        tracing::debug!(
            provider = %self.name,
            model = %self.model,
            url = %self.base_url,
            "sending transcript"
        );

        let start = Instant::now();
        let text = send_chat_completion(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.model,
            system_prompt,
            raw_transcript,
        )
        .await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        tracing::info!(
            provider = %self.name,
            latency_ms,
            "response received"
        );

        Ok(FormattedText {
            text,
            provider: format!("{}/{}", self.name, self.model),
            latency_ms,
        })
    }

    async fn is_available(&self) -> bool {
        // Local providers (empty API key) are always "available" — the
        // server just needs to be running.
        true
    }
}

// ---------------------------------------------------------------------------
// Preset providers (convenience constructors)
// ---------------------------------------------------------------------------

/// Well-known provider presets with their default base URLs.
pub mod presets {
    use super::{AnthropicProvider, GeminiProvider, OpenAICompatibleProvider};

    pub fn groq(api_key: impl Into<String>, model: impl Into<String>) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "groq",
            "https://api.groq.com/openai/v1/chat/completions",
            api_key,
            model,
        )
    }

    pub fn cerebras(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
            api_key,
            model,
        )
    }

    pub fn together(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "together",
            "https://api.together.xyz/v1/chat/completions",
            api_key,
            model,
        )
    }

    pub fn openrouter(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions",
            api_key,
            model,
        )
    }

    pub fn fireworks(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "fireworks",
            "https://api.fireworks.ai/inference/v1/chat/completions",
            api_key,
            model,
        )
    }

    pub fn openai(
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "openai",
            "https://api.openai.com/v1/chat/completions",
            api_key,
            model,
        )
    }

    pub fn ollama(model: impl Into<String>) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "ollama",
            "http://localhost:11434/v1/chat/completions",
            "",
            model,
        )
    }

    pub fn lm_studio(model: impl Into<String>) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "lm-studio",
            "http://localhost:1234/v1/chat/completions",
            "",
            model,
        )
    }

    pub fn vllm(model: impl Into<String>) -> OpenAICompatibleProvider {
        OpenAICompatibleProvider::new(
            "vllm",
            "http://localhost:8000/v1/chat/completions",
            "",
            model,
        )
    }

    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> GeminiProvider {
        GeminiProvider::new(api_key, model)
    }

    pub fn anthropic(api_key: impl Into<String>, model: impl Into<String>) -> AnthropicProvider {
        AnthropicProvider::new(api_key, model)
    }
}

// ---------------------------------------------------------------------------
// Ollama model discovery
// ---------------------------------------------------------------------------

/// Information about a locally available Ollama model.
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelInfo>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelInfo {
    name: String,
    #[serde(default)]
    size: u64,
}

/// Query the local Ollama instance for all available models.
///
/// Returns an empty vec if Ollama is not running or unreachable.
/// Uses the `/api/tags` endpoint at `http://localhost:11434`.
pub fn list_ollama_models() -> Vec<OllamaModel> {
    // Use blocking reqwest since this is called from onboarding (sync context).
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    match client.get("http://localhost:11434/api/tags").send() {
        Ok(resp) if resp.status().is_success() => match resp.json::<OllamaTagsResponse>() {
            Ok(tags) => tags
                .models
                .into_iter()
                .map(|m| OllamaModel {
                    name: m.name,
                    size: m.size,
                })
                .collect(),
            Err(_) => vec![],
        },
        _ => vec![],
    }
}

/// Check if Ollama is running on localhost.
pub fn is_ollama_running() -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .unwrap_or_default();

    client
        .get("http://localhost:11434/api/tags")
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Format a byte size into a human-readable string (e.g., "3.8 GB").
pub fn format_model_size(bytes: u64) -> String {
    if bytes == 0 {
        return String::new();
    }
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{:.1} GB", gb)
    } else {
        let mb = bytes as f64 / 1_048_576.0;
        format!("{:.0} MB", mb)
    }
}

// ---------------------------------------------------------------------------
// Legacy named providers (kept for backwards compat, delegate to generic)
// ---------------------------------------------------------------------------

const CEREBRAS_BASE_URL: &str = "https://api.cerebras.ai/v1/chat/completions";
const CEREBRAS_DEFAULT_MODEL: &str = "llama3.1-8b";

/// Cerebras cloud LLM provider (primary, lowest-latency inference).
pub struct CerebrasProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl CerebrasProvider {
    /// Create a new `CerebrasProvider` with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: CEREBRAS_DEFAULT_MODEL.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Override the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

impl LlmProvider for CerebrasProvider {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        system_prompt: &str,
    ) -> Result<FormattedText> {
        if self.api_key.is_empty() {
            return Err(LlmError::ProviderUnavailable("Cerebras API key not set".into()).into());
        }

        tracing::debug!(model = %self.model, "sending transcript to Cerebras");

        let start = Instant::now();
        let text = send_chat_completion(
            &self.client,
            CEREBRAS_BASE_URL,
            &self.api_key,
            &self.model,
            system_prompt,
            raw_transcript,
        )
        .await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        tracing::info!(latency_ms, "Cerebras response received");

        Ok(FormattedText {
            text,
            provider: format!("cerebras/{}", self.model),
            latency_ms,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

// ---------------------------------------------------------------------------
// GroqProvider
// ---------------------------------------------------------------------------

const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const GROQ_DEFAULT_MODEL: &str = "openai/gpt-oss-20b";

/// Groq cloud LLM provider (secondary, fast inference).
pub struct GroqProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GroqProvider {
    /// Create a new `GroqProvider` with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: GROQ_DEFAULT_MODEL.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Override the default model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

impl LlmProvider for GroqProvider {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        system_prompt: &str,
    ) -> Result<FormattedText> {
        if self.api_key.is_empty() {
            return Err(LlmError::ProviderUnavailable("Groq API key not set".into()).into());
        }

        tracing::debug!(model = %self.model, "sending transcript to Groq");

        let start = Instant::now();
        let text = send_chat_completion(
            &self.client,
            GROQ_BASE_URL,
            &self.api_key,
            &self.model,
            system_prompt,
            raw_transcript,
        )
        .await?;
        let latency_ms = start.elapsed().as_millis() as u64;

        tracing::info!(latency_ms, "Groq response received");

        Ok(FormattedText {
            text,
            provider: format!("groq/{}", self.model),
            latency_ms,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

// ---------------------------------------------------------------------------
// LocalLlamaProvider (stub — llama-cpp-rs not yet integrated)
// ---------------------------------------------------------------------------

/// Local llama.cpp LLM provider for offline/fallback inference.
///
/// **Current status: stub implementation.**
///
/// This provider performs basic text cleanup (trim, capitalize, punctuate) as a
/// placeholder. When `llama-cpp-rs` is added to Cargo.toml, the `format()`
/// method should be replaced with actual model inference:
///
/// ```ignore
/// // TODO: Replace stub with real llama-cpp-rs integration:
/// //   1. Add `llama-cpp-2 = "0.1"` (or similar) to Cargo.toml [dependencies]
/// //   2. In `new()`, load the GGUF model via `LlamaModel::load_from_file()`
/// //   3. In `format()`, create a session, feed the system prompt + transcript,
/// //      and sample tokens to produce the formatted output.
/// //   4. Handle context-window limits and sampling parameters.
/// ```
pub struct LocalLlamaProvider {
    model_path: String,
    model_exists: bool,
}

impl LocalLlamaProvider {
    /// Create a new `LocalLlamaProvider` pointing at a GGUF model file.
    ///
    /// The provider checks whether the file exists at construction time;
    /// `is_available()` will return `false` if it does not.
    pub fn new(model_path: String) -> Self {
        let model_exists = std::path::Path::new(&model_path).exists();
        if !model_exists {
            tracing::warn!(path = %model_path, "local LLM model file not found");
        }
        Self {
            model_path,
            model_exists,
        }
    }

    /// Basic text cleanup used as a placeholder until real LLM inference is
    /// wired up via llama-cpp-rs.
    fn basic_cleanup(text: &str) -> String {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }

        // Capitalize the first character.
        let mut chars = trimmed.chars();
        let first = chars.next().unwrap().to_uppercase().to_string();
        let rest: String = chars.collect();
        let mut result = format!("{first}{rest}");

        // Ensure the text ends with sentence-ending punctuation.
        if let Some(last) = result.chars().last()
            && !matches!(last, '.' | '!' | '?')
        {
            result.push('.');
        }

        result
    }
}

impl LlmProvider for LocalLlamaProvider {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        _system_prompt: &str,
    ) -> Result<FormattedText> {
        if !self.model_exists {
            return Err(LlmError::LocalModelError(format!(
                "model file not found: {}",
                self.model_path
            ))
            .into());
        }

        // TODO: Replace this stub with actual llama-cpp-rs inference.
        // For now, apply basic text cleanup as a placeholder.
        tracing::debug!(path = %self.model_path, "using local LLM stub (basic cleanup)");

        let start = Instant::now();
        let text = Self::basic_cleanup(raw_transcript);
        let latency_ms = start.elapsed().as_millis() as u64;

        Ok(FormattedText {
            text,
            provider: "local/llama-stub".to_string(),
            latency_ms,
        })
    }

    async fn is_available(&self) -> bool {
        self.model_exists
    }
}

// ---------------------------------------------------------------------------
// RawTranscriptFallback
// ---------------------------------------------------------------------------

/// Absolute last-resort provider that returns the raw transcript unchanged.
///
/// This should be the final entry in a [`ProviderChain`] so that the user
/// always gets *something* back, even when cloud APIs and the local LLM are
/// all unavailable.
pub struct RawTranscriptFallback;

impl RawTranscriptFallback {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RawTranscriptFallback {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmProvider for RawTranscriptFallback {
    async fn format(
        &self,
        raw_transcript: &str,
        _context: &AppContext,
        _system_prompt: &str,
    ) -> Result<FormattedText> {
        tracing::debug!("using raw transcript fallback (no formatting)");

        Ok(FormattedText {
            text: raw_transcript.to_string(),
            provider: "raw-fallback".to_string(),
            latency_ms: 0,
        })
    }

    async fn is_available(&self) -> bool {
        true // always available
    }
}

// ---------------------------------------------------------------------------
// ProviderChain
// ---------------------------------------------------------------------------

/// A chain of LLM providers tried in priority order.
///
/// The first provider that succeeds wins; on failure the chain falls through
/// to the next provider. If all providers fail, the last error is returned.
pub struct ProviderChain {
    providers: Vec<Box<dyn LlmProviderBoxed>>,
}

/// Object-safe version of [`LlmProvider`] used internally by [`ProviderChain`].
///
/// Users should not need to implement this trait directly — it is automatically
/// implemented for every type that implements [`LlmProvider`].
#[doc(hidden)]
pub trait LlmProviderBoxed: Send + Sync {
    fn format_boxed<'a>(
        &'a self,
        raw_transcript: &'a str,
        context: &'a AppContext,
        system_prompt: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<FormattedText>> + Send + 'a>>;

    fn is_available_boxed(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>>;
}

impl<T: LlmProvider> LlmProviderBoxed for T {
    fn format_boxed<'a>(
        &'a self,
        raw_transcript: &'a str,
        context: &'a AppContext,
        system_prompt: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<FormattedText>> + Send + 'a>>
    {
        Box::pin(self.format(raw_transcript, context, system_prompt))
    }

    fn is_available_boxed(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        Box::pin(self.is_available())
    }
}

impl ProviderChain {
    /// Create a new empty provider chain.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Append a provider to the chain (lower priority than previously added ones).
    #[allow(clippy::should_implement_trait)]
    pub fn add<P: LlmProvider + 'static>(mut self, provider: P) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// Try each provider in order. Returns the first successful result, or the
    /// last error if all providers fail.
    pub async fn format(
        &self,
        raw_transcript: &str,
        context: &AppContext,
        system_prompt: &str,
    ) -> Result<FormattedText> {
        if self.providers.is_empty() {
            return Err(LlmError::ProviderUnavailable("no providers configured".into()).into());
        }

        let mut last_error: Option<anyhow::Error> = None;

        for provider in &self.providers {
            if !provider.is_available_boxed().await {
                tracing::debug!("provider not available, skipping");
                continue;
            }

            match provider
                .format_boxed(raw_transcript, context, system_prompt)
                .await
            {
                Ok(result) => return Ok(result),
                Err(e) => {
                    tracing::warn!(error = %e, "provider failed, trying next");
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            LlmError::ProviderUnavailable("all providers unavailable".into()).into()
        }))
    }
}

impl Default for ProviderChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cerebras_provider_creation() {
        let provider = CerebrasProvider::new("test-key");
        assert_eq!(provider.api_key, "test-key");
        assert_eq!(provider.model, CEREBRAS_DEFAULT_MODEL);
    }

    #[test]
    fn test_groq_provider_creation() {
        let provider = GroqProvider::new("test-key");
        assert_eq!(provider.api_key, "test-key");
        assert_eq!(provider.model, GROQ_DEFAULT_MODEL);
    }

    #[test]
    fn test_with_model() {
        let provider = CerebrasProvider::new("key").with_model("custom-model");
        assert_eq!(provider.model, "custom-model");
    }

    #[tokio::test]
    async fn test_is_available_with_key() {
        let provider = CerebrasProvider::new("some-key");
        assert!(provider.is_available().await);
    }

    #[tokio::test]
    async fn test_is_available_without_key() {
        let provider = CerebrasProvider::new("");
        assert!(!provider.is_available().await);
    }

    #[tokio::test]
    async fn test_empty_chain_returns_error() {
        let chain = ProviderChain::new();
        let ctx = AppContext {
            app_name: "test".into(),
            bundle_id: None,
        };
        let result = chain.format("hello", &ctx, "system").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_chain_skips_unavailable_providers() {
        let chain = ProviderChain::new()
            .add(CerebrasProvider::new("")) // unavailable
            .add(GroqProvider::new("")); // unavailable

        let ctx = AppContext {
            app_name: "test".into(),
            bundle_id: None,
        };
        let result = chain.format("hello", &ctx, "system").await;
        assert!(result.is_err());
    }

    // -- LocalLlamaProvider tests --

    #[test]
    fn test_local_llama_missing_model() {
        let provider = LocalLlamaProvider::new("/nonexistent/model.gguf".to_string());
        assert!(!provider.model_exists);
    }

    #[tokio::test]
    async fn test_local_llama_unavailable_when_missing() {
        let provider = LocalLlamaProvider::new("/nonexistent/model.gguf".to_string());
        assert!(!provider.is_available().await);
    }

    #[tokio::test]
    async fn test_local_llama_format_fails_when_missing() {
        let provider = LocalLlamaProvider::new("/nonexistent/model.gguf".to_string());
        let ctx = AppContext {
            app_name: "test".into(),
            bundle_id: None,
        };
        let result = provider.format("hello", &ctx, "system").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_basic_cleanup_capitalizes() {
        assert_eq!(
            LocalLlamaProvider::basic_cleanup("hello world"),
            "Hello world."
        );
    }

    #[test]
    fn test_basic_cleanup_preserves_existing_punctuation() {
        assert_eq!(
            LocalLlamaProvider::basic_cleanup("hello world!"),
            "Hello world!"
        );
        assert_eq!(
            LocalLlamaProvider::basic_cleanup("is this a question?"),
            "Is this a question?"
        );
    }

    #[test]
    fn test_basic_cleanup_trims_whitespace() {
        assert_eq!(LocalLlamaProvider::basic_cleanup("  hello  "), "Hello.");
    }

    #[test]
    fn test_basic_cleanup_empty() {
        assert_eq!(LocalLlamaProvider::basic_cleanup(""), "");
        assert_eq!(LocalLlamaProvider::basic_cleanup("   "), "");
    }

    // -- RawTranscriptFallback tests --

    #[tokio::test]
    async fn test_raw_fallback_always_available() {
        let provider = RawTranscriptFallback::new();
        assert!(provider.is_available().await);
    }

    #[tokio::test]
    async fn test_raw_fallback_returns_text_unchanged() {
        let provider = RawTranscriptFallback::new();
        let ctx = AppContext {
            app_name: "test".into(),
            bundle_id: None,
        };
        let result = provider
            .format("  raw text here  ", &ctx, "system")
            .await
            .unwrap();
        assert_eq!(result.text, "  raw text here  ");
        assert_eq!(result.provider, "raw-fallback");
        assert_eq!(result.latency_ms, 0);
    }

    #[tokio::test]
    async fn test_chain_falls_through_to_raw_fallback() {
        let chain = ProviderChain::new()
            .add(CerebrasProvider::new("")) // unavailable
            .add(GroqProvider::new("")) // unavailable
            .add(RawTranscriptFallback::new());

        let ctx = AppContext {
            app_name: "test".into(),
            bundle_id: None,
        };
        let result = chain.format("hello world", &ctx, "system").await.unwrap();
        assert_eq!(result.text, "hello world");
        assert_eq!(result.provider, "raw-fallback");
    }
}
