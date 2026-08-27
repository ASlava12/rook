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

#[derive(serde::Deserialize)]
struct Edit {
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace exact strings in a file. Every edit to one file goes in one \
                          call: they apply in order, and either all land or none do. Each `old` \
                          is matched literally, indentation included, and must match exactly once \
                          against the text as it then stands — add surrounding context to \
                          disambiguate, or set `replace_all`."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "old": { "type": "string" },
                                "new": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old", "new"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path")?)?;
        let edits = parse_edits(args)?;

        let original = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::Io { path: path.clone(), source: e })?;

        // Applied to a copy: a later edit that cannot be placed must not leave
        // the earlier ones on disk.
        let mut text = original;
        let mut replaced = 0usize;
        for (i, edit) in edits.iter().enumerate() {
            match apply(&text, edit) {
                Ok((updated, count)) => {
                    text = updated;
                    replaced += count;
                }
                Err(reason) => {
                    return Ok(ToolOutcome::error(format!(
                        "edit {} of {} did not apply to {}: {reason}. Nothing was written.",
                        i + 1,
                        edits.len(),
                        path.display()
                    ))
                    .with("failed_edit", i as u64 + 1));
                }
            }
        }

        tokio::fs::write(&path, &text).await.map_err(|e| ToolError::Io { path: path.clone(), source: e })?;
        Ok(ToolOutcome::ok(format!(
            "{} edit(s), {replaced} replacement(s) in {}",
            edits.len(),
            path.display()
        ))
        .with("occurrences", replaced as u64))
    }

    fn touched_paths(&self, args: &serde_json::Value) -> Vec<String> {
        path_arg(args)
    }
}

/// Accepts a bare `{old, new}` alongside `edits`, so a model that learnt the
/// single-edit shape elsewhere is not refused over a detail of framing.
fn parse_edits(args: &serde_json::Value) -> Result<Vec<Edit>> {
    let invalid = |message: String| ToolError::Invalid { tool: "edit_file".into(), message };

    if let Some(edits) = args.get("edits") {
        let edits: Vec<Edit> = serde_json::from_value(edits.clone())
            .map_err(|e| invalid(format!("`edits` must be an array of {{old, new}}: {e}")))?;
        if edits.is_empty() {
            return Err(invalid("`edits` is empty — there is nothing to change".into()));
        }
        return Ok(edits);
    }
    match (args.get("old"), args.get("new")) {
        (Some(old), Some(new)) => Ok(vec![Edit {
            old: old.as_str().unwrap_or_default().to_string(),
            new: new.as_str().unwrap_or_default().to_string(),
            replace_all: args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false),
        }]),
        _ => Err(invalid("no edits given — pass `edits: [{old, new}]`".into())),
    }
}

fn apply(text: &str, edit: &Edit) -> std::result::Result<(String, usize), String> {
    match text.matches(&edit.old).count() {
        0 => Err("that text is not in the file as given — read it again, it may have changed".into()),
        n if n > 1 && !edit.replace_all => {
            Err(format!("that text appears {n} times; add surrounding context or set replace_all"))
        }
        n if edit.replace_all => Ok((text.replace(&edit.old, &edit.new), n)),
        _ => Ok((text.replacen(&edit.old, &edit.new, 1), 1)),
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
