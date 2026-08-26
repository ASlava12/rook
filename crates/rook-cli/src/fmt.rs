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
