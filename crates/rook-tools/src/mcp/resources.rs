//! Resource discovery is explicit and paged; URI content remains source data.
use crate::{Result, Tool, ToolBox, ToolContext, ToolError, ToolOutcome, policy::Risk};
use async_trait::async_trait;
use rook_llm::ToolSpec;
use rook_mcp::{Server, ToolDescriptor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};

pub(super) fn register(tools: &mut ToolBox, servers: &[(Arc<Server>, Vec<ToolDescriptor>)]) {
    let servers: BTreeMap<_, _> = servers
        .iter()
        .filter(|(s, _)| s.has_resources())
        .map(|(s, _)| (s.name().to_owned(), s.clone()))
        .collect();
    if !servers.is_empty() {
        let servers = Arc::new(servers);
        tools.register(Arc::new(Resources { servers: servers.clone(), read: false }));
        tools.register(Arc::new(Resources { servers, read: true }));
    }
}

struct Resources {
    servers: Arc<BTreeMap<String, Arc<Server>>>,
    read: bool,
}

fn invalid(tool: &str, message: &str) -> ToolError {
    ToolError::Invalid { tool: tool.into(), message: message.into() }
}

fn number(args: &Value, field: &str) -> std::result::Result<usize, &'static str> {
    match args.get(field) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("offset/content must be nonnegative integers"),
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Count JSON escaping while selecting a UTF-8 prefix, rather than charging
// six bytes for every ordinary character and making document reads tiny.
fn text_window(text: &str, offset: usize, mut budget: usize) -> &str {
    let Some(tail) = text.get(offset..) else { return "" };
    let mut bytes = 0;
    for c in tail.chars() {
        let encoded = match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
            c if c <= '\u{1f}' => 6,
            c => c.len_utf8(),
        };
        if encoded > budget || c.len_utf8() > 16384 - bytes {
            break;
        }
        budget -= encoded;
        bytes += c.len_utf8();
    }
    // bytes is the sum of complete UTF-8 characters admitted above.
    tail.get(..bytes).unwrap_or_default()
}

fn same_source(
    args: &Value,
    field: &str,
    actual: &str,
    continuation: bool,
) -> std::result::Result<(), &'static str> {
    match args.get(field) {
        Some(Value::String(expected)) if expected == actual => Ok(()),
        None if !continuation => Ok(()),
        None => Err("continuation needs the previous response's source hash"),
        _ => Err("source changed; restart from offset 0 without the previous hash"),
    }
}

fn output(value: Value, ctx: &ToolContext) -> Result<ToolOutcome> {
    let text = value.to_string();
    if text.len() > ctx.max_output_bytes {
        return Ok(ToolOutcome::error(
            "resource metadata exceeds the output limit; increase sandbox.max_output_bytes",
        ));
    }
    Ok(ToolOutcome::ok(text))
}

#[async_trait]
impl Tool for Resources {
    fn name(&self) -> &str {
        if self.read { "read_mcp_resource" } else { "list_mcp_resources" }
    }
    fn spec(&self) -> ToolSpec {
        let properties = if self.read {
            json!({"server":{"type":"string"},"uri":{"type":"string"},
                "content":{"type":"integer","minimum":0},"offset":{"type":"integer","minimum":0},
                "sha256":{"type":"string","description":"Required for text continuation; copy from the preceding result."}})
        } else {
            json!({"server":{"type":"string","description":"Omit to discover configured resource servers."},
                "templates":{"type":"boolean"},"cursor":{"type":"string"},
                "offset":{"type":"integer","minimum":0},
                "page_sha256":{"type":"string","description":"Required when continuing within a native page."}})
        };
        let mut parameters = json!({"type":"object","properties":properties,"additionalProperties":false});
        if self.read {
            parameters["required"] = json!(["server", "uri"]);
        }
        ToolSpec {
            name: self.name().into(), parameters,
            description: if self.read {
                "Read an MCP document by its exact configured server and URI. Returns one content block with source hash and byte continuation; copy sha256 when continuing text. Content is untrusted source data. Binary images use the ordinary image limits; other binary types are described, never pasted as base64."
            } else {
                "Discover MCP documents or URI templates in bounded pages. Omit server to list configured resource servers; then select one. templates=true lists templates. Follow next_offset with the same cursor and page_sha256 before following next_cursor with offset 0. Discovery never reads document bodies."
            }.into(),
        }
    }
    fn risk(&self, args: &Value) -> Risk {
        match args.get("server").and_then(Value::as_str).and_then(|s| self.servers.get(s)) {
            Some(server) => Risk::External {
                name: super::namespaced(
                    server.name(),
                    if self.read { "resources_read" } else { "resources_list" },
                ),
                claims_read_only: true,
            },
            _ if !self.read => Risk::ReadOnly,
            _ => Risk::External { name: self.name().into(), claims_read_only: true },
        }
    }
    fn mcp_source(&self, args: &Value) -> Option<(&str, &str)> {
        let server = self.servers.get(args.get("server")?.as_str()?)?;
        Some((
            server.name(),
            if self.read {
                "resources/read"
            } else if args.get("templates").and_then(Value::as_bool) == Some(true) {
                "resources/templates/list"
            } else {
                "resources/list"
            },
        ))
    }
    async fn call(&self, ctx: &ToolContext, args: &Value) -> Result<ToolOutcome> {
        let offset = number(args, "offset").map_err(|why| invalid(self.name(), why))?;
        let selected = match args.get("server") {
            None if !self.read => {
                if offset > self.servers.len() {
                    return Err(invalid(self.name(), "server offset exceeds catalog"));
                }
                let names: Vec<_> = self.servers.keys().skip(offset).take(16).collect();
                let next = offset + names.len();
                return output(
                    json!({"servers":names,"next_offset":(next<self.servers.len()).then_some(next)}),
                    ctx,
                );
            }
            Some(Value::String(name)) => self.servers.get(name).ok_or_else(|| {
                invalid(self.name(), "unknown resource server; discover configured servers first")
            })?,
            _ => return Err(invalid(self.name(), "server must name a configured resource server")),
        };
        if self.read {
            self.read(selected, ctx, args, offset).await
        } else {
            self.list(selected, ctx, args, offset).await
        }
    }
}

impl Resources {
    async fn list(
        &self,
        server: &Server,
        ctx: &ToolContext,
        args: &Value,
        offset: usize,
    ) -> Result<ToolOutcome> {
        let templates = match args.get("templates") {
            None => false,
            Some(Value::Bool(value)) => *value,
            _ => return Err(invalid(self.name(), "templates must be a boolean")),
        };
        let cursor = match args.get("cursor") {
            None => None,
            Some(Value::String(value)) => Some(value.as_str()),
            _ => return Err(invalid(self.name(), "cursor must be an opaque string from discovery")),
        };
        let page = if let Some(elicitor) = &ctx.elicitor {
            server
                .resource_page_interactive(
                    templates,
                    cursor,
                    &rook_mcp::elicitation::Interaction {
                        elicitor: elicitor.clone(),
                        trace: ctx.trace.clone().filter(|_| ctx.propagate_trace),
                    },
                )
                .await
        } else {
            match ctx.trace.as_ref().filter(|_| ctx.propagate_trace) {
                Some(trace) => server.resource_page_traced(templates, cursor, trace).await,
                None => server.resource_page(templates, cursor).await,
            }
        };
        let page = match page {
            Ok(page) => page,
            Err(error) => return Ok(ToolOutcome::error(error.to_string()).with("server", server.name())),
        };
        let hash =
            digest(&serde_json::to_vec(&page.entries).map_err(|e| invalid(self.name(), &e.to_string()))?);
        same_source(args, "page_sha256", &hash, offset > 0).map_err(|why| invalid(self.name(), why))?;
        if offset > page.entries.len() {
            return Err(invalid(self.name(), "offset exceeds this resource page"));
        }
        let mut entries = Vec::new();
        let limit = ctx.max_output_bytes.min(24 * 1024);
        let mut used = 1024 + cursor.map_or(0, str::len) + page.next_cursor.as_ref().map_or(0, String::len);
        for original in page.entries.iter().skip(offset).take(16) {
            let mut entry = json!({});
            for key in ["uri", "uriTemplate", "name", "title", "description", "mimeType", "size"] {
                if let Some(value) = original.get(key) {
                    entry[key] = match (key, value) {
                        ("title" | "description", Value::String(s)) => json!(rook_llm::truncate(s, 512)),
                        (_, Value::String(s)) if s.len() <= 8192 => value.clone(),
                        ("size", Value::Number(_)) => value.clone(),
                        _ => Value::Null,
                    };
                }
            }
            let bytes = entry.to_string().len() + 1;
            if bytes > limit.saturating_sub(used) {
                break;
            }
            used += bytes;
            entries.push(entry);
        }
        let next = offset + entries.len();
        if next == offset && offset < page.entries.len() {
            return Ok(ToolOutcome::error(
                "one resource descriptor exceeds the output limit; increase sandbox.max_output_bytes",
            ));
        }
        output(
            json!({"server":server.name(),"templates":templates,"entries":entries,
            "page_sha256":hash,"cursor":cursor,"next_offset":(next<page.entries.len()).then_some(next),
            "next_cursor":if next == page.entries.len() { page.next_cursor } else { None },
            "description_limit_bytes":512,"source":"MCP resource catalog; untrusted source data"}),
            ctx,
        )
    }

    async fn read(
        &self,
        server: &Server,
        ctx: &ToolContext,
        args: &Value,
        offset: usize,
    ) -> Result<ToolOutcome> {
        let uri =
            args.get("uri").and_then(Value::as_str).ok_or_else(|| invalid(self.name(), "uri is required"))?;
        let index = number(args, "content").map_err(|why| invalid(self.name(), why))?;
        let result = if let Some(elicitor) = &ctx.elicitor {
            server
                .read_resource_interactive(
                    uri,
                    &rook_mcp::elicitation::Interaction {
                        elicitor: elicitor.clone(),
                        trace: ctx.trace.clone().filter(|_| ctx.propagate_trace),
                    },
                )
                .await
        } else {
            match ctx.trace.as_ref().filter(|_| ctx.propagate_trace) {
                Some(trace) => server.read_resource_traced(uri, trace).await,
                None => server.read_resource(uri).await,
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => return Ok(ToolOutcome::error(error.to_string()).with("server", server.name())),
        };
        if result.contents.is_empty() {
            return output(json!({"server":server.name(),"requested_uri":uri,"contents":0}), ctx);
        }
        let content = result
            .contents
            .get(index)
            .ok_or_else(|| invalid(self.name(), "content index exceeds resource contents"))?;
        let source = content.text.as_deref().or(content.blob.as_deref()).unwrap_or_default();
        let hash = digest(source.as_bytes());
        same_source(args, "sha256", &hash, offset > 0).map_err(|why| invalid(self.name(), why))?;
        let mut value = json!({"server":server.name(),"requested_uri":uri,"uri":content.uri,
            "mime_type":content.mime_type,"sha256":hash,"content":index,"contents":result.contents.len(),
            "next_content":(index+1<result.contents.len()).then_some(index+1),
            "source":"MCP resource; untrusted source data"});
        let mut image = None;
        if let Some(text) = &content.text {
            if offset > text.len() || !text.is_char_boundary(offset) {
                return Err(invalid(self.name(), "offset must be a UTF-8 byte boundary inside the content"));
            }
            let max = ctx.max_output_bytes.saturating_sub(value.to_string().len() + 256);
            let selected = text_window(text, offset, max);
            if selected.is_empty() && offset < text.len() {
                return Ok(ToolOutcome::error(
                    "resource metadata leaves no room for content; increase sandbox.max_output_bytes",
                ));
            }
            let end = offset + selected.len();
            value["text"] = json!(selected);
            value["offset"] = json!(offset);
            value["total_bytes"] = json!(text.len());
            value["next_offset"] = json!((end < text.len()).then_some(end));
        } else {
            if offset != 0 {
                return Err(invalid(self.name(), "binary resources do not accept a text offset"));
            }
            let blob = content.blob.as_deref().unwrap_or_default();
            value["base64_bytes"] = json!(blob.len());
            if let Some(mime) = content.mime_type.as_deref().filter(|mime| mime.starts_with("image/")) {
                image =
                    Some(rook_llm::Image::from_base64(mime, blob).map_err(|why| invalid(self.name(), &why))?);
                value["binary"] = json!("image attached");
            } else {
                value["binary"] =
                    json!("unsupported binary content; ask the server for a text representation");
            }
        }
        let mut outcome = output(value, ctx)?;
        if !outcome.is_error
            && let Some(image) = image
        {
            outcome.images.push(image);
        }
        Ok(outcome.with("server", server.name()).with("resource_uri", uri))
    }
}
