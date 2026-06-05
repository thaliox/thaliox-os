//! `fetch` — HTTP GET a URL and return its body (truncated).

use async_trait::async_trait;
use thaliox_core::{TamError, Tool, ToolResult};

/// Cap the returned body so a fetch can't blow up the agent's context.
const MAX_CHARS: usize = 4000;

/// The `fetch` tool: GET a URL, return the body text.
pub struct Fetch {
    client: reqwest::Client,
}

impl Fetch {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for Fetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Fetch {
    fn name(&self) -> &str {
        "fetch"
    }

    async fn invoke(&self, url: &str) -> Result<ToolResult, TamError> {
        let resp = self
            .client
            .get(url)
            .header("user-agent", "thaliox-tools/0.0")
            .send()
            .await
            .map_err(|e| TamError::Provider(format!("fetch '{url}' failed: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| TamError::Provider(format!("fetch body: {e}")))?;
        if !status.is_success() {
            return Err(TamError::Provider(format!("fetch HTTP {status} for {url}")));
        }
        let output: String = body.chars().take(MAX_CHARS).collect();
        let cost = (output.chars().count() as u64 / 4).max(1);
        Ok(ToolResult { output, cost })
    }
}
