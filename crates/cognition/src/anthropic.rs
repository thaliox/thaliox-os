//! Anthropic (Claude) provider — Messages API with tool use.

use async_trait::async_trait;
use serde_json::{Value, json};
use thaliox_core::TamError;

use crate::{Completion, LlmProvider, Message, Role, ToolCall, ToolSpec};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// An [`LlmProvider`] backed by Anthropic's Messages API (or a compatible
/// gateway via [`with_base_url`](Self::with_base_url)).
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl AnthropicProvider {
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
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| TamError::Provider("ANTHROPIC_API_KEY not set".into()))?;
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

    /// Render the Messages API body. System turns hoist into `system`; assistant
    /// tool calls become `tool_use` blocks; tool results coalesce into a single
    /// following user turn of `tool_result` blocks (per the API).
    fn build_body(&self, messages: &[Message], tools: &[ToolSpec]) -> Value {
        let mut system = String::new();
        let mut msgs: Vec<Value> = Vec::new();
        let mut pending: Vec<Value> = Vec::new();
        for m in messages {
            if m.role != Role::Tool && !pending.is_empty() {
                msgs.push(json!({"role": "user", "content": std::mem::take(&mut pending)}));
            }
            match m.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(&m.content);
                }
                Role::User => msgs.push(json!({"role": "user", "content": m.content})),
                Role::Assistant if m.tool_calls.is_empty() => {
                    msgs.push(json!({"role": "assistant", "content": m.content}))
                }
                Role::Assistant => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(json!({"type": "text", "text": m.content}));
                    }
                    for tc in &m.tool_calls {
                        let input: Value =
                            serde_json::from_str(&tc.arguments).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use", "id": tc.id, "name": tc.name, "input": input,
                        }));
                    }
                    msgs.push(json!({"role": "assistant", "content": blocks}));
                }
                Role::Tool => pending.push(json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                })),
            }
        }
        if !pending.is_empty() {
            msgs.push(json!({"role": "user", "content": pending}));
        }

        let tools_json: Vec<Value> = tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": msgs,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !tools_json.is_empty() {
            body["tools"] = json!(tools_json);
        }
        body
    }

    fn parse(v: &Value) -> Result<Completion, TamError> {
        let blocks = v.get("content").and_then(|c| c.as_array()).ok_or_else(|| {
            TamError::Provider(format!("anthropic: missing content array in {v}"))
        })?;
        let mut content = String::new();
        let mut tool_calls = Vec::new();
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        content.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                    tool_calls.push(ToolCall {
                        id: b
                            .get("id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: b
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: input.to_string(),
                    });
                }
                _ => {}
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
            tool_calls,
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

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<Completion, TamError> {
        let body = self.build_body(messages, tools);
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

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "fetch".into(),
            description: "GET a URL".into(),
            input_schema: json!({"type": "object", "properties": {"input": {"type": "string"}}}),
        }
    }

    #[test]
    fn build_body_advertises_tools_and_renders_tool_turns() {
        let p = AnthropicProvider::new("k", "claude");
        let body = p.build_body(
            &[
                Message::system("be terse"),
                Message::user("fetch example.com"),
                Message::assistant_with_tools(
                    "",
                    vec![ToolCall {
                        id: "tu_1".into(),
                        name: "fetch".into(),
                        arguments: r#"{"input":"https://example.com"}"#.into(),
                    }],
                ),
                Message::tool_result("tu_1", "<html>..."),
            ],
            &[tool()],
        );
        assert_eq!(body["system"], json!("be terse"));
        assert_eq!(body["tools"][0]["name"], json!("fetch"));
        // system is hoisted; messages = [user, assistant(tool_use), user(tool_result)].
        assert_eq!(body["messages"][1]["content"][0]["type"], json!("tool_use"));
        assert_eq!(
            body["messages"][2]["content"][0]["type"],
            json!("tool_result")
        );
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            json!("tu_1")
        );
    }

    #[test]
    fn parse_text_and_tool_use() {
        let v = json!({
            "content": [
                {"type": "text", "text": "let me fetch"},
                {"type": "tool_use", "id": "tu_9", "name": "fetch", "input": {"input": "https://x"}}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 6}
        });
        let c = AnthropicProvider::parse(&v).unwrap();
        assert_eq!(c.content, "let me fetch");
        assert_eq!(c.tokens, 16);
        assert_eq!(c.tool_calls[0].name, "fetch");
        assert_eq!(c.tool_calls[0].id, "tu_9");
    }
}
