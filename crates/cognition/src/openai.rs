//! OpenAI provider — Chat Completions API with tool calls.

use async_trait::async_trait;
use serde_json::{Value, json};
use thaliox_core::TamError;

use crate::{Completion, LlmProvider, Message, Role, ToolCall, ToolSpec};

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
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            model: model.into(),
            max_tokens: 1024,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env(model: impl Into<String>) -> Result<Self, TamError> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| TamError::Provider("OPENAI_API_KEY not set".into()))?;
        Ok(Self::new(key, model))
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    fn build_body(&self, messages: &[Message], tools: &[ToolSpec]) -> Value {
        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| match m.role {
                Role::System => json!({"role": "system", "content": m.content}),
                Role::User => json!({"role": "user", "content": m.content}),
                Role::Tool => json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }),
                Role::Assistant if m.tool_calls.is_empty() => {
                    json!({"role": "assistant", "content": m.content})
                }
                Role::Assistant => {
                    let calls: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {"name": tc.name, "arguments": tc.arguments},
                            })
                        })
                        .collect();
                    let content = if m.content.is_empty() {
                        Value::Null
                    } else {
                        json!(m.content)
                    };
                    json!({"role": "assistant", "content": content, "tool_calls": calls})
                }
            })
            .collect();

        let mut body = json!({
            "model": self.model,
            "messages": msgs,
            "max_tokens": self.max_tokens,
        });
        if !tools.is_empty() {
            let tools_json: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tools_json);
            body["tool_choice"] = json!("auto");
        }
        body
    }

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
        let mut tool_calls = Vec::new();
        if let Some(tcs) = message.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                if let Some(f) = tc.get("function") {
                    tool_calls.push(ToolCall {
                        id: tc
                            .get("id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: f
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: f
                            .get("arguments")
                            .and_then(|x| x.as_str())
                            .unwrap_or("{}")
                            .to_string(),
                    });
                }
            }
        }
        let usage = v.get("usage");
        let total = usage
            .and_then(|u| u.get("total_tokens"))
            .and_then(|x| x.as_u64())
            .unwrap_or_else(|| {
                let p = usage
                    .and_then(|u| u.get("prompt_tokens"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                let c = usage
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                p + c
            });
        Ok(Completion {
            content,
            tokens: total,
            tool_calls,
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

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<Completion, TamError> {
        let body = self.build_body(messages, tools);
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
    fn build_body_advertises_tools_and_renders_tool_turns() {
        let p = OpenAiProvider::new("k", "gpt-4o");
        let body = p.build_body(
            &[
                Message::user("fetch it"),
                Message::assistant_with_tools(
                    "",
                    vec![ToolCall {
                        id: "call_1".into(),
                        name: "fetch".into(),
                        arguments: r#"{"input":"https://x"}"#.into(),
                    }],
                ),
                Message::tool_result("call_1", "<html>"),
            ],
            &[ToolSpec {
                name: "fetch".into(),
                description: "GET".into(),
                input_schema: json!({"type": "object"}),
            }],
        );
        assert_eq!(body["tool_choice"], json!("auto"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("fetch"));
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(body["messages"][2]["role"], json!("tool"));
        assert_eq!(body["messages"][2]["tool_call_id"], json!("call_1"));
    }

    #[test]
    fn parse_content_and_tool_calls() {
        let v = json!({
            "choices": [{"message": {"content": null, "tool_calls": [{
                "id": "call_7", "type": "function",
                "function": {"name": "fetch", "arguments": "{\"input\":\"https://x\"}"}
            }]}}],
            "usage": {"total_tokens": 30}
        });
        let c = OpenAiProvider::parse(&v).unwrap();
        assert_eq!(c.tokens, 30);
        assert_eq!(c.tool_calls[0].name, "fetch");
        assert_eq!(c.tool_calls[0].id, "call_7");
    }
}
