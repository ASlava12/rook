//! Clipboard transport is an explicit frontend action, never a model tool.
use std::io::{Seek, Write};
use std::process::{Command, Stdio};

pub(crate) fn arguments(text: &str) -> anyhow::Result<bool> {
    match text.trim() {
        "" => Ok(false),
        "--summary" => Ok(true),
        _ => anyhow::bail!("use /copy [--summary]"),
    }
}

pub(crate) fn copy(answer: &rook_core::last_answer::Answer) -> String {
    let result = put(&answer.text);
    let note = if answer.truncated { " (bounded excerpt; full answer remains in history)" } else { "" };
    match result {
        Ok(()) => format!(
            "Copied saved {} #{}{note}.",
            if answer.summary { "summary" } else { "answer" },
            answer.seq
        ),
        Err(error) => {
            let safe: String =
                answer.text.chars().filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t')).collect();
            format!("Clipboard unavailable ({error}); saved answer #{}{note}:\n{safe}", answer.seq)
        }
    }
}

fn put(text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(text.len() <= 65536, "clipboard text exceeds 64 KiB");
    let mut input = tempfile::tempfile()?;
    input.write_all(text.as_bytes())?;
    input.rewind()?;
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(windows)]
    let candidates: &[(&str, &[&str])] = &[(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ],
    )];
    #[cfg(not(any(target_os = "macos", windows)))]
    let candidates: &[(&str, &[&str])] =
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])];
    for (program, args) in candidates {
        input.rewind()?;
        let mut command = Command::new(program);
        command
            .args(*args)
            .stdin(Stdio::from(input.try_clone()?))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = match rook_contain::quietly(&mut command).spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().ok();
                child.wait().ok();
                anyhow::bail!("clipboard utility exceeded three seconds");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    anyhow::bail!("no usable clipboard utility")
}
