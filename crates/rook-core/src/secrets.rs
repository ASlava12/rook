//! Secrets the agent can use and cannot leak.
//!
//! A secret is named, never valued, in everything the model sees: it asks for
//! `deploy_token` and the value is put in on the way out, inside the tool, into
//! the environment of the process that needs it. So it is not in the transcript,
//! not in the store, not in what compaction summarises, not in what a sub-agent
//! inherits, and not in any request to a provider.
//!
//! What that does not do is stop a model from printing one on purpose: a
//! command that can use a secret can echo it. Nothing here claims otherwise —
//! what stops that is the approval policy and the sandbox. This keeps a secret
//! out of the places it would end up *by accident*, which is where secrets
//! actually end up.
//!
//! Values live in `~/.rook/secrets.toml`, 0600, in the clear — the same
//! property as `~/.ssh/id_rsa` without a passphrase, `~/.aws/credentials` and
//! `.netrc`, and the same as `config.toml` already has, where an MCP server's
//! API key goes. Encrypting it under a passphrase was declined: an agent that
//! cannot run without somebody to type one is not an agent, and encrypting it
//! under a key from the OS keychain defends against an attacker who, having
//! this account, can read the value out of the running process anyway.
//!
//! A value that already lives somewhere else stays there. `env:`, `cmd:` and
//! `keychain:` name where to get it, so a password manager keeps its job and
//! this file holds nothing at all.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Where a secret's value comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// In the file, because somebody handed it over and had nowhere else to
    /// put it. `rook secrets add` writes these.
    Kept,
    /// A variable of this process's environment.
    Env(String),
    /// The output of a command, run when the value is needed: `op read`,
    /// `pass show`, `vault kv get`. The manager keeps the value; this keeps the
    /// question.
    Command(String),
    /// The OS keychain, by `service/account`.
    ///
    /// Read only. Writing one means passing the value as an argument to
    /// `security` or `secret-tool`, where every process on the machine can read
    /// it out of the process list for as long as the call takes — so it is put
    /// there by the person, with their own tool, and read from here.
    Keychain(String),
}

impl Source {
    /// How it is written in the file, and shown by `secrets ls`. A kept value
    /// is `kept` and never the value itself.
    pub fn as_str(&self) -> String {
        match self {
            Source::Kept => "kept".into(),
            Source::Env(name) => format!("env:{name}"),
            Source::Command(line) => format!("cmd:{line}"),
            Source::Keychain(what) => format!("keychain:{what}"),
        }
    }

    /// `None` for a spelling nothing can act on, which is a thing to say rather
    /// than a thing to guess at.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(name) = text.strip_prefix("env:") {
            return (!name.trim().is_empty()).then(|| Source::Env(name.trim().to_string()));
        }
        if let Some(line) = text.strip_prefix("cmd:") {
            return (!line.trim().is_empty()).then(|| Source::Command(line.trim().to_string()));
        }
        if let Some(what) = text.strip_prefix("keychain:") {
            return (!what.trim().is_empty()).then(|| Source::Keychain(what.trim().to_string()));
        }
        None
    }
}

/// One secret as a reader of the list sees it: what it is called, where it
/// comes from, and whether that answers. Never the value — there is no call
/// anywhere that returns one, which is what makes the rest of this worth doing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Named {
    pub name: String,
    pub source: String,
    /// Whether asking for it right now would produce something. A `cmd:` that
    /// needs an unlocked password manager says `false` until it is unlocked,
    /// which is the answer somebody debugging wants.
    pub resolves: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Entry {
    /// Present for a kept secret and for no other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    /// Present for every other kind: `env:NAME`, `cmd:…`, `keychain:…`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

/// The secrets this machine holds, and the values handed out this turn.
///
/// One object for both because they are one question: what a tool is given is
/// what has to be taken back out of whatever comes back. Handed to the toolbox
/// as `rook_tools::Secrets`, built per turn, and dropped with it — so a value
/// resolved for one turn is not in memory for the next.
pub struct Vault {
    entries: BTreeMap<String, Entry>,
    path: std::path::PathBuf,
    /// What has actually been handed to a tool, for the redaction. A `cmd:`
    /// secret nobody asked for is never resolved: running a password manager
    /// once per tool result to see whether its answer is in there would cost
    /// more than the whole feature.
    handed_out: std::sync::Mutex<Vec<String>>,
}

impl Vault {
    /// A vault holding nothing, for a machine with no secrets and for a loop
    /// whose file would not read: a name asked for then is a name nobody set,
    /// which is the answer that sends somebody to `rook secrets`.
    pub fn empty() -> Self {
        Self { entries: BTreeMap::new(), path: crate::paths::secrets_file(), handed_out: Default::default() }
    }

    pub fn load() -> Result<Self> {
        Self::load_from(crate::paths::secrets_file())
    }

    pub fn load_from(path: std::path::PathBuf) -> Result<Self> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                crate::CoreError::Other(format!("{} is not readable as secrets: {e}", path.display()))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(crate::CoreError::Io { path: path.clone(), source }),
        };
        Ok(Self { entries, path, handed_out: Default::default() })
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            crate::paths::private_dir(parent)
                .map_err(|source| crate::CoreError::Io { path: parent.to_path_buf(), source })?;
        }
        let text = toml::to_string_pretty(&self.entries)
            .map_err(|e| crate::CoreError::Other(format!("writing secrets: {e}")))?;
        std::fs::write(&self.path, text)
            .map_err(|source| crate::CoreError::Io { path: self.path.clone(), source })?;
        // Before anything else can open it. A file of passwords written
        // world-readable for even a moment is a file that was world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .map_err(|source| crate::CoreError::Io { path: self.path.clone(), source })?;
        }
        Ok(())
    }

    /// Hold a value here, because there was nowhere else to put it.
    pub fn keep(&mut self, name: &str, value: &str) -> Result<()> {
        let name = usable_name(name)?;
        let value = cleaned(value);
        if value.is_empty() {
            return Err(crate::CoreError::Other(format!(
                "{name:?} would be empty: what was given is whitespace and invisible characters, \
                 which is what a bad paste looks like"
            )));
        }
        self.entries.insert(name, Entry { value: Some(value), source: None });
        self.save()
    }

    /// Name where a value lives without holding it.
    pub fn refer(&mut self, name: &str, source: &Source) -> Result<()> {
        let name = usable_name(name)?;
        self.entries.insert(name, Entry { value: None, source: Some(source.as_str()) });
        self.save()
    }

    pub fn forget(&mut self, name: &str) -> Result<bool> {
        let gone = self.entries.remove(name.trim()).is_some();
        if gone {
            self.save()?;
        }
        Ok(gone)
    }

    /// Every secret, with its source and whether it answers right now.
    ///
    /// Resolving each one to answer `resolves` is the cost of a listing, and it
    /// is what makes the listing worth reading: a `cmd:` against a locked
    /// manager and a name nobody set are two different problems that look the
    /// same in a list of names.
    pub fn named(&self) -> Vec<Named> {
        self.entries
            .iter()
            .map(|(name, entry)| Named {
                name: name.clone(),
                source: self.source_of(entry).as_str(),
                resolves: self.value_of(entry).is_some(),
            })
            .collect()
    }

    fn source_of(&self, entry: &Entry) -> Source {
        entry.source.as_deref().and_then(Source::parse).unwrap_or(Source::Kept)
    }

    fn value_of(&self, entry: &Entry) -> Option<String> {
        let raw = match self.source_of(entry) {
            Source::Kept => entry.value.clone(),
            Source::Env(name) => std::env::var(name).ok(),
            Source::Command(line) => from_command(&line),
            Source::Keychain(what) => from_keychain(&what),
        };
        // Every source, not only the one typed here: a variable exported by a
        // script keeps its trailing newline, and a manager that prints a banner
        // prints it in whatever encoding it likes.
        raw.map(|value| cleaned(&value)).filter(|value| !value.is_empty())
    }

    /// The value, and remembered as handed out so it can be taken back out of
    /// what comes back.
    pub fn value(&self, name: &str) -> Option<String> {
        let value = self.value_of(self.entries.get(name.trim())?)?;
        if let Ok(mut handed) = self.handed_out.lock()
            && !handed.contains(&value)
        {
            handed.push(value.clone());
        }
        Some(value)
    }

    /// Whatever a tool answered, with every value this vault has handed out
    /// taken back out of it.
    ///
    /// The one place it happens, so a command's output, a page, a file and an
    /// MCP server's answer are all covered by one rule rather than four. It is
    /// not airtight and does not pretend to be: a value split across a line, or
    /// base64'd, or printed a character at a time goes through. What it stops is
    /// the ordinary way a secret ends up in a transcript — `echo $TOKEN`, a
    /// curl trace, a config file printed by `cat`.
    pub fn redact(&self, text: &str) -> String {
        let Ok(handed) = self.handed_out.lock() else { return text.to_string() };
        let mut out = text.to_string();
        for value in handed.iter() {
            // Short values would match half the output of anything. A secret
            // this short is not one, and saying so beats mangling a transcript.
            if value.len() < 6 {
                continue;
            }
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), "${secret}");
            }
        }
        out
    }
}

/// Run a command for its output, trimmed. Nothing is logged, and the value goes
/// nowhere but back to the caller.
fn from_command(line: &str) -> Option<String> {
    #[cfg(windows)]
    let out = std::process::Command::new("cmd").args(["/C", line]).output().ok()?;
    #[cfg(not(windows))]
    let out = std::process::Command::new("/bin/sh").arg("-c").arg(line).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?.trim_end_matches(['\n', '\r']).to_string();
    (!value.is_empty()).then_some(value)
}

/// Read one out of the OS keychain, by `service/account`.
///
/// Reading only, and by name: the name is what lands in the process list, which
/// is where writing one would have put the value.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn from_keychain(what: &str) -> Option<String> {
    let (service, account) = what.split_once('/')?;
    #[cfg(target_os = "macos")]
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .output()
        .ok()?;
    #[cfg(target_os = "linux")]
    let out = std::process::Command::new("secret-tool")
        .args(["lookup", "service", service, "account", account])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?.trim_end_matches(['\n', '\r']).to_string();
    (!value.is_empty()).then_some(value)
}

/// Nothing to read here yet.
///
/// Windows has a Credential Manager and FreeBSD has nothing of the kind, and
/// neither is reached by a command this would recognise. `cmd:` covers both
/// without this knowing anything: `cmd:powershell -c "…"` is a source like any
/// other. A separate function rather than a branch inside one, because a branch
/// that ends in `return` leaves the binding above it unused, and an unused
/// binding is an error under `-D warnings` on exactly the platform nothing here
/// compiles for locally.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn from_keychain(_what: &str) -> Option<String> {
    None
}

/// A value without what a paste puts around it.
///
/// Ported from cline: a key pasted with a byte-order mark or a zero-width space
/// is stored corrupted and then answers 401 indistinguishably from a wrong one
/// — and here nothing prints a value, so the one way to see the difference is
/// gone. Control and format characters are never part of a credential, and
/// neither is the whitespace around one.
fn cleaned(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            !c.is_control()
                // Cf, which `is_control` does not cover: the byte-order mark, the
                // zero-width spaces and joiners, the bidirectional marks, and the
                // soft hyphen a word processor leaves behind.
                && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}')
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// A name a tool argument can carry and a shell can hold in a variable.
fn usable_name(name: &str) -> Result<String> {
    let name = name.trim();
    let usable = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && name.starts_with(|c: char| c.is_ascii_alphabetic());
    match usable {
        true => Ok(name.to_string()),
        false => Err(crate::CoreError::Other(format!(
            "{name:?} is not a usable secret name: letters, digits, `_` and `-`, starting with a \
             letter — it becomes an environment variable"
        ))),
    }
}

impl rook_tools::Secrets for Vault {
    fn value(&self, name: &str) -> Option<String> {
        Vault::value(self, name)
    }
}
