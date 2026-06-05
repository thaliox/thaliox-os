//! `web_search` — web search via the Tavily API (built for agents).

use async_trait::async_trait;
use serde_json::{Value, json};
use thaliox_core::{TamError, Tool, ToolResult};

const TAVILY_URL: &str = "https://api.tavily.com/search";

/// The `web_search` tool, backed by Tavily.
pub struct WebSearch {
    api_key: String,
    client: reqwest::Client,
}

impl WebSearch {
    /// Build with an explicit Tavily API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Build from the `TAVILY_API_KEY` environment variable.
    pub fn from_env() -> Result<Self, TamError> {
        let key = std::env::var("TAVILY_API_KEY")
            .map_err(|_| TamError::Provider("TAVILY_API_KEY not set".into()))?;
        Ok(Self::new(key))
    }

    /// Render a Tavily response into a compact, agent-readable summary.
    fn summarize(v: &Value) -> ToolResult {
        let mut out = String::new();
        if let Some(answer) = v.get("answer").and_then(|x| x.as_str())
            && !answer.is_empty()
        {
            out.push_str("答案: ");
            out.push_str(answer);
            out.push('\n');
        }
        if let Some(results) = v.get("results").and_then(|x| x.as_array()) {
            for r in results.iter().take(3) {
                let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("");
                let url = r.get("url").and_then(|x| x.as_str()).unwrap_or("");
                let content: String = r
                    .get("content")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();
                out.push_str(&format!("· {title} ({url})\n  {content}\n"));
            }
        }
        if out.is_empty() {
            out.push_str("(no results)");
        }
        let cost = (out.chars().count() as u64 / 4).max(1);
        ToolResult { output: out, cost }
    }
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    async fn invoke(&self, query: &str) -> Result<ToolResult, TamError> {
        let body = json!({
            "api_key": self.api_key,
            "query": query,
            "max_results": 3,
            "include_answer": true,
        });
        let resp = self
            .client
            .post(TAVILY_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| TamError::Provider(format!("web_search request failed: {e}")))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| TamError::Provider(format!("web_search: invalid JSON: {e}")))?;
        if !status.is_success() {
            return Err(TamError::Provider(format!("web_search HTTP {status}: {v}")));
        }
        Ok(Self::summarize(&v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_answer_and_results() {
        let v = json!({
            "answer": "Rust 是一门系统编程语言。",
            "results": [{
                "title": "Rust",
                "url": "https://rust-lang.org",
                "content": "A language empowering everyone to build reliable and efficient software."
            }]
        });
        let r = WebSearch::summarize(&v);
        assert!(r.output.contains("Rust 是一门系统编程语言"));
        assert!(r.output.contains("rust-lang.org"));
        assert!(r.cost >= 1);
    }

    #[test]
    fn summarize_empty_is_graceful() {
        let r = WebSearch::summarize(&json!({}));
        assert_eq!(r.output, "(no results)");
    }
}
