//! What "better" means for a project, decided before the work starts.
//!
//! An agent that writes the code and the tests can bring the two into
//! agreement, and over a long run it will: told a check fails, the cheapest
//! move available is to change the check. That is not a hypothetical here —
//! `verify` carries a rule about it because two models, asked to judge a
//! function, rewrote the function; and a claim that failed and then held after
//! the turn edited what it was about is reported `unproven` for the same
//! reason.
//!
//! So the scorecard is declared in the workspace, in `.rook/evaluation.toml`,
//! and run by the harness rather than by the model. The model never calls it
//! and cannot call it: it is a command the agent's operator wrote down, run
//! outside a turn, and its result is evidence of the same kind as the files.
//!
//! Real independence is not available to a coding agent — it has to be able to
//! edit the repository, which is where the checks and the tests live. What is
//! available, and is what this buys, is that any change to them is named
//! beside the score instead of disappearing into it. A run that went from four
//! failures to none while rewriting the tests reads as exactly that.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a project writes down how it is judged.
pub fn scorecard_path(workspace: &Path) -> PathBuf {
    workspace.join(".rook").join("evaluation.toml")
}

/// The checks a project is judged by.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Scorecard {
    #[serde(rename = "check")]
    pub checks: Vec<Check>,
}

/// One thing that is either true of the workspace or not.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Check {
    /// What it is called in the report. Also its identity across runs, which is
    /// how one run is compared with the last.
    pub name: String,
    /// The command, run through a shell in the workspace.
    pub run: String,
    /// The exit status that counts as a pass. Zero unless it says otherwise —
    /// a check for "this still fails" is a legitimate thing to want, and it is
    /// better written down than expressed by inverting the command.
    pub expect: i32,
    /// Read the last line of output as a number and report it beside the pass.
    ///
    /// For the things a project wants to watch rather than gate on: a
    /// percentage, a count, a size. Named so the report can say what the number
    /// is — "coverage 81.4" rather than "81.4".
    pub measures: String,
    /// Paths whose change is worth reporting beside this check's result.
    ///
    /// Not a prohibition. A test file legitimately changes when a feature
    /// lands, and a rule that forbade it would be a rule people turn off. What
    /// it buys is that a check which started passing in the same iteration that
    /// rewrote it says so, and whoever reads the score can tell the two apart.
    pub guards: Vec<String>,
    /// How long it may run. Zero is the default below.
    pub timeout_secs: u64,
}

/// The default deadline for one check, where it names none.
///
/// Generous on purpose: this is somebody's test suite, and the number exists to
/// tell a hung command from a slow one rather than to keep a run brisk. The
/// same reasoning as every other wait here.
const PATIENCE: u64 = 1_800;

/// What one check did.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scored {
    pub name: String,
    pub passed: bool,
    /// The status it actually exited with, or `None` where it was stopped at
    /// its deadline — which is not the same as failing and does not read as it.
    pub status: Option<i32>,
    pub took_ms: u64,
    /// What `measures` asked for, where the last line was a number.
    pub measured: Option<f64>,
    pub measures: String,
    /// The last of what it printed, for a failure somebody has to act on.
    pub said: String,
    /// Which of this check's guarded paths changed since the scorecard was
    /// read. Empty is the ordinary case and prints as nothing.
    pub touched: Vec<String>,
}

/// One whole run of the scorecard.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub checks: Vec<Scored>,
    /// True when the scorecard itself differs from what it was when the work
    /// began. Its own hash, kept apart from the checks, because a scorecard
    /// that changed mid-run makes every number in the report a different
    /// measurement rather than a worse or better one.
    pub scorecard_changed: bool,
}

impl Report {
    pub fn passed(&self) -> usize {
        self.checks.iter().filter(|c| c.passed).count()
    }

    /// Whether every check passed — and nothing that would make that claim
    /// meaningless happened while they were being run.
    pub fn clean(&self) -> bool {
        !self.scorecard_changed && self.checks.iter().all(|c| c.passed && c.touched.is_empty())
    }

    /// The one-line summary a turn is handed at the start of the next
    /// iteration, and a person reads at the end of this one.
    pub fn summary(&self) -> String {
        let (passed, all) = (self.passed(), self.checks.len());
        let mut said = match all {
            0 => "no checks are declared".to_string(),
            _ => format!("{passed} of {all} checks pass"),
        };
        let measured: Vec<String> =
            self.checks.iter().filter_map(|c| c.measured.map(|n| format!("{} {n}", c.measures))).collect();
        if !measured.is_empty() {
            said.push_str(&format!(" · {}", measured.join(", ")));
        }
        // Said last and said plainly: it is the part that changes what the
        // numbers before it mean.
        let touched: Vec<&str> =
            self.checks.iter().filter(|c| !c.touched.is_empty()).map(|c| c.name.as_str()).collect();
        if self.scorecard_changed {
            said.push_str(" — the scorecard itself changed during this run, so these are not the same measurement as the last");
        } else if !touched.is_empty() {
            said.push_str(&format!(
                " — but what {} checks was changed in the same run, so a pass there is not news yet",
                touched.join(" and ")
            ));
        }
        said
    }
}

/// Read the scorecard a workspace declares, or `None` where it declares none.
pub fn read(workspace: &Path) -> Result<Option<Scorecard>, String> {
    let path = scorecard_path(workspace);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let card: Scorecard = toml::from_str(&text)
        .map_err(|e| format!("{} is not a scorecard this can read: {e}", path.display()))?;
    if let Some(unnamed) = card.checks.iter().position(|c| c.name.trim().is_empty()) {
        return Err(format!(
            "the check at position {} in {} has no `name`, and the name is how one run is \
             compared with the last",
            unnamed + 1,
            path.display()
        ));
    }
    if let Some(empty) = card.checks.iter().find(|c| c.run.trim().is_empty()) {
        return Err(format!("`{}` in {} has no `run`", empty.name, path.display()));
    }
    Ok(Some(card))
}

/// What the files a scorecard watches look like right now.
///
/// Taken before the work and again after it, so "this changed while it was
/// being measured" is a comparison rather than a guess. Content rather than
/// mtime: a checkout, a formatter and a rebuild all move mtimes without
/// changing what a check would find.
pub fn witness(workspace: &Path, card: &Scorecard) -> Witness {
    let mut seen = BTreeMap::new();
    seen.insert(String::from(".rook/evaluation.toml"), hash_of(&scorecard_path(workspace)));
    for check in &card.checks {
        for guard in &check.guards {
            for path in matching(workspace, guard) {
                let named =
                    path.strip_prefix(workspace).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                seen.insert(named, hash_of(&path));
            }
        }
    }
    Witness { seen }
}

/// The state of what a scorecard watches, at one moment.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Witness {
    /// Workspace-relative path to a content hash. A path that does not exist
    /// has `None`, which is a state of its own: a guarded file appearing or
    /// disappearing is exactly as interesting as its contents changing.
    seen: BTreeMap<String, Option<String>>,
}

impl Witness {
    /// What differs between then and now, as workspace-relative paths.
    fn differs_from(&self, earlier: &Witness) -> Vec<String> {
        let mut changed: Vec<String> = Vec::new();
        for (path, now) in &self.seen {
            if earlier.seen.get(path).is_none_or(|before| before != now) {
                changed.push(path.clone());
            }
        }
        // A guarded file that was deleted is gone from `seen` on this side.
        for path in earlier.seen.keys() {
            if !self.seen.contains_key(path) {
                changed.push(path.clone());
            }
        }
        changed.sort();
        changed.dedup();
        changed
    }
}

/// Without holding the file: a guarded path may be somebody's fixture and this
/// is asked of every one of them twice a run.
fn hash_of(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    rook_store::ObjectId::of_reader(std::io::BufReader::new(file)).ok().map(|id| id.to_string())
}

/// The files a guard names, relative to the workspace.
///
/// A leading `**/` and a trailing `/**` are understood, and so is a `*` inside
/// one segment; anything else is a literal path. Deliberately small — a guard
/// is somebody naming the tests for one check, not a build system's globber,
/// and a pattern language nobody can predict is worse here than a list.
fn matching(workspace: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = pattern.trim().trim_start_matches("./");
    if !pattern.contains('*') {
        let path = workspace.join(pattern);
        return match path.exists() {
            true if path.is_dir() => under(&path),
            true => vec![path],
            false => Vec::new(),
        };
    }
    let mut found = Vec::new();
    for path in under(workspace) {
        let Ok(relative) = path.strip_prefix(workspace) else { continue };
        if covers(pattern, &relative.to_string_lossy().replace('\\', "/")) {
            found.push(path);
        }
    }
    found
}

/// Every file under a directory, skipping the places nobody guards and
/// everybody has: a walk into `target/` or `.git/` is minutes, not a check.
fn under(root: &Path) -> Vec<PathBuf> {
    const SKIP: &[&str] = &["target", ".git", "node_modules", ".venv", "dist", "build"];
    let mut found = Vec::new();
    let mut look = vec![root.to_path_buf()];
    while let Some(at) = look.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            match entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                true if !SKIP.contains(&name.as_str()) => look.push(path),
                true => {}
                false => found.push(path),
            }
        }
    }
    found
}

/// Whether a small glob covers a workspace-relative path.
fn covers(pattern: &str, path: &str) -> bool {
    let (pattern, path) = (pattern.trim_start_matches("**/"), path);
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(&format!("{prefix}/"));
    }
    let (want, have): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    // `**` in the middle would need a real matcher; a guard that wants it can
    // name the directory instead, which `/**` already covers.
    if want.len() != have.len() {
        return false;
    }
    want.iter().zip(have).all(|(want, have)| segment(want, have))
}

/// One path segment against one pattern segment, where `*` stands for any run
/// of characters that is not a separator.
fn segment(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else { return pattern == name };
    if !name.starts_with(first) {
        return false;
    }
    // `find` returns a byte index at a character boundary, which is what the
    // slicing below needs and is why it is done this way rather than by
    // counting.
    let mut at = first.len();
    let mut last = None;
    for part in parts {
        last = Some(part);
        if part.is_empty() {
            continue;
        }
        match name.get(at..).and_then(|rest| rest.find(part)) {
            Some(found) => at += found + part.len(),
            None => return false,
        }
    }
    match last {
        // A trailing `*` takes whatever is left.
        Some("") => true,
        Some(tail) => name.ends_with(tail) && at <= name.len(),
        None => at == name.len(),
    }
}

/// Run every check, against the workspace as it stands.
///
/// Blocking and outside any turn: this is the harness measuring, not the agent
/// working, and it must not be reachable as a tool. A model that could run its
/// own evaluation could run it until it passed.
pub fn run(workspace: &Path, card: &Scorecard, before: &Witness) -> Report {
    let after = witness(workspace, card);
    let changed = after.differs_from(before);
    let scorecard_changed = changed.iter().any(|p| p == ".rook/evaluation.toml");

    let mut checks = Vec::new();
    for check in &card.checks {
        let began = std::time::Instant::now();
        let (status, said) = ran(workspace, &check.run, check.timeout_secs);
        let measured = match check.measures.trim().is_empty() {
            true => None,
            false => number_in(&said),
        };
        let guarded: Vec<String> = changed
            .iter()
            .filter(|path| {
                *path != ".rook/evaluation.toml"
                    && check.guards.iter().any(|guard| covers(guard.trim().trim_start_matches("./"), path))
            })
            .cloned()
            .collect();
        checks.push(Scored {
            name: check.name.clone(),
            passed: status == Some(check.expect),
            status,
            took_ms: began.elapsed().as_millis() as u64,
            measured,
            measures: check.measures.clone(),
            said: tail_of(&said),
            touched: guarded,
        });
    }
    Report { checks, scorecard_changed }
}

/// The last of what a command printed — enough to act on, and not a log.
fn tail_of(said: &str) -> String {
    const MOST: usize = 2_000;
    let said = said.trim_end();
    match said.len() > MOST {
        false => said.to_string(),
        // From a character boundary, because this is a command's output and
        // nothing about it is guaranteed to be ASCII.
        true => {
            let from = said.len() - MOST;
            let from = (from..said.len()).find(|at| said.is_char_boundary(*at)).unwrap_or(said.len());
            format!("…\n{}", &said[from..])
        }
    }
}

/// The last line read as a number, where it is one.
fn number_in(said: &str) -> Option<f64> {
    said.lines().rev().find(|line| !line.trim().is_empty())?.trim().parse().ok()
}

/// Most of one check's output to keep. A test suite that fails everywhere
/// prints megabytes and what anybody reads is the end of it.
const MOST_OUTPUT: usize = 256 * 1024;

/// Run one check and collect what it said, both streams together.
///
/// Both streams are drained on threads of their own, and that is not tidiness:
/// a check whose stderr filled its pipe while nobody read it would block
/// forever, which is how `hooks` once deadlocked here. Bounded while the bytes
/// arrive rather than after, for the same reason every other accumulator is.
fn ran(workspace: &Path, command: &str, timeout_secs: u64) -> (Option<i32>, String) {
    let (shell, flag) = match cfg!(windows) {
        true => ("cmd", "/C"),
        false => ("sh", "-c"),
    };
    let mut built = std::process::Command::new(shell);
    built
        .arg(flag)
        .arg(command)
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Without a console of its own: a scorecard of ten checks would otherwise
    // be ten windows opening and shutting on Windows, once per run.
    let child = rook_contain::quietly(&mut built).spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return (None, format!("could not run {command:?}: {e}")),
    };

    let draining: Vec<_> = [child.stdout.take().map(Drained::Out), child.stderr.take().map(Drained::Err)]
        .into_iter()
        .flatten()
        .map(|stream| std::thread::spawn(move || stream.read_to_end()))
        .collect();

    let patience = std::time::Duration::from_secs(match timeout_secs {
        0 => PATIENCE,
        given => given,
    });
    let began = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if began.elapsed() >= patience => {
                let _ = child.kill();
                let _ = child.wait();
                // `None` rather than a failing code: stopped at a deadline is
                // not the same as failed, and a report that called it one would
                // send somebody looking for a bug in a suite that is merely
                // slow.
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return (None, format!("waiting for {command:?}: {e}")),
        }
    };

    let mut said = String::new();
    for reader in draining {
        if let Ok(part) = reader.join() {
            said.push_str(&part);
        }
    }
    if status.is_none() && began.elapsed() >= patience {
        said.push_str(&format!("\n(stopped after {}s without finishing)", patience.as_secs()));
    }
    (status, said)
}

/// One of a child's two streams, so both can be drained the same way.
enum Drained {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Drained {
    fn read_to_end(self) -> String {
        use std::io::Read;
        let mut source: Box<dyn Read> = match self {
            Drained::Out(out) => Box::new(out),
            Drained::Err(err) => Box::new(err),
        };
        let mut kept: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Kept to the cap and then read past it rather than stopped:
                    // the writer is still on the other end, and a reader that
                    // stops is the deadlock this exists to avoid.
                    if kept.len() < MOST_OUTPUT {
                        let room = MOST_OUTPUT - kept.len();
                        kept.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    }
}
