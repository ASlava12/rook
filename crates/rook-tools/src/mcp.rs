//! Exposing an MCP server's tools as ordinary [`Tool`]s.
//!
//! The agent loop never learns that a tool came from a subprocess: an MCP tool
//! and a built-in one differ only in what happens inside `call`.

use std::sync::Arc;

use async_trait::async_trait;
use rook_llm::ToolSpec;
use rook_mcp::{Server, ToolDescriptor};

use crate::{Result, Tool, ToolContext, ToolOutcome};

mod catalog;
pub use catalog::{CatalogLimits, CatalogSummary};

/// Preserve ordinary names; encode ambiguous or oversized pairs with a digest
/// of the original strings, before sanitization can make two tools identical.
pub fn namespaced(server: &str, tool: &str) -> String {
    use sha2::{Digest, Sha256};
    let clean = |s: &str| -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
    };
    let (left, right) = (clean(server), clean(tool));
    if left == server
        && right == tool
        && !left.contains("__")
        && !right.contains("__")
        && !left.is_empty()
        && !right.is_empty()
        && left.len() + right.len() + 2 <= 64
    {
        return format!("{left}__{right}");
    }
    let mut hash = Sha256::new();
    hash.update((server.len() as u64).to_le_bytes());
    hash.update(server.as_bytes());
    hash.update(tool.as_bytes());
    let digest = format!("{:x}", hash.finalize());
    // All three strings are ASCII; the digest separates pairs even when both
    // human-readable halves are shortened or empty.
    let right = &right[..right.len().min(40)];
    let room = 64 - right.len() - 18;
    format!("{}{}__{right}", &left[..left.len().min(room)], &digest[..16])
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
            parameters: self.schema.clone(),
        }
    }

    fn advertisement(&self, _lazy: bool) -> ToolSpec {
        self.spec()
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
        let images = match images(&result) {
            Ok(images) => images,
            Err(why) => {
                return Ok(ToolOutcome::error(format!("MCP image result refused: {why}"))
                    .with("server", self.server.name()));
            }
        };
        let full = text.len();
        let (windowed, truncated) = window(&text, ctx.max_output_bytes);
        Ok(ToolOutcome {
            images,
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

fn images(result: &rook_mcp::protocol::ToolResult) -> std::result::Result<Vec<rook_llm::Image>, String> {
    let mut images = Vec::new();
    for block in &result.content {
        if let rook_mcp::protocol::Content::Image { data, mime_type } = block {
            if images.len() >= rook_llm::images::MAX_IMAGES_PER_MESSAGE {
                return Err("at most 4 images per tool result; request fewer images".into());
            }
            images.push(rook_llm::Image::from_base64(mime_type, data)?);
        }
    }
    Ok(images)
}

#[cfg(test)]
mod image_tests {
    use super::images;
    use rook_mcp::protocol::{Content, ToolResult};

    #[test]
    fn mcp_image_limits_apply_before_pixels_reach_the_agent() {
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let block = Content::Image { mime_type: "image/png".into(), data: data.into() };
        let mixed = ToolResult {
            content: vec![Content::Text { text: "caption".into() }, block.clone()],
            is_error: false,
        };
        assert_eq!(images(&mixed).unwrap()[0].width, 1);
        assert!(mixed.to_text().contains("caption"));
        assert!(!mixed.to_text().contains(data));
        let over = ToolResult { content: vec![block; 5], is_error: false };
        assert_eq!(over.content.len(), rook_llm::images::MAX_IMAGES_PER_MESSAGE + 1);
        assert!(images(&over).unwrap_err().contains("at most 4"));
        for data in ["not base64".into(), "A".repeat(rook_llm::images::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1)] {
            let bad = ToolResult {
                content: vec![Content::Image { mime_type: "image/png".into(), data }],
                is_error: false,
            };
            assert!(images(&bad).is_err());
        }
    }
}
