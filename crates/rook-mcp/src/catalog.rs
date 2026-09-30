//! A successful catalog means every advertised page was read. A partial prefix
//! cannot make the omitted tools discoverable through the agent's lazy catalog.
use crate::{McpError, Result, Server, ServerConfig, ToolDescriptor};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Write, time::Duration};

impl ServerConfig {
    /// Shared with offline configuration checks; never opens a connection.
    pub fn catalog_error(&self) -> Option<&'static str> {
        if !(1024..=32 * 1024 * 1024).contains(&self.catalog_max_bytes) {
            Some("catalog_max_bytes must be 1024..=33554432")
        } else if !(1..=16384).contains(&self.catalog_max_tools) {
            Some("catalog_max_tools must be 1..=16384")
        } else if !(1..=256).contains(&self.catalog_max_pages) {
            Some("catalog_max_pages must be 1..=256")
        } else if !(1..=300).contains(&self.catalog_timeout_secs) {
            Some("catalog_timeout_secs must be 1..=300")
        } else {
            None
        }
    }
}

struct Budget(usize);
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::other("catalog byte limit exceeded"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Server {
    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>> {
        let timeout = Duration::from_secs(self.config.catalog_timeout_secs);
        tokio::time::timeout(timeout, self.catalog()).await.map_err(|_| McpError::Timeout {
            server: self.name.clone(),
            method: "tools/list catalog".into(),
            timeout,
            said: String::new(),
        })?
    }

    async fn catalog(&self) -> Result<Vec<ToolDescriptor>> {
        let bad = |message: &str| McpError::Decode {
            server: self.name.clone(),
            method: "tools/list".into(),
            message: message.into(),
        };
        let mut budget = Budget(self.config.catalog_max_bytes);
        let mut tools = Vec::new();
        let mut names = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..self.config.catalog_max_pages {
            let params = cursor.as_ref().map(|c| json!({"cursor":c})).unwrap_or_else(|| json!({}));
            let mut result: Value = self.request("tools/list", Some(params)).await?;
            // Count the entire page (including continuation and ignored fields)
            // without allocating a second serialized copy.
            serde_json::to_writer(&mut budget, &result)
                .map_err(|_| bad("catalog_max_bytes exceeded; no partial catalog installed"))?;
            let Some(Value::Array(page)) = result.get_mut("tools").map(Value::take) else {
                return Err(bad("tools/list must return a tools array"));
            };
            if page.len() > self.config.catalog_max_tools.saturating_sub(tools.len()) {
                return Err(bad("catalog_max_tools exceeded; no partial catalog installed"));
            }
            for item in page {
                let tool: ToolDescriptor =
                    serde_json::from_value(item).map_err(|_| bad("invalid tool descriptor"))?;
                if tool.name.is_empty() || !names.insert(tool.name.clone()) {
                    return Err(bad("empty or duplicate tool name; no partial catalog installed"));
                }
                tools.push(tool);
            }
            cursor = match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(tools),
                Some(Value::String(next)) if !next.is_empty() && next.len() <= 4096 => Some(next.clone()),
                _ => return Err(bad("invalid nextCursor; expected a nonempty string of at most 4096 bytes")),
            };
            if !cursors.insert(cursor.clone()) {
                return Err(bad("tools/list cursor cycle; no partial catalog installed"));
            }
        }
        Err(bad("catalog_max_pages exceeded; no partial catalog installed"))
    }
}
