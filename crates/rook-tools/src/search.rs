//! Content search across the workspace.

use async_trait::async_trait;
use serde_json::json;

use rook_llm::ToolSpec;

use crate::{Result, Tool, ToolContext, ToolError, ToolOutcome, arg_str, arg_usize};

pub struct Search;

#[async_trait]
impl Tool for Search {
    fn name(&self) -> &str {
        "search"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "Search file contents by regular expression, honouring .gitignore. \
                          Returns file:line:text, capped at `limit` matches."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust regex syntax." },
                    "path": { "type": "string", "default": "." },
                    "glob": { "type": "string", "description": "Only search paths containing this substring." },
                    "limit": { "type": "integer", "default": 200 }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let pattern = arg_str(args, self.name(), "pattern")?;
        let root = ctx.resolve(args.get("path").and_then(|v| v.as_str()).unwrap_or("."))?;
        let glob = args.get("glob").and_then(|v| v.as_str()).map(str::to_string);
        let limit = arg_usize(args, "limit", 200);

        let re = regex::Regex::new(&pattern).map_err(|e| ToolError::Invalid {
            tool: self.name().to_string(),
            message: format!("bad regex {pattern:?}: {e}"),
        })?;

        // The walk is blocking; keep it off the async runtime's worker threads.
        let result = tokio::task::spawn_blocking(move || {
            let mut hits = Vec::new();
            let mut total = 0usize;
            let mut files_scanned = 0usize;
            // Not following links is the default; stated because it is the
            // workspace boundary, and a default is not a decision. `require_git`
            // is not the default: without it a `.gitignore` is silently ignored
            // outside a repository, and a Rook workspace need not be one.
            for entry in
                ignore::WalkBuilder::new(&root).follow_links(false).require_git(false).build().flatten()
            {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let path = entry.path();
                if let Some(g) = &glob
                    && !path.to_string_lossy().contains(g.as_str())
                {
                    continue;
                }
                let Ok(bytes) = std::fs::read(path) else { continue };
                // Skip binaries cheaply rather than regexing megabytes of them.
                if bytes.iter().take(4096).any(|b| *b == 0) {
                    continue;
                }
                files_scanned += 1;
                let text = String::from_utf8_lossy(&bytes);
                for (n, line) in text.lines().enumerate() {
                    if !re.is_match(line) {
                        continue;
                    }
                    total += 1;
                    if hits.len() < limit {
                        let rel = path.strip_prefix(&root).unwrap_or(path);
                        let shown: String = line.chars().take(400).collect();
                        hits.push(format!("{}:{}:{}", rel.display(), n + 1, shown));
                    }
                }
            }
            (hits, total, files_scanned)
        })
        .await
        .map_err(|e| ToolError::Invalid { tool: "search".into(), message: e.to_string() })?;

        let (hits, total, files_scanned) = result;
        let truncated = total > hits.len();
        let mut body = if hits.is_empty() {
            format!("no matches for {pattern:?} in {files_scanned} files")
        } else {
            hits.join("\n")
        };
        if truncated {
            body.push_str(&format!(
                "\n[{} more matches; narrow the pattern or raise limit]",
                total - hits.len()
            ));
        }
        Ok(ToolOutcome { content: body, is_error: false, truncated, full_bytes: 0, meta: Default::default() }
            .with("matches", total as u64)
            .with("files_scanned", files_scanned as u64))
    }
}
