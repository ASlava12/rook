//! File tools. Paged rather than capped, and refusing ambiguous edits.

use async_trait::async_trait;
use serde_json::json;

use rook_llm::ToolSpec;

use crate::{Result, Tool, ToolContext, ToolError, ToolOutcome, arg_str, arg_usize};

fn path_arg(args: &serde_json::Value) -> Vec<String> {
    args.get("path").and_then(|v| v.as_str()).map(|p| vec![p.to_string()]).unwrap_or_default()
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a text file. Large files are paged: pass `offset` and `limit` \
                          (in lines) to walk through the rest rather than being refused."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path, relative to the workspace." },
                    "offset": { "type": "integer", "description": "First line to return, 0-based.", "default": 0 },
                    "limit": { "type": "integer", "description": "How many lines to return.", "default": 2000 }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path")?)?;
        let offset = arg_usize(args, "offset", 0);
        let limit = arg_usize(args, "limit", 2000);

        let bytes =
            tokio::fs::read(&path).await.map_err(|e| ToolError::Io { path: path.clone(), source: e })?;
        let total_bytes = bytes.len();

        // Binary files are reported, not pasted into context as mojibake.
        if bytes.iter().take(8192).any(|b| *b == 0) {
            return Ok(ToolOutcome::error(format!(
                "{} looks binary ({total_bytes} bytes); read_file only handles text",
                path.display()
            ))
            .with("binary", true));
        }

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let total_lines = lines.len();
        let end = offset.saturating_add(limit).min(total_lines);
        let slice = if offset >= total_lines { &[][..] } else { &lines[offset..end] };

        let mut body = String::with_capacity(slice.len() * 80);
        for (i, line) in slice.iter().enumerate() {
            body.push_str(&format!("{:>6}\t{}\n", offset + i + 1, line));
        }

        let truncated = end < total_lines || offset > 0;
        if end < total_lines {
            body.push_str(&format!(
                "\n[{} more lines; call read_file again with offset={end}]\n",
                total_lines - end
            ));
        }

        Ok(ToolOutcome {
            content: body,
            is_error: false,
            truncated,
            full_bytes: total_bytes,
            meta: Default::default(),
        }
        .with("total_lines", total_lines as u64)
        .with("returned_lines", slice.len() as u64))
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given contents.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path")?)?;
        let content = arg_str(args, self.name(), "content")?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::Io { path: parent.to_path_buf(), source: e })?;
        }
        let existed = path.exists();
        tokio::fs::write(&path, &content)
            .await
            .map_err(|e| ToolError::Io { path: path.clone(), source: e })?;
        Ok(ToolOutcome::ok(format!(
            "{} {} ({} bytes)",
            if existed { "overwrote" } else { "created" },
            path.display(),
            content.len()
        ))
        .with("created", !existed))
    }

    fn touched_paths(&self, args: &serde_json::Value) -> Vec<String> {
        path_arg(args)
    }
}

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace an exact string in a file. `old` must appear exactly once, \
                          so an ambiguous edit is rejected rather than applied to the wrong place."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old": { "type": "string", "description": "Exact text to replace, including indentation." },
                    "new": { "type": "string" },
                    "replace_all": { "type": "boolean", "default": false }
                },
                "required": ["path", "old", "new"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path")?)?;
        let old = arg_str(args, self.name(), "old")?;
        let new = arg_str(args, self.name(), "new")?;
        let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::Io { path: path.clone(), source: e })?;
        let count = text.matches(&old).count();

        if count == 0 {
            return Ok(ToolOutcome::error(format!(
                "no occurrence of that text in {}. Read the file again — it may have changed.",
                path.display()
            )));
        }
        if count > 1 && !replace_all {
            return Ok(ToolOutcome::error(format!(
                "that text appears {count} times in {}. Include more surrounding context to \
                 make it unique, or pass replace_all.",
                path.display()
            ))
            .with("occurrences", count as u64));
        }

        let updated = if replace_all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
        tokio::fs::write(&path, &updated)
            .await
            .map_err(|e| ToolError::Io { path: path.clone(), source: e })?;
        Ok(ToolOutcome::ok(format!(
            "replaced {} occurrence(s) in {}",
            if replace_all { count } else { 1 },
            path.display()
        ))
        .with("occurrences", count as u64))
    }

    fn touched_paths(&self, args: &serde_json::Value) -> Vec<String> {
        path_arg(args)
    }
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List a directory, honouring .gitignore. Output is capped.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "default": "." },
                    "depth": { "type": "integer", "default": 2 },
                    "limit": { "type": "integer", "default": 500 }
                }
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = ctx.resolve(raw)?;
        let depth = arg_usize(args, "depth", 2);
        let limit = arg_usize(args, "limit", 500);

        let mut entries = Vec::new();
        let mut total = 0usize;
        // Not following links is the default; stated because it is the
        // workspace boundary, and a default is not a decision.
        for entry in
            ignore::WalkBuilder::new(&root).max_depth(Some(depth)).follow_links(false).build().flatten()
        {
            if entry.depth() == 0 {
                continue;
            }
            total += 1;
            if entries.len() >= limit {
                continue;
            }
            let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            entries.push(format!("{}{}", rel.display(), if is_dir { "/" } else { "" }));
        }
        entries.sort();
        let truncated = total > entries.len();
        let mut body = entries.join("\n");
        if truncated {
            body.push_str(&format!("\n[{} more entries not shown]", total - entries.len()));
        }
        Ok(ToolOutcome { content: body, is_error: false, truncated, full_bytes: 0, meta: Default::default() }
            .with("entries", total as u64))
    }
}
