//! Output formatting shared by the commands.

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

/// Unix seconds as a local-ish `YYYY-MM-DD HH:MM` stamp.
///
/// Hand-rolled civil-from-days rather than a date crate: this is the only place
/// the binary needs calendar arithmetic, and it is not worth a dependency plus
/// a timezone database on four platforms.
pub fn timestamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

pub fn ago(unix: i64) -> String {
    let delta = rook_store::now_unix() - unix;
    match delta {
        d if d < 0 => "in the future".into(),
        d if d < 60 => format!("{d}s ago"),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86_400 => format!("{}h ago", d / 3600),
        d if d < 86_400 * 60 => format!("{}d ago", d / 86_400),
        d => format!("{}mo ago", d / (86_400 * 30)),
    }
}

/// Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Render rows as an aligned table.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let mut out = String::new();
    for (i, h) in headers.iter().enumerate() {
        out.push_str(&pad(h, widths[i]));
        if i + 1 < headers.len() {
            out.push_str("  ");
        }
    }
    out.push('\n');
    out.push_str(&"─".repeat(widths.iter().sum::<usize>() + 2 * headers.len().saturating_sub(1)));
    out.push('\n');
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                continue;
            }
            out.push_str(&pad(cell, widths[i]));
            if i + 1 < row.len() {
                out.push_str("  ");
            }
        }
        out.push('\n');
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width { s.to_string() } else { format!("{s}{}", " ".repeat(width - len)) }
}

/// A one-line proportional bar, for the storage breakdown.
pub fn bar(value: u64, max: u64, width: usize) -> String {
    if max == 0 {
        return " ".repeat(width);
    }
    let filled = ((value as f64 / max as f64) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled.min(width)), "·".repeat(width.saturating_sub(filled)))
}

/// Where a search hit came from, in one column.
///
/// A hit in something said belongs to a session and a sequence number; one in a
/// captured file belongs to the capture and the path, and a checkpoint someone
/// made by hand has no session at all.
pub fn hit_where(hit: &rook_core::search::Hit) -> String {
    match (&hit.file, hit.session.is_empty()) {
        (Some(path), true) => format!("{}:{path}", hit.title),
        (Some(path), false) => format!("{}:{path}", &hit.session[..12]),
        (None, _) => format!("{}  #{:<4}", &hit.session[..12], hit.seq),
    }
}

/// `12s`, `4m10s` — the two units a person waiting reads, and no more.
///
/// Here rather than in the TUI because a call that took a while says so in
/// every front end now, and two formatters for one question drift.
pub fn elapsed(since: std::time::Duration) -> String {
    let seconds = since.as_secs();
    match seconds < 60 {
        true => format!("{seconds}s"),
        false => format!("{}m{:02}s", seconds / 60, seconds % 60),
    }
}

/// A turn's tool calls as a terminal reads them: announced when they start,
/// marked when they finish.
///
/// The mark used to go wherever the cursor was, which is right only while calls
/// run one at a time. A turn that lists a directory and reads a file announces
/// both before either finishes, and the two ticks landed on the end of the
/// second line and on a line of their own — so the read was reported as
/// finished twice and the listing not at all. What the mark belongs to is a
/// queue question, which `rook_core::calls::Running` answers for every front
/// end; what a terminal can do about it is this: append when the call's own
/// line is still the last thing written, and name the call again when it is
/// not.
///
/// Not "when only one is running": a call can be alone and still not be the
/// last line, because another finished under it.
#[derive(Debug, Default)]
pub struct Calls {
    running: rook_core::calls::Running,
    /// The announcement whose line is still the last thing written.
    announced: Option<String>,
    /// Whether what was written last left the line unterminated.
    open: bool,
}

impl Calls {
    /// Text from the model. It writes itself; what this needs to know is that
    /// no call's line is the current one any more.
    pub fn said(&mut self, text: &str) {
        self.announced = None;
        self.open = !text.ends_with('\n');
    }

    pub fn started(&mut self, name: &str, doing: &str) -> String {
        let line = format!("{}  · {doing}", if self.open { "\n" } else { "" });
        self.running.started(name, doing);
        self.announced = Some(doing.to_string());
        self.open = true;
        line
    }

    pub fn finished(&mut self, name: &str, failed: bool) -> String {
        let (said, took) = self.running.finished(name);
        // A call worth having waited for says how long it was. `working…`
        // counts the turn rather than the call, so a minute spent inside one
        // command left no trace of itself once the command came back.
        let mark = match (failed, took) {
            (true, None) => "✗".to_string(),
            (false, None) => "✓".to_string(),
            (true, Some(took)) => format!("✗ {}", elapsed(took)),
            (false, Some(took)) => format!("✓ {}", elapsed(took)),
        };
        let line = match self.announced.as_deref() == Some(said.as_str()) {
            true => format!(" {mark}\n"),
            false => format!("{}  {mark} {said}\n", if self.open { "\n" } else { "" }),
        };
        self.announced = None;
        self.open = false;
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two calls announced before either finishes. The mark used to go where
    /// the cursor was, so the listing's tick landed on the read's line and the
    /// read's tick on a line of its own — one call marked twice, the other not
    /// at all, and a stray ` ✓` under them both.
    #[test]
    fn a_mark_belongs_to_its_own_call_however_many_are_running() {
        let mut calls = Calls::default();
        let mut screen = String::new();
        screen.push_str("I'll look.");
        calls.said("I'll look.");

        screen.push_str(&calls.started("list_dir", "list ."));
        screen.push_str(&calls.started("read_file", "read service.toml"));
        screen.push_str(&calls.finished("list_dir", false));
        screen.push_str(&calls.finished("read_file", false));

        assert_eq!(
            screen, "I'll look.\n  · list .\n  · read service.toml\n  ✓ list .\n  ✓ read service.toml\n",
            "each call is announced once and marked once, by name"
        );
    }

    /// One at a time, which is most turns: the mark goes on the end of the line
    /// the call was announced on, and no line names it twice.
    #[test]
    fn a_call_that_is_the_only_one_running_is_marked_where_it_stands() {
        let mut calls = Calls::default();
        let mut screen = String::new();
        screen.push_str(&calls.started("read_file", "read a.rs"));
        screen.push_str(&calls.finished("read_file", false));
        screen.push_str(&calls.started("run_command", "run cargo test"));
        screen.push_str(&calls.finished("run_command", true));

        assert_eq!(screen, "  · read a.rs ✓\n  · run cargo test ✗\n", "and a failure says so");
    }

    /// A turn that sat on one command for a minute left no trace of it once the
    /// command came back: `working…` counts the turn, not the call. Whether a
    /// call is long enough to be worth a number is core's rule and is tested
    /// there; what this asks is that nothing is said before it is known, and
    /// that a fast call stays quiet.
    #[test]
    fn a_duration_is_claimed_only_once_there_is_one_worth_claiming() {
        let mut calls = Calls::default();
        assert_eq!(
            calls.started("run_command", "run cargo test"),
            "  · run cargo test",
            "the announcement is the work, and the call has not finished"
        );
        assert_eq!(calls.finished("run_command", false), " ✓\n", "and a fast one stays quiet");
    }
}
