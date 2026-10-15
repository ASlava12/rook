//! Explicit resource requests; discovery never downloads document bodies.
use crate::{McpError, Result, Server};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;

const MAX_READ_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONTENTS: usize = 64;

#[derive(Debug, Serialize)]
pub struct ResourcePage {
    pub entries: Vec<Value>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceRead {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContent {
    pub uri: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob: Option<String>,
}

struct Budget(usize);
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::other("resource reply exceeds its byte limit"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Server {
    /// Capability from this connection's handshake, never from a tool result.
    pub fn has_resources(&self) -> bool {
        self.info.capabilities.get("resources").is_some_and(Value::is_object)
    }

    /// One native page. The caller owns pagination; opaque cursors are not URLs.
    pub async fn resource_page(&self, templates: bool, cursor: Option<&str>) -> Result<ResourcePage> {
        self.resource_page_inner(templates, cursor, None).await
    }

    pub async fn resource_page_traced(
        &self,
        templates: bool,
        cursor: Option<&str>,
        trace: &rook_llm::TraceContext,
    ) -> Result<ResourcePage> {
        self.resource_page_inner(templates, cursor, Some(trace)).await
    }

    async fn resource_page_inner(
        &self,
        templates: bool,
        cursor: Option<&str>,
        trace: Option<&rook_llm::TraceContext>,
    ) -> Result<ResourcePage> {
        let method = if templates { "resources/templates/list" } else { "resources/list" };
        self.resource_support(method)?;
        if cursor.is_some_and(|s| s.is_empty() || s.len() > 4096) {
            return Err(self.resource_error(method, "cursor must contain 1..4096 bytes"));
        }
        let mut params = json!({});
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        if let Some(trace) = trace {
            params = crate::trace_params(params, trace);
        }
        let mut value: Value = self.request(method, Some(params)).await?;
        self.resource_budget(method, &value, self.config.catalog_max_bytes)?;
        let field = if templates { "resourceTemplates" } else { "resources" };
        let Some(Value::Array(entries)) = value.get_mut(field).map(Value::take) else {
            return Err(self.resource_error(method, "resource catalog must contain the requested array"));
        };
        if entries.len() > self.config.catalog_max_tools {
            return Err(self.resource_error(method, "resource page exceeds catalog_max_tools"));
        }
        let identity = if templates { "uriTemplate" } else { "uri" };
        let mut seen = std::collections::BTreeSet::new();
        for entry in &entries {
            let valid = |field: &str, max: usize| {
                entry
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
            };
            if !valid(identity, 8192)
                || !valid("name", 1024)
                || !seen.insert(entry[identity].as_str().unwrap_or_default())
            {
                return Err(self.resource_error(method, "resource identity/name is invalid or repeated"));
            }
        }
        let next_cursor = match value.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() && s.len() <= 4096 && Some(s.as_str()) != cursor => {
                Some(s.clone())
            }
            _ => return Err(self.resource_error(method, "invalid or unchanged nextCursor")),
        };
        Ok(ResourcePage { entries, next_cursor })
    }

    /// Read only through the selected server. A resource URI is never fetched by
    /// Rook's web/file transports and cannot change the connection's authority.
    pub async fn read_resource(&self, uri: &str) -> Result<ResourceRead> {
        self.read_resource_inner(uri, None).await
    }

    pub async fn read_resource_traced(
        &self,
        uri: &str,
        trace: &rook_llm::TraceContext,
    ) -> Result<ResourceRead> {
        self.read_resource_inner(uri, Some(trace)).await
    }

    async fn read_resource_inner(
        &self,
        uri: &str,
        trace: Option<&rook_llm::TraceContext>,
    ) -> Result<ResourceRead> {
        self.resource_support("resources/read")?;
        if uri.is_empty() || uri.len() > 8192 || uri.chars().any(char::is_control) {
            return Err(
                self.resource_error("resources/read", "URI must contain 1..8192 bytes without controls")
            );
        }
        let params = match trace {
            Some(trace) => crate::trace_params(json!({"uri":uri}), trace),
            None => json!({"uri":uri}),
        };
        let value: Value = self.request("resources/read", Some(params)).await?;
        self.resource_budget("resources/read", &value, MAX_READ_BYTES)?;
        let Some(contents) = value.get("contents").and_then(Value::as_array) else {
            return Err(self.resource_error("resources/read", "missing contents array"));
        };
        if contents.len() > MAX_CONTENTS {
            return Err(self.resource_error("resources/read", "resource has more than 64 content blocks"));
        }
        let result: ResourceRead = serde_json::from_value(value)
            .map_err(|_| self.resource_error("resources/read", "invalid resource content"))?;
        for content in &result.contents {
            if content.uri.is_empty()
                || content.uri.len() > 8192
                || content.uri.chars().any(char::is_control)
                || content
                    .mime_type
                    .as_ref()
                    .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
                || content.text.is_some() == content.blob.is_some()
            {
                return Err(self
                    .resource_error("resources/read", "each block needs URI and exactly one of text/blob"));
            }
        }
        Ok(result)
    }

    fn resource_support(&self, method: &str) -> Result<()> {
        if self.has_resources() {
            Ok(())
        } else {
            Err(self.resource_error(method, "server did not advertise resources on this connection"))
        }
    }
    fn resource_error(&self, method: &str, message: &str) -> McpError {
        McpError::Decode { server: self.name.clone(), method: method.into(), message: message.into() }
    }
    fn resource_budget(&self, method: &str, value: &Value, bytes: usize) -> Result<()> {
        serde_json::to_writer(&mut Budget(bytes), value)
            .map_err(|_| self.resource_error(method, "resource reply exceeds its configured byte limit"))
    }
}
