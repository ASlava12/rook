//! Discovery and resolution of installed skills.
//!
//! Two problems shape this module.
//!
//! **Token cost.** Injecting every skill body into every request is what makes
//! large skill libraries unaffordable. Discovery therefore produces [`SkillCard`]s
//! — name, description, applicability — and nothing else. A body is read only
//! when the agent asks for that skill by name.
//!
//! **Silent non-application.** A skill that never fires because its requirements
//! do not match looks identical to one that is simply being ignored. Resolution
//! always returns the reasons, so `rook skills why <name>` can print them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::env::Environment;
use crate::error::{Result, SkillError};
use crate::manifest::{self, Mismatch, SkillManifest, Variant};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
    /// Compiled into the binary or shipped alongside it.
    Builtin,
    /// `~/.rook/skills`
    User,
    /// `<workspace>/.rook/skills`
    Project,
    /// Installed as part of an Agent Plugins package.
    Plugin(String),
}

impl SkillSource {
    /// Higher wins. A skill vendored into the project is there on purpose and
    /// overrides a newer one from anywhere else.
    pub fn rank(&self) -> u8 {
        match self {
            SkillSource::Builtin => 0,
            SkillSource::Plugin(_) => 1,
            SkillSource::User => 2,
            SkillSource::Project => 3,
        }
    }

    pub fn label(&self) -> String {
        match self {
            SkillSource::Builtin => "builtin".into(),
            SkillSource::User => "user".into(),
            SkillSource::Project => "project".into(),
            SkillSource::Plugin(p) => format!("plugin:{p}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Skill {
    pub manifest: SkillManifest,
    pub body: String,
    pub dir: PathBuf,
    pub source: SkillSource,
}

/// What the agent sees before deciding to load anything: a few dozen tokens per
/// skill instead of a few thousand.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillCard {
    pub name: String,
    pub description: String,
    pub version: String,
    pub source: String,
    pub keywords: Vec<String>,
    /// False when `requires` does not match the current environment.
    pub applicable: bool,
    pub mismatches: Vec<String>,
    /// Rough size of the body if it were loaded, for budgeting.
    pub body_tokens: usize,
}

impl Skill {
    pub fn load(dir: impl AsRef<Path>, source: SkillSource) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let path = skill_file(&dir).ok_or_else(|| SkillError::NotFound(dir.display().to_string()))?;
        let text = std::fs::read_to_string(&path).map_err(|e| SkillError::io(&path, e))?;
        let (manifest, body) = manifest::parse(&text, &path)?;
        Ok(Self { manifest, body, dir, source })
    }

    pub fn id(&self) -> String {
        format!("{}@{}", self.manifest.name, self.manifest.semver())
    }

    pub fn version(&self) -> Version {
        self.manifest.semver()
    }

    pub fn card(&self, env: &Environment) -> SkillCard {
        let mismatches = self.manifest.requires.check(env);
        SkillCard {
            name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            version: self.manifest.semver().to_string(),
            source: self.source.label(),
            keywords: self.manifest.keywords.clone(),
            applicable: mismatches.is_empty(),
            mismatches: mismatches.iter().map(|m| m.to_string()).collect(),
            body_tokens: estimate_tokens(&self.body),
        }
    }

    /// Pick the body for `env`: the most specific matching variant, or the
    /// default body when none matches.
    pub fn body_for(&self, env: &Environment) -> Result<(String, Option<Variant>)> {
        let mut best: Option<&Variant> = None;
        for v in &self.manifest.variants {
            if !v.when.satisfied_by(env) {
                continue;
            }
            let better = match best {
                None => true,
                Some(cur) => v.when.specificity() > cur.when.specificity(),
            };
            if better {
                best = Some(v);
            }
        }
        match best {
            None => Ok((self.body.clone(), None)),
            Some(v) => {
                let path = self.dir.join(&v.body);
                let text = std::fs::read_to_string(&path).map_err(|e| SkillError::io(&path, e))?;
                Ok((text, Some(v.clone())))
            }
        }
    }

    /// Files bundled with the skill (scripts, references, assets), relative to
    /// its directory. These are what version control and packaging operate on.
    pub fn resources(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.dir).follow_links(false).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            if let Ok(rel) = entry.path().strip_prefix(&self.dir) {
                out.push(rel.to_path_buf());
            }
        }
        out.sort();
        out
    }
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub skill: Skill,
    pub variant: Option<Variant>,
    pub body: String,
    /// Versions that were rejected, and why. Empty when the newest applied.
    pub rejected: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, Default)]
pub struct SkillIndex {
    skills: Vec<Skill>,
}

impl SkillIndex {
    /// Walk `roots` in order. Errors are collected rather than fatal: one broken
    /// skill must not take down the whole catalog.
    pub fn discover(roots: &[(PathBuf, SkillSource)]) -> (Self, Vec<SkillError>) {
        let mut skills = Vec::new();
        let mut errors = Vec::new();
        for (root, source) in roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if skill_file(&path).is_some() {
                    match Skill::load(&path, source.clone()) {
                        Ok(s) => skills.push(s),
                        Err(e) => errors.push(e),
                    }
                    continue;
                }
                // `<name>/<version>/SKILL.md` — several versions side by side.
                let Ok(versions) = std::fs::read_dir(&path) else { continue };
                for v in versions.flatten() {
                    let vpath = v.path();
                    if vpath.is_dir() && skill_file(&vpath).is_some() {
                        match Skill::load(&vpath, source.clone()) {
                            Ok(s) => skills.push(s),
                            Err(e) => errors.push(e),
                        }
                    }
                }
            }
        }
        (Self { skills }, errors)
    }

    pub fn from_skills(skills: Vec<Skill>) -> Self {
        Self { skills }
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    pub fn versions_of(&self, name: &str) -> Vec<&Skill> {
        let mut out: Vec<&Skill> = self.skills.iter().filter(|s| s.manifest.name == name).collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.version()));
        out
    }

    /// Best candidate per name, for the prompt-time catalog.
    pub fn catalog(&self, env: &Environment) -> Vec<SkillCard> {
        let mut by_name: BTreeMap<&str, &Skill> = BTreeMap::new();
        for s in &self.skills {
            let name = s.manifest.name.as_str();
            match by_name.get(name) {
                Some(cur) if !better(s, cur, env) => {}
                _ => {
                    by_name.insert(name, s);
                }
            }
        }
        by_name.values().map(|s| s.card(env)).collect()
    }

    /// Resolve `name` for `env`, honouring source precedence then version order.
    pub fn resolve(&self, name: &str, env: &Environment) -> Result<Resolved> {
        let mut candidates: Vec<&Skill> = self
            .skills
            .iter()
            .filter(|s| s.manifest.name == name || s.manifest.supersedes.iter().any(|o| o == name))
            .collect();
        if candidates.is_empty() {
            return Err(SkillError::NotFound(name.to_string()));
        }
        candidates.sort_by(|a, b| {
            b.source.rank().cmp(&a.source.rank()).then_with(|| b.version().cmp(&a.version()))
        });

        let mut rejected = Vec::new();
        for skill in &candidates {
            let mismatches: Vec<Mismatch> = skill.manifest.requires.check(env);
            if mismatches.is_empty() {
                let (body, variant) = skill.body_for(env)?;
                return Ok(Resolved { skill: (*skill).clone(), variant, body, rejected });
            }
            rejected.push((skill.id(), mismatches.iter().map(|m| m.to_string()).collect()));
        }

        let detail = rejected
            .iter()
            .map(|(id, reasons)| format!("{id}: {}", reasons.join("; ")))
            .collect::<Vec<_>>()
            .join(" | ");
        Err(SkillError::NoCompatibleVersion { name: name.to_string(), detail })
    }
}

fn better(candidate: &Skill, current: &Skill, env: &Environment) -> bool {
    let ca = candidate.manifest.requires.satisfied_by(env);
    let cu = current.manifest.requires.satisfied_by(env);
    // Applicable beats inapplicable, then source, then version.
    (ca, candidate.source.rank(), candidate.version()) > (cu, current.source.rank(), current.version())
}

fn skill_file(dir: &Path) -> Option<PathBuf> {
    for name in ["SKILL.md", "skill.md"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Cheap token estimate. Good enough for budgeting; never used for billing.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}
