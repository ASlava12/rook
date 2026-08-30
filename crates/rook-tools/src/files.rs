//! File tools. Paged rather than capped, and refusing ambiguous edits.

use async_trait::async_trait;
use serde_json::json;

use rook_llm::ToolSpec;

use crate::{Result, Tool, ToolContext, ToolError, ToolOutcome, arg_str, arg_usize};

fn path_arg(args: &serde_json::Value) -> Vec<String> {
    args.get("path").and_then(|v| v.as_str()).map(|p| vec![p.to_string()]).unwrap_or_default()
}

/// Enough that a whole source file usually arrives in one call, and small
/// enough that the byte budget rather than the line count is what stops it.
const DEFAULT_LINES: usize = 2000;

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
        // A limit of zero returned no lines and a note to call again from where
        // it stopped, which is where it started. This tool pages rather than
        // refusing, so the answer to a limit that cannot page is the default one.
        let limit = match arg_usize(args, "limit", DEFAULT_LINES) {
            0 => DEFAULT_LINES,
            given => given,
        };

        // The window rather than the file: reading a large file whole to return
        // two thousand lines of it made its size the caller's problem in memory
        // as well as in context.
        let window = match ctx.files {
            Some(_) => Window::of(&ctx.read_text(&path).await?, offset, limit),
            None => match read_window(&path, offset, limit).await? {
                Some(window) => window,
                None => {
                    return Ok(ToolOutcome::error(format!(
                        "{} looks binary; read_file only handles text",
                        path.display()
                    ))
                    .with("binary", true));
                }
            },
        };
        let total_lines = window.total_lines;

        if offset > 0 && offset >= total_lines {
            return Ok(ToolOutcome::error(format!(
                "{} has {total_lines} line(s); there is nothing at offset {offset}",
                path.display()
            ))
            .with("total_lines", total_lines as u64));
        }

        // Room reserved for the "call again with offset=" line, which is added
        // after the budget is spent and would otherwise push the reply over it.
        let budget = ctx.max_output_bytes.saturating_sub(80);
        let shown: Vec<&str> = window.lines.iter().map(String::as_str).collect();
        let mut page = page(&shown, offset, budget);

        let stopped_at = offset + page.shown;
        if stopped_at < total_lines {
            page.body.push_str(&format!(
                "\n[{} more lines; call read_file again with offset={stopped_at}]\n",
                total_lines - stopped_at
            ));
        }

        Ok(ToolOutcome {
            content: page.body,
            is_error: false,
            truncated: page.cut || stopped_at < total_lines || offset > 0,
            full_bytes: window.total_bytes,
            meta: Default::default(),
        }
        .with("total_lines", total_lines as u64)
        .with("returned_lines", page.shown as u64))
    }

    fn observed_paths(&self, args: &serde_json::Value) -> Vec<String> {
        path_arg(args)
    }
}

/// The lines a read asked for, and what the file has around them.
struct Window {
    lines: Vec<String>,
    total_lines: usize,
    total_bytes: usize,
}

impl Window {
    /// From text a front end already holds — an editor buffer is text by
    /// definition, and it is the editor's memory, not ours.
    fn of(text: &str, offset: usize, limit: usize) -> Self {
        let all: Vec<&str> = text.lines().collect();
        Self {
            lines: all.iter().skip(offset).take(limit).map(|l| (*l).to_string()).collect(),
            total_lines: all.len(),
            total_bytes: text.len(),
        }
    }
}

/// `None` when the file is not text. The check reads the first buffered chunk
/// rather than the whole file, so deciding a file is not worth reading does not
/// read it.
async fn read_window(path: &std::path::Path, offset: usize, limit: usize) -> Result<Option<Window>> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, Read};

        let file =
            std::fs::File::open(&owned).map_err(|e| ToolError::Io { path: owned.clone(), source: e })?;
        let total_bytes = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
        let mut reader = std::io::BufReader::new(file);

        let prefix = reader.fill_buf().map_err(|e| ToolError::Io { path: owned.clone(), source: e })?;
        if prefix.iter().take(8192).any(|b| *b == 0) {
            return Ok(None);
        }

        let mut window = Window { lines: Vec::new(), total_lines: 0, total_bytes };
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.by_ref().take(crate::MAX_LINE).read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // A line past the cap: take what was read as the line and step over
            // the rest of it, so the numbering still describes the file.
            if raw.len() as u64 == crate::MAX_LINE && !raw.ends_with(b"\n") {
                let mut skipped = Vec::new();
                loop {
                    skipped.clear();
                    match reader.by_ref().take(crate::MAX_LINE).read_until(b'\n', &mut skipped) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if skipped.ends_with(b"\n") => break,
                        Ok(_) => {}
                    }
                }
            }
            let n = window.total_lines;
            window.total_lines += 1;
            if n >= offset && window.lines.len() < limit {
                let text = String::from_utf8_lossy(&raw);
                window.lines.push(text.trim_end_matches(['\n', '\r']).to_string());
            }
        }
        Ok(Some(window))
    })
    .await
    .map_err(|e| ToolError::Invalid { tool: "read_file".into(), message: e.to_string() })?
}

/// Numbered lines up to a byte budget.
///
/// The budget is the point: `limit` counts lines, and one line of a minified
/// bundle can be larger than the whole context window. A line that does not fit
/// on its own is cut rather than dropped, so the caller still sees what is there.
fn page(lines: &[&str], offset: usize, max_bytes: usize) -> Page {
    let mut page = Page { body: String::new(), shown: lines.len(), cut: false };
    for (i, line) in lines.iter().enumerate() {
        let prefix = format!("{:>6}\t", offset + i + 1);
        let room = max_bytes.saturating_sub(page.body.len() + prefix.len() + 1);
        if room == 0 {
            page.shown = i;
            return page;
        }
        page.body.push_str(&prefix);
        if line.len() <= room {
            page.body.push_str(line);
        } else {
            let note = format!(" … [{} more bytes on this line]", line.len());
            let cut = char_boundary_at_or_before(line, room.saturating_sub(note.len()));
            page.body.push_str(&line[..cut]);
            page.body.push_str(&note);
            page.cut = true;
        }
        page.body.push('\n');
    }
    page
}

/// `str::floor_char_boundary` says this in one call and is stable only from
/// 1.91; the declared minimum here is 1.90.
fn char_boundary_at_or_before(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

struct Page {
    body: String,
    /// Lines that fit whole or in part. Fewer than asked for when the budget ran
    /// out first.
    shown: usize,
    /// A line was too long for the budget by itself. `offset` cannot page past
    /// it, since it addresses lines.
    cut: bool,
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
        let existed = path.exists();
        ctx.write_text(&path, &content).await?;
        Ok(ToolOutcome::ok(format!(
            "{} {} ({} bytes)",
            if existed { "overwrote" } else { "created" },
            path.display(),
            content.len()
        ))
        .with("created", !existed))
    }

    async fn preview(&self, ctx: &ToolContext, args: &serde_json::Value) -> Option<String> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path").ok()?).ok()?;
        let before = ctx.read_text(&path).await.unwrap_or_default();
        Some(diff(&path, &before, args.get("content")?.as_str()?))
    }

    fn touched_paths(&self, args: &serde_json::Value) -> Vec<String> {
        path_arg(args)
    }

    fn overwrites(&self) -> bool {
        true
    }
}

/// Written out once because the schema names it twice, and a shape that drifted
/// between the two would be a tool documenting something it does not accept.
fn edit_list() -> serde_json::Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "old": { "type": "string" },
                "new": { "type": "string" },
                "replace_all": { "type": "boolean" }
            },
            "required": ["old", "new"]
        }
    })
}

/// Deleting is the one change to a file that `run_command` cannot undo.
///
/// Everything else the shell does to a file leaves the content somewhere — a
/// move, an overwrite by a build. A `rm` leaves nothing, and the loop only
/// checkpoints what a tool says it will touch, so a deletion through the shell
/// is outside `rook session rewind` entirely. Through here it is inside it.
pub struct DeleteFile;

#[async_trait]
impl Tool for DeleteFile {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_file".into(),
            description: "Delete one file. Use this rather than `rm`, which no rewind can undo.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path")?)?;
        if path.is_dir() {
            return Ok(ToolOutcome::error(format!(
                "{} is a directory — this deletes one file, so name the files or use \
                 `run_command` and accept that a rewind will not bring them back",
                path.display()
            )));
        }
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        std::fs::remove_file(&path).map_err(|e| ToolError::Io { path: path.clone(), source: e })?;
        Ok(ToolOutcome::ok(format!("deleted {} ({bytes} bytes)", path.display())).with("bytes", bytes))
    }

    async fn preview(&self, ctx: &ToolContext, args: &serde_json::Value) -> Option<String> {
        let path = ctx.resolve(&arg_str(args, self.name(), "path").ok()?).ok()?;
        let before = ctx.read_text(&path).await.ok()?;
        // Not `diff` alone: with nothing on either side it says the file "would
        // be written unchanged", which is the opposite of what is about to
        // happen, in the one place a person is deciding whether to let it.
        Some(match before.is_empty() {
            true => format!("{} would be deleted (it is empty)", path.display()),
            false => diff(&path, &before, ""),
        })
    }

    fn touched_paths(&self, args: &serde_json::Value) -> Vec<String> {
        path_arg(args)
    }

    /// The whole file goes, so the slower race applies: deleting something
    /// another turn has looked at since this one did is the same mistake as
    /// overwriting it.
    fn overwrites(&self) -> bool {
        true
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
            description: "Replace exact strings in files. Every edit to one file goes in one \
                          call, and a refactor across several in one `files`: they apply in order \
                          and either all land or none do. Each `old` is matched literally, \
                          indentation included, and must match exactly once against the text as \
                          it then stands — add context to disambiguate, or set `replace_all`."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "edits": edit_list(),
                    "files": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "path": { "type": "string" }, "edits": edit_list() }
                        }
                    }
                }
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let mut edited = Vec::new();
        let (mut replaced, mut edits) = (0usize, 0usize);
        // Every file worked out before any is written: a refactor that fails on
        // the third must not leave the first two changed, which is the rule this
        // already kept within one file.
        for target in parse_targets(args)? {
            let path = ctx.resolve(&target.path)?;
            let mut text = ctx.read_text(&path).await?;
            for (i, edit) in target.edits.iter().enumerate() {
                match apply(&text, edit) {
                    Ok((updated, count)) => {
                        text = updated;
                        replaced += count;
                    }
                    Err(reason) => {
                        return Ok(ToolOutcome::error(format!(
                            "edit {} of {} did not apply to {}: {reason}. Nothing was written, \
                             here or in any other file.",
                            i + 1,
                            target.edits.len(),
                            path.display()
                        ))
                        .with("failed_edit", i as u64 + 1));
                    }
                }
            }
            edits += target.edits.len();
            edited.push((path, text));
        }

        let names: Vec<String> = edited.iter().map(|(p, _)| p.display().to_string()).collect();
        for (i, (path, text)) in edited.iter().enumerate() {
            // Everything that can be decided was decided above, so a failure
            // here is the filesystem refusing — and by then the files before it
            // are written. Saying which is the difference between a half-done
            // refactor somebody can finish and one they have to find.
            ctx.write_text(path, text).await.map_err(|e| match i {
                0 => e,
                done => ToolError::Invalid {
                    tool: "edit_file".into(),
                    message: format!(
                        "{e} — {} of {} were already written: {}",
                        done,
                        edited.len(),
                        names[..done].join(", ")
                    ),
                },
            })?;
        }
        Ok(ToolOutcome::ok(format!("{edits} edit(s), {replaced} replacement(s) in {}", names.join(", ")))
            .with("occurrences", replaced as u64))
    }

    async fn preview(&self, ctx: &ToolContext, args: &serde_json::Value) -> Option<String> {
        let mut shown = Vec::new();
        for target in parse_targets(args).ok()? {
            let path = ctx.resolve(&target.path).ok()?;
            let before = ctx.read_text(&path).await.ok()?;
            // The same edits the call would make, against a copy nothing writes:
            // an approval shown anything else is an approval of something else.
            let mut after = before.clone();
            for edit in &target.edits {
                after = apply(&after, edit).ok()?.0;
            }
            shown.push(diff(&path, &before, &after));
        }
        Some(shown.join("\n"))
    }

    fn touched_paths(&self, args: &serde_json::Value) -> Vec<String> {
        match parse_targets(args) {
            Ok(targets) => targets.into_iter().map(|t| t.path).collect(),
            Err(_) => path_arg(args),
        }
    }
}

/// One file and what to do to it.
struct Target {
    path: String,
    edits: Vec<Edit>,
}

/// Accepts `files` for several and `path` with `edits` for one, so a refactor
/// and a one-line fix are the same tool.
fn parse_targets(args: &serde_json::Value) -> Result<Vec<Target>> {
    let invalid = |message: String| ToolError::Invalid { tool: "edit_file".into(), message };
    let Some(files) = args.get("files") else {
        return Ok(vec![Target { path: arg_str(args, "edit_file", "path")?, edits: parse_edits(args)? }]);
    };
    let files = files.as_array().filter(|f| !f.is_empty()).ok_or_else(|| {
        invalid(
            "`files` must be a non-empty array of {path, edits} — for one file, pass `path` \
                 and `edits` instead"
                .into(),
        )
    })?;
    let targets: Vec<Target> = files
        .iter()
        .map(|f| Ok(Target { path: arg_str(f, "edit_file", "path")?, edits: parse_edits(f)? }))
        .collect::<Result<_>>()?;

    // Each entry is read from disk on its own, so two for the same file would
    // both start from the original and the second write would quietly undo the
    // first.
    let mut seen = std::collections::BTreeSet::new();
    if let Some(twice) = targets.iter().find(|t| !seen.insert(&t.path)) {
        return Err(invalid(format!(
            "{} appears twice in `files` — put all of its edits in one entry, in the order they \
             should apply",
            twice.path
        )));
    }
    Ok(targets)
}

/// The change as a person reads one. Bounded, because a rewritten file is a diff
/// the size of the file twice over, and this is a question in a terminal.
fn diff(path: &std::path::Path, before: &str, after: &str) -> String {
    const MOST: usize = 8 * 1024;
    if before == after {
        return format!("{} would be written unchanged", path.display());
    }
    let text = similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header("before", "after")
        .to_string();
    crate::elide_middle(&text, MOST)
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
    // The empty string matches between every pair of characters, so `replace_all`
    // would interleave `new` through the whole file and report it as a success —
    // and the ambiguity message below is what would send a model there.
    if edit.old.is_empty() {
        return Err("`old` is empty, which matches everywhere. To put text at a \
                    particular place, match the line it goes next to; to replace the \
                    file, use write_file"
            .into());
    }
    if edit.old == edit.new {
        return Err("`old` and `new` are the same, so this edit would change nothing".into());
    }
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
        // workspace boundary, and a default is not a decision. `require_git`
        // is not the default: without it a `.gitignore` is silently ignored
        // outside a repository, and a Rook workspace need not be one.
        for entry in ignore::WalkBuilder::new(&root)
            .max_depth(Some(depth))
            .follow_links(false)
            .require_git(false)
            .build()
            .flatten()
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
            // Forward slashes, as the manifests store them and as a directory is
            // already marked here. Native separators made the listing disagree
            // with itself on Windows — `src/` on one line and `src\main.rs` on
            // the next — and left the model two spellings for one path.
            let rel = rel.display().to_string().replace('\\', "/");
            entries.push(format!("{rel}{}", if is_dir { "/" } else { "" }));
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
