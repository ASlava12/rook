//! `SKILL.md` parsing.
//!
//! The base format is the Agent Skills specification: a YAML frontmatter block
//! with `name` and `description` required, followed by a Markdown body. Skills
//! written for other agents load here unchanged.
//!
//! Two additions live under keys the spec leaves free, so a Rook skill stays a
//! valid skill everywhere else:
//!
//! - `requires:` — the environment the skill is valid in (OS, arch, userland,
//!   language and tool versions, agent version).
//! - `variants:` — alternative bodies selected by the same predicate, so one
//!   skill can carry its platform-specific parts instead of forking into
//!   `foo-linux` and `foo-windows`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::env::Environment;
use crate::error::{Result, SkillError};

/// Predicate over an [`Environment`]. An empty field means "no constraint".
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Requirements {
    #[serde(default)]
    pub os: Vec<String>,
    #[serde(default)]
    pub arch: Vec<String>,
    #[serde(default)]
    pub userland: Vec<String>,
    #[serde(default)]
    pub agent: Option<String>,
    /// Language toolchain constraints, e.g. `rust: ">=1.75"`.
    #[serde(default)]
    pub language: BTreeMap<String, String>,
    /// Standalone tool constraints, e.g. `git: ">=2.30"`.
    #[serde(default)]
    pub tool: BTreeMap<String, String>,
}

/// Why a candidate did not apply. Surfaced verbatim by `rook skills why`, so a
/// skill that silently never fires stops being a mystery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mismatch {
    Os { want: Vec<String>, got: String },
    Arch { want: Vec<String>, got: String },
    Userland { want: Vec<String>, got: String },
    AgentVersion { want: String, got: String },
    LanguageMissing { key: String, want: String },
    LanguageVersion { key: String, want: String, got: String },
    ToolMissing { key: String, want: String },
    ToolVersion { key: String, want: String, got: String },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::Os { want, got } => write!(f, "needs os {want:?}, running {got}"),
            Mismatch::Arch { want, got } => write!(f, "needs arch {want:?}, running {got}"),
            Mismatch::Userland { want, got } => write!(f, "needs {want:?} userland, running {got}"),
            Mismatch::AgentVersion { want, got } => write!(f, "needs agent {want}, running {got}"),
            Mismatch::LanguageMissing { key, want } => write!(f, "{key} {want} not found on PATH"),
            Mismatch::LanguageVersion { key, want, got } => write!(f, "needs {key} {want}, found {got}"),
            Mismatch::ToolMissing { key, want } => write!(f, "tool {key} {want} not found on PATH"),
            Mismatch::ToolVersion { key, want, got } => write!(f, "needs {key} {want}, found {got}"),
        }
    }
}

impl Requirements {
    pub fn is_empty(&self) -> bool {
        self.os.is_empty()
            && self.arch.is_empty()
            && self.userland.is_empty()
            && self.agent.is_none()
            && self.language.is_empty()
            && self.tool.is_empty()
    }

    /// Check every constraint, collecting all failures rather than the first, so
    /// a user fixing their environment learns everything at once.
    pub fn check(&self, env: &Environment) -> Vec<Mismatch> {
        let mut out = Vec::new();

        if !self.os.is_empty() && !self.os.iter().any(|o| o.eq_ignore_ascii_case(&env.os)) {
            out.push(Mismatch::Os { want: self.os.clone(), got: env.os.clone() });
        }
        if !self.arch.is_empty() && !self.arch.iter().any(|a| a.eq_ignore_ascii_case(&env.arch)) {
            out.push(Mismatch::Arch { want: self.arch.clone(), got: env.arch.clone() });
        }
        if !self.userland.is_empty() && !self.userland.iter().any(|u| u.eq_ignore_ascii_case(&env.userland)) {
            out.push(Mismatch::Userland { want: self.userland.clone(), got: env.userland.clone() });
        }
        if let Some(req) = &self.agent
            && !version_matches(req, &env.agent_version)
        {
            out.push(Mismatch::AgentVersion { want: req.clone(), got: env.agent_version.clone() });
        }
        for (key, req) in &self.language {
            match env.languages.get(key) {
                None => out.push(Mismatch::LanguageMissing { key: key.clone(), want: req.clone() }),
                Some(got) if !version_matches(req, got) => out.push(Mismatch::LanguageVersion {
                    key: key.clone(),
                    want: req.clone(),
                    got: got.clone(),
                }),
                Some(_) => {}
            }
        }
        for (key, req) in &self.tool {
            match env.tools.get(key) {
                None => out.push(Mismatch::ToolMissing { key: key.clone(), want: req.clone() }),
                Some(got) if !version_matches(req, got) => {
                    out.push(Mismatch::ToolVersion { key: key.clone(), want: req.clone(), got: got.clone() })
                }
                Some(_) => {}
            }
        }
        out
    }

    pub fn satisfied_by(&self, env: &Environment) -> bool {
        self.check(env).is_empty()
    }

    /// How tightly this predicate constrains the environment. Used to break ties
    /// between variants: the most specific match wins, so a Windows-specific
    /// body beats a generic one.
    pub fn specificity(&self) -> usize {
        self.os.len().min(1)
            + self.arch.len().min(1)
            + self.userland.len().min(1)
            + usize::from(self.agent.is_some())
            + self.language.len()
            + self.tool.len()
    }

    /// Validate every version requirement, so a typo fails at load time rather
    /// than silently never matching.
    pub fn validate(&self, path: &Path) -> Result<()> {
        let check = |field: String, value: &String| -> Result<()> {
            VersionReq::parse(value).map_err(|e| SkillError::BadVersionReq {
                path: path.to_path_buf(),
                field,
                value: value.clone(),
                reason: e.to_string(),
            })?;
            Ok(())
        };
        if let Some(a) = &self.agent {
            check("requires.agent".into(), a)?;
        }
        for (k, v) in &self.language {
            check(format!("requires.language.{k}"), v)?;
        }
        for (k, v) in &self.tool {
            check(format!("requires.tool.{k}"), v)?;
        }
        Ok(())
    }
}

/// `"1.97.1"` against `">=1.75, <2.0"`. Unparseable versions never match, which
/// keeps a weird `--version` banner from silently enabling a skill.
pub fn version_matches(req: &str, version: &str) -> bool {
    let Ok(req) = VersionReq::parse(req) else { return false };
    let Ok(v) = Version::parse(version) else { return false };
    // Pre-release versions match only when the requirement mentions one; that is
    // semver's rule and it is the right default for toolchains.
    req.matches(&v)
}

/// A platform-specific alternative body for the same skill.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variant {
    #[serde(default)]
    pub when: Requirements,
    /// Relative to the skill directory.
    pub body: PathBuf,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    /// Defaults to `0.0.0` for skills written against the bare spec.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Tools the skill is permitted to use, mirroring the spec's field name.
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub requires: Requirements,
    #[serde(default)]
    pub variants: Vec<Variant>,
    /// Older skill names this one takes over from.
    #[serde(default)]
    pub supersedes: Vec<String>,
    /// Anything else in the frontmatter is preserved verbatim, so a skill
    /// carrying another agent's fields round-trips unchanged.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl SkillManifest {
    pub fn semver(&self) -> Version {
        Version::parse(&self.version).unwrap_or_else(|_| Version::new(0, 0, 0))
    }

    /// The whole file: this manifest as frontmatter, then the body.
    ///
    /// Serialised rather than formatted by hand, so a field added to the
    /// manifest is written as well as read, and `extra` carries another agent's
    /// fields back out unchanged.
    pub fn to_skill_md(&self, body: &str) -> Result<String> {
        let front = serde_yaml_ng::to_string(self).map_err(|e| SkillError::BadFrontmatter {
            path: PathBuf::from(&self.name),
            reason: e.to_string(),
        })?;
        Ok(format!("---\n{front}---\n\n{}\n", body.trim()))
    }
}

pub fn split_frontmatter<'a>(text: &'a str, path: &Path) -> Result<(&'a str, &'a str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| SkillError::NoFrontmatter { path: path.to_path_buf() })?;

    // Find the closing fence at the start of a line.
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" || trimmed == "..." {
            let body_start = offset + line.len();
            return Ok((&rest[..offset], &rest[body_start..]));
        }
        offset += line.len();
    }
    Err(SkillError::NoFrontmatter { path: path.to_path_buf() })
}

pub fn parse(text: &str, path: &Path) -> Result<(SkillManifest, String)> {
    let (front, body) = split_frontmatter(text, path)?;
    let manifest: SkillManifest = serde_yaml_ng::from_str(front)
        .map_err(|e| SkillError::BadFrontmatter { path: path.to_path_buf(), reason: e.to_string() })?;

    if manifest.name.trim().is_empty() {
        return Err(SkillError::MissingField { path: path.to_path_buf(), field: "name" });
    }
    if manifest.description.trim().is_empty() {
        return Err(SkillError::MissingField { path: path.to_path_buf(), field: "description" });
    }
    manifest.requires.validate(path)?;
    for variant in &manifest.variants {
        variant.when.validate(path)?;
    }
    Ok((manifest, body.to_string()))
}
