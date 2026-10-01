//! Saved previews are made from the text this call read and wrote, never from
//! whatever the file becomes by the time a history viewer opens it.
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

use crate::ToolContext;

const INPUT_BYTES: usize = 64 * 1024;
const DIFF_BYTES: usize = 8 * 1024;
const PATH_BYTES: usize = 512;

struct Preview {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    limit: usize,
    total: usize,
}
impl Write for Preview {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.total = self.total.saturating_add(bytes.len());
        let head_limit = self.limit / 3;
        let take = bytes.len().min(head_limit.saturating_sub(self.head.len()));
        self.head.extend_from_slice(&bytes[..take]);
        let rest = &bytes[take..];
        let tail_limit = self.limit - head_limit;
        if rest.len() >= tail_limit {
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - tail_limit..]);
        } else {
            let discard = self.tail.len().saturating_add(rest.len()).saturating_sub(tail_limit);
            self.tail.drain(..discard);
            self.tail.extend(rest);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Preview {
    fn text(mut self) -> (String, bool) {
        if self.total <= self.limit {
            self.head.extend(self.tail);
            return (String::from_utf8_lossy(&self.head).into_owned(), false);
        }
        // Head/tail admission works in bytes. Restore UTF-8 boundaries before
        // inserting an elision marker; do not replace partial Unicode symbols.
        let end =
            std::str::from_utf8(&self.head).map(|_| self.head.len()).unwrap_or_else(|e| e.valid_up_to());
        let tail = self.tail.make_contiguous();
        let start = tail.iter().position(|byte| byte & 0xc0 != 0x80).unwrap_or(tail.len());
        let omitted = self.total.saturating_sub(end + tail.len() - start);
        (
            format!(
                "{}\n[{omitted} diff bytes elided from the middle; preview only]\n{}",
                String::from_utf8_lossy(&self.head[..end]),
                String::from_utf8_lossy(&tail[start..])
            ),
            true,
        )
    }
}

pub(super) fn file(path: &Path, before: Option<&str>, after: &str) -> Value {
    let path = path.to_string_lossy();
    let label = rook_llm::truncate(&path, PATH_BYTES - 3);
    let Some(before) = before else {
        return json!({"path":label,"diff":"Diff unavailable: the previous text was not captured within the comparison budget.","limited":true});
    };
    if before.len() > INPUT_BYTES || after.len() > INPUT_BYTES {
        return json!({"path":label,"diff":"Diff unavailable: before or after text exceeds the 64 KiB comparison budget.","limited":true});
    }
    if before == after {
        return json!({"path":label,"diff":"Written unchanged.","limited":false});
    }
    let (text, limited) = preview(before, after, 1, DIFF_BYTES);
    json!({"path":label,"diff":text,"limited":limited})
}

pub(super) fn preview(before: &str, after: &str, context: usize, budget: usize) -> (String, bool) {
    if before.len() > INPUT_BYTES || after.len() > INPUT_BYTES {
        return ("Diff unavailable: before or after text exceeds the 64 KiB comparison budget.".into(), true);
    }
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(200))
        .diff_lines(before, after);
    // Reserve marker space. Evict the previous tail before copying each new
    // chunk, keeping both removed and added text for a whole-file rewrite.
    let mut preview =
        Preview { head: Vec::new(), tail: VecDeque::new(), limit: budget.saturating_sub(96), total: 0 };
    if write!(preview, "{}", diff.unified_diff().context_radius(context).header("before", "after")).is_err() {
        return ("Diff preview could not be formatted.".into(), true);
    }
    preview.text()
}

/// The optional preview must not make a write fail or read an entire old file.
/// Editor-owned buffers have no bounded read contract, so leave their old text
/// unknown rather than copying an arbitrarily large buffer for decoration.
pub(super) async fn before_write(ctx: &ToolContext, path: &Path, after: &str) -> Option<String> {
    if after.len() > INPUT_BYTES || ctx.files.is_some() {
        return None;
    }
    let ctx = ctx.clone();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let (root, relative) = ctx.disk_path(&path).ok()?;
        match rook_contain::files::read_text(&root, &relative, INPUT_BYTES) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
            Err(_) => None,
        }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_head_and_tail_stay_bounded_across_split_unicode_without_replacement_characters() {
        let full = "🙂é漢字".repeat(1000);
        let mut preview = Preview { head: Vec::new(), tail: VecDeque::new(), limit: 34, total: 0 };
        for chunk in full.as_bytes().chunks(7) {
            preview.write_all(chunk).unwrap();
            assert!(preview.head.len() + preview.tail.len() <= 34);
        }
        assert!(preview.total > preview.limit);
        assert!(
            std::str::from_utf8(&preview.head).is_err(),
            "the head actually stops inside a Unicode symbol"
        );
        assert!(
            preview.tail.front().is_some_and(|byte| byte & 0xc0 == 0x80),
            "the tail actually starts inside a Unicode symbol"
        );
        let (text, limited) = preview.text();
        assert!(limited && text.len() <= 34 + 96, "{text}");
        assert!(!text.contains('\u{fffd}'), "{text}");
        assert!(text.contains("elided from the middle"), "{text}");
    }
}
