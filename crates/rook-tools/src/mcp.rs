//! Exposing an MCP server's tools as ordinary [`Tool`]s.
//!
//! The agent loop never learns that a tool came from a subprocess: an MCP tool
//! and a built-in one differ only in what happens inside `call`.

use std::sync::Arc;

use async_trait::async_trait;
use rook_llm::ToolSpec;
use rook_mcp::{Server, ToolDescriptor};

use crate::{Result, Tool, ToolBox, ToolContext, ToolOutcome};

/// Models constrain tool names to `[a-zA-Z0-9_-]` and to 64 characters, so
/// servers are namespaced with a double underscore rather than a dot.
///
/// A package-style server name — `npm:@modelcontextprotocol/server-everything`
/// — sanitises to something long enough that the pair does not fit, and a name
/// that does not fit is not one tool refused: the provider rejects the whole
/// request, so every turn fails while the tool list contains it. The server half
/// gives way, since the tool half is what tells two of them apart, and what is
/// cut is replaced by a digest of the whole name so two long servers do not
/// become one.
pub fn namespaced(server: &str, tool: &str) -> String {
    const MOST: usize = 64;
    let (server, tool) = (sanitize(server), sanitize(tool));
    let room = MOST.saturating_sub(tool.len() + 2);
    if server.len() <= room {
        return format!("{server}__{tool}");
    }
    // Enough of the name to stay recognisable, and enough digest to stay
    // distinct. A tool name so long that neither fits is the server's own
    // problem, and truncating the tool would make two of them the same.
    let digest = format!("{:04x}", crc16(&server));
    let keep = room.saturating_sub(digest.len());
    format!("{}{digest}__{tool}", &server[..keep.min(server.len())])
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Four hex characters of difference, which is all this needs: it separates the
/// handful of servers one agent talks to, not the world's.
fn crc16(text: &str) -> u16 {
    text.bytes().fold(0xffffu16, |crc, byte| {
        (0..8).fold(crc ^ u16::from(byte), |crc, _| match crc & 1 {
            1 => (crc >> 1) ^ 0xa001,
            _ => crc >> 1,
        })
    })
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

#[cfg(test)]
mod tests {
    use super::namespaced;

    /// A name over sixty-four characters is not one tool refused: the provider
    /// rejects the request, so every turn fails while the list contains it.
    #[test]
    fn a_package_style_server_name_still_fits_what_a_model_accepts() {
        let long = "npm:@modelcontextprotocol/server-sequential.thinking";
        let name = namespaced(long, "sequentialthinking");

        assert!(name.len() <= 64, "{} characters: {name}", name.len());
        assert!(name.ends_with("__sequentialthinking"), "the tool half is what tells two apart: {name}");
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{name}");
    }

    /// Cutting the server half to make room would make two long ones the same,
    /// and a call meant for one would go to the other.
    #[test]
    fn two_long_server_names_do_not_become_one() {
        let tool = "search";
        let a = namespaced("npm:@modelcontextprotocol/server-everything-alpha", tool);
        let b = namespaced("npm:@modelcontextprotocol/server-everything-beta", tool);

        assert_ne!(a, b, "{a}");
        assert_eq!(a, namespaced("npm:@modelcontextprotocol/server-everything-alpha", tool), "and stable");
    }

    #[test]
    fn a_short_name_is_left_alone() {
        assert_eq!(namespaced("docs", "search"), "docs__search");
    }
}
