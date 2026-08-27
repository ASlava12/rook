//! Exposing an MCP server's tools as ordinary [`Tool`]s.
//!
//! The agent loop never learns that a tool came from a subprocess: an MCP tool
//! and a built-in one differ only in what happens inside `call`.

use std::sync::Arc;

use async_trait::async_trait;
use rook_llm::ToolSpec;
use rook_mcp::{Server, ToolDescriptor};

use crate::{Result, Tool, ToolBox, ToolContext, ToolOutcome};

/// Models constrain tool names to `[a-zA-Z0-9_-]`, so servers are namespaced
/// with a double underscore rather than a dot.
pub fn namespaced(server: &str, tool: &str) -> String {
    format!("{}__{}", sanitize(server), sanitize(tool))
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

pub struct McpTool {
    server: Arc<Server>,
    remote_name: String,
    name: String,
    description: String,
    schema: serde_json::Value,
    claims_read_only: bool,
}

impl McpTool {
    pub fn new(server: Arc<Server>, descriptor: ToolDescriptor) -> Self {
        Self {
            name: namespaced(server.name(), &descriptor.name),
            remote_name: descriptor.name,
            description: descriptor.description,
            schema: descriptor.input_schema,
            claims_read_only: descriptor.annotations.read_only,
            server,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: if self.schema.is_object() {
                self.schema.clone()
            } else {
                serde_json::json!({ "type": "object", "properties": {} })
            },
        }
    }

    /// Never read-only: what a server's tool does is not visible from here, so
    /// it is the user's approval policy that decides, not the server's word.
    fn risk(&self, _args: &serde_json::Value) -> crate::policy::Risk {
        crate::policy::Risk::External { name: self.name.clone(), claims_read_only: self.claims_read_only }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        // A failing server is the model's problem to work around, not the
        // turn's to die on, so transport errors come back as tool errors.
        let result = match self.server.call_tool(&self.remote_name, args).await {
            Ok(result) => result,
            Err(e) => return Ok(ToolOutcome::error(e.to_string()).with("server", self.server.name())),
        };

        let text = result.to_text();
        let full = text.len();
        let (windowed, truncated) = window(&text, ctx.max_output_bytes);
        Ok(ToolOutcome {
            content: windowed,
            is_error: result.is_error,
            truncated,
            full_bytes: full,
            meta: Default::default(),
        }
        .with("server", self.server.name()))
    }
}

/// The same rule commands get: a long result loses its middle, not its end.
fn window(text: &str, max: usize) -> (String, bool) {
    (crate::elide_middle(text, max), text.len() > max)
}

impl ToolBox {
    /// Register every tool a connected server advertises.
    pub fn register_server(&mut self, server: Arc<Server>, tools: Vec<ToolDescriptor>) {
        for descriptor in tools {
            self.register(Arc::new(McpTool::new(server.clone(), descriptor)));
        }
    }
}
