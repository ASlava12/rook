//! Local terminal editors own the tty only while the draft is outside the TUI.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use crossterm::{cursor, event, execute, terminal};
use serde::{Deserialize, Serialize};

const MAX_DRAFT: u64 = 1 << 20;
const MAX_COMMAND: u64 = 4096;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Editor {
    program: PathBuf,
    args: Vec<String>,
}

impl Editor {
    pub(super) fn label(&self) -> String {
        let name = self.program.file_name().unwrap_or(self.program.as_os_str()).to_string_lossy();
        if self.args.is_empty() { name.into_owned() } else { format!("{name} {}", self.args.join(" ")) }
    }
}

pub(super) struct Picker {
    pub(super) editors: Vec<Editor>,
    pub(super) at: usize,
    pub(super) launch: bool,
}

impl Picker {
    pub(super) fn discover() -> Self {
        let mut editors = Vec::new();
        if let Ok(bytes) = read_bounded(&preference(), MAX_COMMAND)
            && let Ok(editor) = serde_json::from_str::<Editor>(&bytes)
            && executable(&editor.program)
        {
            editors.push(editor);
        }
        // Environment commands are tokenized, never evaluated by a shell.
        // A path with spaces can also be supplied without extra quoting.
        for name in ["VISUAL", "EDITOR"] {
            if let Ok(value) = std::env::var(name)
                && value.len() <= MAX_COMMAND as usize
            {
                let words =
                    if Path::new(&value).is_file() { Some(vec![value]) } else { shlex::split(&value) };
                if let Some(words) = words {
                    add(&mut editors, &words);
                }
            }
        }
        for words in [
            &["nano"][..],
            &["nvim"],
            &["vim"],
            &["vi"],
            &["micro"],
            &["hx"],
            &["helix"],
            &["emacs", "-nw"],
            &["joe"],
            &["ne"],
        ] {
            add(&mut editors, &words.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        }
        Self { editors, at: 0, launch: false }
    }
}

fn add(editors: &mut Vec<Editor>, words: &[String]) {
    if let Some((program, args)) = words.split_first()
        && let Some(program) = resolve(program)
    {
        let editor = Editor { program, args: args.to_vec() };
        if !editors.contains(&editor) {
            editors.push(editor);
        }
    }
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn resolve(program: &str) -> Option<PathBuf> {
    let program = Path::new(program);
    let candidates: Vec<PathBuf> = if program.components().count() > 1 || program.is_absolute() {
        vec![program.to_path_buf()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(program))
            .collect()
    };
    for candidate in candidates {
        if executable(&candidate) {
            return std::path::absolute(candidate).ok();
        }
        #[cfg(windows)]
        if candidate.extension().is_none() {
            let exe = candidate.with_extension("exe");
            if executable(&exe) {
                return std::path::absolute(exe).ok();
            }
        }
    }
    None
}

fn preference() -> PathBuf {
    rook_core::paths::home().join("tui-editor.json")
}

fn remember(editor: &Editor) -> Result<()> {
    let home = rook_core::paths::home();
    std::fs::create_dir_all(&home)?;
    let bytes = serde_json::to_vec(editor)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_COMMAND, "editor command exceeds {MAX_COMMAND} bytes");
    let mut temp = tempfile::NamedTempFile::new_in(home)?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.persist(preference())?;
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<String> {
    anyhow::ensure!(path.metadata()?.is_file(), "not a regular file");
    let file = std::fs::File::open(path)?;
    anyhow::ensure!(file.metadata()?.is_file(), "not a regular file");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= limit, "file exceeds {limit} bytes");
    String::from_utf8(bytes).context("save the draft as UTF-8")
}

/// Always put the terminal back, including failed process creation or a panic.
struct Suspended {
    mouse: bool,
    enhanced: bool,
    restored: bool,
}
impl Suspended {
    fn restore(&mut self) -> Result<()> {
        terminal::enable_raw_mode()?;
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            event::EnableBracketedPaste
        )?;
        if self.mouse {
            execute!(std::io::stdout(), event::EnableMouseCapture)?;
        }
        if self.enhanced {
            execute!(
                std::io::stdout(),
                event::PushKeyboardEnhancementFlags(
                    event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                )
            )?;
        }
        self.restored = true;
        Ok(())
    }
}
impl Drop for Suspended {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.restore();
        }
    }
}

pub(super) fn edit(
    editor: &Editor,
    draft: &str,
    workspace: &Path,
    mouse: bool,
    enhanced: bool,
    mut tick: impl FnMut(),
) -> Result<(String, Option<String>)> {
    anyhow::ensure!(draft.len() as u64 <= MAX_DRAFT, "draft exceeds {MAX_DRAFT} bytes");
    let mut builder = tempfile::Builder::new();
    builder.prefix("rook-prompt-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let dir = builder.tempdir()?;
    let path = dir.path().join("prompt.md");
    std::fs::write(&path, draft)?;
    let mut suspended = Suspended { mouse, enhanced, restored: false };
    execute!(std::io::stdout(), event::DisableMouseCapture, event::DisableBracketedPaste)?;
    if enhanced {
        execute!(std::io::stdout(), event::PopKeyboardEnhancementFlags)?;
    }
    execute!(std::io::stdout(), cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    let result = (|| -> Result<()> {
        let mut command = Command::new(&editor.program);
        // This child is interactive: inherit the current console. Suppressing
        // its console, as background tools do, would leave editors without
        // the console APIs they need for keyboard input and drawing.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0);
        }
        let mut child = command
            .args(&editor.args)
            .arg(&path)
            .current_dir(workspace)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("could not start {}", editor.label()))?;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    anyhow::ensure!(status.success(), "{} exited with {status}", editor.label());
                    return Ok(());
                }
                Ok(None) => {
                    tick();
                    std::thread::sleep(super::TICK);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        }
    })();
    let restored = suspended.restore();
    // Keep a recoverable copy on failure: an editor can save before exiting
    // unsuccessfully, and an oversized/invalid file may still contain work.
    let text = result.and_then(|()| read_bounded(&path, MAX_DRAFT));
    restored?;
    match text {
        Ok(text) => Ok((text, remember(editor).err().map(|e| format!("editor preference not saved: {e}")))),
        Err(error) => {
            let kept = dir.keep().join("prompt.md");
            bail!("{error:#}; original draft kept; edited file: {}", kept.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edited_drafts_refuse_oversize_and_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.md");
        std::fs::write(&path, vec![b'x'; MAX_DRAFT as usize + 1]).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_DRAFT);
        assert!(read_bounded(&path, MAX_DRAFT).unwrap_err().to_string().contains("exceeds"));
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read_bounded(&path, MAX_DRAFT).unwrap_err().to_string().contains("UTF-8"));
        std::fs::write(&path, "Привет\n\n").unwrap();
        assert_eq!(read_bounded(&path, MAX_DRAFT).unwrap(), "Привет\n\n");
    }
}
