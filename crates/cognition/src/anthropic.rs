//! Anthropic (Claude) provider — real calls to the Messages API.

use async_trait::async_trait;
use serde_json::{Value, json};
use thaliox_core::TamError;

use crate::{Completion, LlmProvider, Message, Role};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// An [`LlmProvider`] backed by Anthropic's Messages API. Point
/// [`with_base_url`](Self::with_base_url) at any compatible gateway (proxy,
/// self-host) to use Claude-compatible endpoints.
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Build a provider with an explicit key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            max_tokens: 1024,
            client: reqwest::Client::new(),
        }
    }

    /// Build from the `ANTHROPIC_API_KEY` environment variable.
    pub fn from_env(model: impl Into<String>) -> Result<Self, TamError> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| TamError::Provider("ANTHROPIC_API_KEY not set".into()))?;
        Ok(Self::new(key, model))
    }

    /// Repoint at a compatible gateway.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the response token ceiling.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Render the Messages API request body. `System` turns are hoisted into the
    /// top-level `system` field; the rest become `messages`.
    fn build_body(&self, messages: &[Message]) -> Value {
        let mut system = String::new();
        let mut msgs: Vec<Value> = Vec::new();
        for m in messages {
            match m.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(&m.content);
                }
                Role::Assistant => msgs.push(json!({"role": "assistant", "content": m.content})),
                Role::User | Role::Tool => msgs.push(json!({"role": "user", "content": m.content})),
            }
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        body
    }

    /// Parse a Messages API response into a [`Completion`] (text + total tokens).
    fn parse(v: &Value) -> Result<Completion, TamError> {
        let blocks = v.get("content").and_then(|c| c.as_array()).ok_or_else(|| {
            TamError::Provider(format!("anthropic: missing content array in {v}"))
        })?;
        let mut content = String::new();
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(t) = b.get("text").and_then(|t| t.as_str())
            {
                content.push_str(t);
            }
        }
        let usage = v.get("usage");
        let input = usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let output = usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        Ok(Completion {
            content,
            tokens: input + output,
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn complete(&self, messages: &[Message]) -> Result<Completion, TamError> {
        let body = self.build_body(messages);
        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| TamError::Provider(format!("anthropic request failed: {e}")))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| TamError::Provider(format!("anthropic: invalid JSON response: {e}")))?;
        if !status.is_success() {
            return Err(TamError::Provider(format!("anthropic HTTP {status}: {v}")));
        }
        Self::parse(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_body_hoists_system() {
        let p = AnthropicProvider::new("k", "claude-sonnet-4-6");
        let body = p.build_body(&[Message::system("be terse"), Message::user("hi")]);
        assert_eq!(body["system"], json!("be terse"));
        assert_eq!(body["model"], json!("claude-sonnet-4-6"));
        assert_eq!(body["messages"][0]["role"], json!("user"));
    }

    #[test]
    fn parse_text_and_usage() {
        let v = json!({
            "content": [{"type": "text", "text": "hello there"}],
            "usage": {"input_tokens": 12, "output_tokens": 8}
        });
        let c = AnthropicProvider::parse(&v).unwrap();
        assert_eq!(c.content, "hello there");
        assert_eq!(c.tokens, 20);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(AnthropicProvider::parse(&json!({"oops": true})).is_err());
    }
}
