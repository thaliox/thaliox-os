//! OpenAI provider — real calls to the Chat Completions API.

use async_trait::async_trait;
use serde_json::{Value, json};
use thaliox_core::TamError;

use crate::{Completion, LlmProvider, Message, Role};

// Base URL includes the version segment (OpenAI-SDK style); `/chat/completions`
// is appended. Point `with_base_url` at any compatible gateway, e.g.
// `https://host/v1` or a proxy whose base already carries a version like `…/v3`.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// An [`LlmProvider`] backed by OpenAI's Chat Completions API (or any
/// OpenAI-compatible gateway via [`with_base_url`](Self::with_base_url)).
pub struct OpenAiProvider {
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl OpenAiProvider {
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

    /// Build from the `OPENAI_API_KEY` environment variable.
    pub fn from_env(model: impl Into<String>) -> Result<Self, TamError> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| TamError::Provider("OPENAI_API_KEY not set".into()))?;
        Ok(Self::new(key, model))
    }

    /// Repoint at a compatible gateway (its base should include the version segment).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set the response token ceiling.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Render the Chat Completions request body.
    fn build_body(&self, messages: &[Message]) -> Value {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                json!({"role": role, "content": m.content})
            })
            .collect();
        json!({
            "model": self.model,
            "messages": msgs,
            "max_tokens": self.max_tokens,
        })
    }

    /// Parse a Chat Completions response into a [`Completion`].
    fn parse(v: &Value) -> Result<Completion, TamError> {
        let message = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|choice| choice.get("message"))
            .ok_or_else(|| TamError::Provider(format!("openai: no choices/message in {v}")))?;
        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let usage = v.get("usage");
        let total = usage
            .and_then(|u| u.get("total_tokens"))
            .and_then(|x| x.as_u64())
            .or_else(|| {
                let p = usage
                    .and_then(|u| u.get("prompt_tokens"))
                    .and_then(|x| x.as_u64());
                let c = usage
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|x| x.as_u64());
                Some(p.unwrap_or(0) + c.unwrap_or(0))
            })
            .unwrap_or(0);
        Ok(Completion {
            content,
            tokens: total,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn complete(&self, messages: &[Message]) -> Result<Completion, TamError> {
        let body = self.build_body(messages);
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| TamError::Provider(format!("openai request failed: {e}")))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| TamError::Provider(format!("openai: invalid JSON response: {e}")))?;
        if !status.is_success() {
            return Err(TamError::Provider(format!("openai HTTP {status}: {v}")));
        }
        Self::parse(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_body_maps_roles() {
        let p = OpenAiProvider::new("k", "gpt-4o");
        let body = p.build_body(&[Message::system("sys"), Message::user("hi")]);
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][1]["role"], json!("user"));
        assert_eq!(body["model"], json!("gpt-4o"));
    }

    #[test]
    fn parse_content_and_usage() {
        let v = json!({
            "choices": [{"message": {"role": "assistant", "content": "4"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
        });
        let c = OpenAiProvider::parse(&v).unwrap();
        assert_eq!(c.content, "4");
        assert_eq!(c.tokens, 6);
    }
}
