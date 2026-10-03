//! Finite Kitty capability assessment. Does not upload or place an image.
use std::{
    fs::OpenOptions,
    io::{IsTerminal, Read, Write},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::{execute, terminal};

const MAX_CAPTURE: usize = 1024;
// The query loads one RGB pixel without storing it or replacing an image.
const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";

#[derive(Parser)]
struct Args {
    /// Fresh owned evidence directory, not an agent home.
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    alternate: bool,
}

struct Restore {
    alternate: bool,
}
impl Drop for Restore {
    fn drop(&mut self) {
        if self.alternate {
            let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
        }
        let _ = terminal::disable_raw_mode();
    }
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

fn outcome(bytes: &[u8], saturated: bool) -> &'static str {
    if saturated {
        return "capture_limit_inconclusive";
    }
    if contains(bytes, b"\x1b_Gi=31;OK\x1b\\") {
        return "kitty_query_ok";
    }
    let prefix = b"\x1b_Gi=31;";
    if let Some(at) = bytes.windows(prefix.len()).position(|window| window == prefix)
        && contains(&bytes[at + prefix.len()..], b"\x1b\\")
    {
        return "kitty_query_rejected";
    }
    // Only a complete primary DA response after the query is negative evidence.
    if let Some(at) = bytes.windows(3).position(|window| window == b"\x1b[?") {
        let tail = &bytes[at + 3..];
        if let Some(end) = tail.iter().position(|byte| *byte == b'c')
            && end > 0
            && tail[..end].iter().all(|byte| byte.is_ascii_digit() || *byte == b';')
        {
            return "da_without_kitty_reply";
        }
    }
    "no_complete_reply_inconclusive"
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.root.is_dir() {
        bail!("evidence root must already exist");
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("run the probe in an owned terminal");
    }
    let mut result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.root.join("image-query.json"))
        .context("fresh result")?;
    terminal::enable_raw_mode()?;
    let mut restore = Restore { alternate: false };
    if args.alternate {
        execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
        restore.alternate = true;
    }
    let (send, receive) = mpsc::sync_channel(64);
    // Read at most the admitted capture. An idle console read can remain blocked;
    // this finite executable exits after the deadline and never joins that read.
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        for _ in 0..MAX_CAPTURE {
            let mut byte = [0];
            match input.read(&mut byte) {
                Ok(1) => {
                    if send.send(byte[0]).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });
    let start = Instant::now();
    let deadline = start + Duration::from_secs(3);
    std::io::stdout().write_all(QUERY)?;
    std::io::stdout().flush()?;
    let mut capture = Vec::with_capacity(MAX_CAPTURE);
    while capture.len() < MAX_CAPTURE {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receive.recv_timeout(remaining) {
            Ok(byte) => capture.push(byte),
            Err(_) => break,
        }
    }
    drop(restore);
    let saturated = capture.len() == MAX_CAPTURE;
    serde_json::to_writer_pretty(
        &mut result,
        &serde_json::json!({
            "scope": "capability query only; no pixel, placement, crop or deletion proof",
            "alternate": args.alternate,
            "outcome": outcome(&capture, saturated),
            "response_bytes": capture,
            "capture_limit": MAX_CAPTURE,
            "elapsed_ms": start.elapsed().as_millis(),
        }),
    )?;
    result.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_matching_complete_reply_proves_query_success() {
        assert_eq!(outcome(b"\x1b_Gi=31;OK\x1b\\", false), "kitty_query_ok");
        assert_eq!(outcome(b"\x1b_Gi=32;OK\x1b\\", false), "no_complete_reply_inconclusive");
        assert_eq!(outcome(b"\x1b_Gi=31;O", false), "no_complete_reply_inconclusive");
        assert_eq!(outcome(b"\x1b_Gi=31;ENOENT\x1b\\", false), "kitty_query_rejected");
    }

    #[test]
    fn silence_and_saturation_do_not_claim_unsupported_protocol() {
        assert_eq!(outcome(b"", false), "no_complete_reply_inconclusive");
        assert_eq!(outcome(b"\x1b[?62;4c", false), "da_without_kitty_reply");
        assert_eq!(outcome(b"\x1b[?garbagec", false), "no_complete_reply_inconclusive");
        assert_eq!(outcome(b"\x1b_Gi=31;OK\x1b\\", true), "capture_limit_inconclusive");
    }
}
