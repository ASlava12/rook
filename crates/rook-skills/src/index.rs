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

    /// The inverse of [`label`](Self::label).
    ///
    /// A [`SkillCard`] carries where it came from as the label, because that is
    /// what a person reads in `rook skills ls` — and the catalogue has to rank
    /// cards by it when there are more than fit. Read back here rather than
    /// carried twice: two fields for one fact are two fields to keep in step.
    ///
    /// An unknown label is `Builtin`, which is the lowest rank: something this
    /// build does not recognise is not a reason to drop what the user put in
    /// their own workspace.
    pub fn from_label(label: &str) -> Self {
        match label {
            "user" => SkillSource::User,
            "project" => SkillSource::Project,
            _ => match label.strip_prefix("plugin:") {
                Some(name) => SkillSource::Plugin(name.to_string()),
                None => SkillSource::Builtin,
            },
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
        let mut remaining = usize::MAX;
        Self::load_budgeted(dir.as_ref(), source, &mut remaining)
    }

    fn load_budgeted(dir: &Path, source: SkillSource, remaining: &mut usize) -> Result<Self> {
        let dir = dir.to_path_buf();
        let path = skill_file(&dir).ok_or_else(|| SkillError::NotFound(dir.display().to_string()))?;
        let allowance = (*remaining).min(1024 * 1024);
        let read =
            rook_contain::files::read_text(&dir, Path::new(path.file_name().unwrap_or_default()), allowance);
        // A failed capped read may have consumed its entire allowance. Charge
        // it conservatively so malformed/oversized files cannot evade the
        // aggregate budget; successful small files pay only their actual bytes.
        *remaining = remaining.saturating_sub(read.as_ref().map_or(allowance, String::len));
        let text = read.map_err(|e| SkillError::io(&path, e))?;
        let (manifest, body) = manifest::parse(&text, &path)?;
        if !manifest.allowed_tools.is_empty() {
            tracing::warn!(skill = %manifest.name, "allowed-tools is informational metadata; Rook's approval policy controls tool access");
        }
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
                let text = rook_contain::files::read_text(&self.dir, &v.body, 1024 * 1024)
                    .map_err(|e| SkillError::io(&path, e))?;
                Ok((text, Some(v.clone())))
            }
        }
    }

    /// Files bundled with the skill (scripts, references, assets), relative to
    /// its directory. These are what version control and packaging operate on.
    pub fn resources(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for (visited, entry) in walkdir::WalkDir::new(&self.dir)
            .follow_links(false)
            .max_depth(32)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git")
            .enumerate()
        {
            if visited >= 4096 {
                return Err(SkillError::DiscoveryLimit {
                    path: self.dir.clone(),
                    limit: "4096 resource entries",
                });
            }
            let entry = entry.map_err(|e| SkillError::io(&self.dir, std::io::Error::other(e)))?;
            if entry.file_type().is_dir() && entry.depth() >= 32 {
                return Err(SkillError::DiscoveryLimit {
                    path: entry.into_path(),
                    limit: "32 resource directory levels",
                });
            }
            if !entry.file_type().is_file() {
                continue;
            }
            if let Ok(rel) = entry.path().strip_prefix(&self.dir) {
                if rel.as_os_str().len() > 4096 {
                    return Err(SkillError::DiscoveryLimit {
                        path: self.dir.clone(),
                        limit: "4096-byte resource paths",
                    });
                }
                out.push(rel.to_path_buf());
            }
        }
        out.sort();
        Ok(out)
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

impl Resolved {
    /// One resolved skill as a caller outside this crate sees it. `Skill` holds
    /// a parsed manifest and a directory handle, neither of which survives a
    /// wire; the CLI and the API each flattened it by hand and disagreed about
    /// what a variant was — a path in one, a string in the other.
    pub fn detail(&self) -> SkillDetail {
        SkillDetail {
            name: self.skill.manifest.name.clone(),
            version: self.skill.version().to_string(),
            source: self.skill.source.label().to_string(),
            dir: self.skill.dir.clone(),
            variant: self.variant.as_ref().map(|v| v.body.clone()),
            rejected: self.rejected.clone(),
            body: self.body.clone(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SkillDetail {
    pub name: String,
    pub version: String,
    pub source: String,
    pub dir: std::path::PathBuf,
    /// The body chosen for this environment, when a variant was.
    pub variant: Option<std::path::PathBuf>,
    /// Versions that were rejected, and why. Empty when the newest applied.
    pub rejected: Vec<(String, Vec<String>)>,
    pub body: String,
}

#[derive(Clone, Debug, Default)]
pub struct SkillIndex {
    skills: Vec<Skill>,
}

impl SkillIndex {
    /// Scan admitted roots in source-priority order, retaining equal-source
    /// root order. Errors/limit notices are collected instead of hiding failures.
    pub fn discover(roots: &[(PathBuf, SkillSource)]) -> (Self, Vec<SkillError>) {
        Self::discover_bounded(roots, DiscoveryLimits::default())
    }

    fn discover_bounded(
        roots: &[(PathBuf, SkillSource)],
        limits: DiscoveryLimits,
    ) -> (Self, Vec<SkillError>) {
        let mut skills = Vec::new();
        let mut errors = Vec::new();
        let mut remaining = limits.bytes;
        let mut visited = 0;
        // Preserve the existing precedence when truncation is necessary: the
        // workspace's deliberate overrides get room before a huge plugin or
        // builtin library. Equal-source root order is retained.
        let mut ordered: Vec<&(PathBuf, SkillSource)> = Vec::new();
        for root in roots {
            let position = ordered.partition_point(|(_, source)| source.rank() >= root.1.rank());
            if position < limits.roots {
                ordered.insert(position, root);
                ordered.truncate(limits.roots);
            }
        }
        if roots.len() > limits.roots {
            errors.push(SkillError::DiscoveryLimit {
                path: PathBuf::from("skills"),
                limit: "configured roots",
            });
        }
        'roots: for (root, source) in ordered {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    errors.push(SkillError::io(root, e));
                    continue;
                }
            };
            // One queue for the two supported layouts, never a recursive scan
            // of arbitrary project/dependency trees.
            let mut pending = vec![(entries, false)];
            while let Some((entries, version_level)) = pending.last_mut() {
                let Some(entry) = entries.next() else {
                    pending.pop();
                    continue;
                };
                let limit = if visited >= limits.entries {
                    Some("directory entries")
                } else if skills.len() >= limits.skills {
                    Some("loaded skills")
                } else if remaining == 0 {
                    Some("manifest bytes")
                } else if errors.len() >= limits.errors {
                    Some("diagnostics")
                } else {
                    None
                };
                if let Some(limit) = limit {
                    errors.push(SkillError::DiscoveryLimit { path: root.clone(), limit });
                    break 'roots;
                }
                visited += 1;
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        errors.push(SkillError::io(root, e));
                        continue;
                    }
                };
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if skill_file(&path).is_some() {
                    match Skill::load_budgeted(&path, source.clone(), &mut remaining) {
                        Ok(s) => skills.push(s),
                        Err(e) => errors.push(e),
                    }
                    continue;
                }
                // `<name>/<version>/SKILL.md` — several versions side by side.
                if !*version_level {
                    match std::fs::read_dir(&path) {
                        Ok(versions) => pending.push((versions, true)),
                        Err(e) => errors.push(SkillError::io(&path, e)),
                    }
                }
            }
        }
        (Self { skills }, errors)
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
    /// Skills whose name, description or keywords answer a query, best first.
    ///
    /// What a failed `resolve` offers instead of "not found": the catalog in the
    /// system prompt is capped, so a name the model has never seen may still be
    /// a skill that exists.
    pub fn search(&self, query: &str, env: &Environment, limit: usize) -> Vec<SkillCard> {
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .map(str::to_lowercase)
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut hits: Vec<(usize, SkillCard)> = self
            .catalog(env)
            .into_iter()
            .filter_map(|card| {
                let haystack =
                    format!("{} {} {}", card.name, card.description, card.keywords.join(" ")).to_lowercase();
                // A name match outranks a description match: someone asking for
                // "docker" wants the docker skill, not the six that mention it.
                let score: usize = terms
                    .iter()
                    .map(|t| match (card.name.to_lowercase().contains(t), haystack.contains(t)) {
                        (true, _) => 4,
                        (_, true) => 1,
                        _ => 0,
                    })
                    .sum();
                (score > 0).then_some((score, card))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        hits.into_iter().take(limit).map(|(_, card)| card).collect()
    }

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

struct DiscoveryLimits {
    roots: usize,
    entries: usize,
    skills: usize,
    bytes: usize,
    errors: usize,
}
impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self { roots: 64, entries: 16_384, skills: 512, bytes: 16 * 1024 * 1024, errors: 64 }
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

#[cfg(test)]
mod discovery_tests {
    use super::*;

    fn write(root: &Path, name: &str, body: &str) -> usize {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        let text = format!("---\nname: bounded\ndescription: Use when testing discovery.\n---\n{body}");
        std::fs::write(path.join("SKILL.md"), &text).unwrap();
        text.len()
    }

    fn hit(errors: &[SkillError], expected: &str) {
        assert!(
            errors.iter().any(|e| matches!(e, SkillError::DiscoveryLimit {limit, ..} if *limit == expected)),
            "{errors:?}"
        );
    }

    #[test]
    fn entry_skill_and_byte_limits_are_reached_before_loading_the_next_manifest() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = 0;
        for i in 0..5 {
            bytes = write(root.path(), &format!("v{i}"), "small body");
        }
        let roots = [(root.path().into(), SkillSource::User)];
        for (limits, kind) in [
            (DiscoveryLimits { entries: 2, ..Default::default() }, "directory entries"),
            (DiscoveryLimits { skills: 2, ..Default::default() }, "loaded skills"),
            (DiscoveryLimits { bytes: 2 * bytes, ..Default::default() }, "manifest bytes"),
        ] {
            assert!(5 > limits.entries || 5 > limits.skills || 5 * bytes > limits.bytes);
            let (index, errors) = SkillIndex::discover_bounded(&roots, limits);
            assert_eq!(index.len(), 2);
            hit(&errors, kind);
        }
    }

    #[test]
    fn version_directories_share_the_same_entry_budget() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..5 {
            write(root.path(), &format!("versions/{i}.0.0"), "version");
        }
        let (index, errors) = SkillIndex::discover_bounded(
            &[(root.path().into(), SkillSource::User)],
            DiscoveryLimits { entries: 3, ..Default::default() },
        );
        assert_eq!(index.len(), 2, "the parent directory also costs one visited entry");
        hit(&errors, "directory entries");
    }

    #[test]
    fn higher_priority_sources_get_room_before_a_large_low_priority_library() {
        let builtin = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        for i in 0..3 {
            write(builtin.path(), &format!("s{i}"), "builtin");
        }
        write(project.path(), "override", "project");
        let (index, errors) = SkillIndex::discover_bounded(
            &[(builtin.path().into(), SkillSource::Builtin), (project.path().into(), SkillSource::Project)],
            DiscoveryLimits { skills: 1, ..Default::default() },
        );
        assert_eq!(index.len(), 1);
        assert_eq!(index.all()[0].source, SkillSource::Project);
        hit(&errors, "loaded skills");
        let (index, errors) = SkillIndex::discover_bounded(
            &[(builtin.path().into(), SkillSource::Builtin), (project.path().into(), SkillSource::Project)],
            DiscoveryLimits { roots: 1, ..Default::default() },
        );
        assert_eq!(index.len(), 1);
        assert_eq!(index.all()[0].source, SkillSource::Project, "root admission must also respect priority");
        hit(&errors, "configured roots");
    }

    #[test]
    fn oversized_and_malformed_reads_cannot_evade_the_shared_byte_and_error_limits() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "oversized", &"x".repeat(1024 * 1024));
        let mut remaining = 1024 * 1024;
        assert!(
            Skill::load_budgeted(&root.path().join("oversized"), SkillSource::User, &mut remaining).is_err()
        );
        assert_eq!(remaining, 0);
        let broken = tempfile::tempdir().unwrap();
        for i in 0..5 {
            let path = broken.path().join(i.to_string());
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "missing frontmatter").unwrap();
        }
        let (index, errors) = SkillIndex::discover_bounded(
            &[(broken.path().into(), SkillSource::User)],
            DiscoveryLimits { errors: 2, ..Default::default() },
        );
        assert!(index.is_empty());
        assert_eq!(errors.len(), 3, "two failures and one explicit truncation diagnostic");
        hit(&errors, "diagnostics");
    }

    #[test]
    fn a_large_or_deep_resource_tree_is_reported_instead_of_silently_truncated() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "skill", "body");
        let dir = root.path().join("skill");
        let skill = Skill::load(&dir, SkillSource::User).unwrap();
        for i in 0..4097 {
            std::fs::write(dir.join(format!("file{i}")), "").unwrap();
        }
        assert!(matches!(
            skill.resources(),
            Err(SkillError::DiscoveryLimit { limit: "4096 resource entries", .. })
        ));
        let deep = tempfile::tempdir().unwrap();
        write(deep.path(), "skill", "body");
        let dir = deep.path().join("skill");
        let mut nested = dir.clone();
        for _ in 0..33 {
            nested.push("d");
        }
        std::fs::create_dir_all(&nested).unwrap();
        let skill = Skill::load(&dir, SkillSource::User).unwrap();
        assert!(matches!(
            skill.resources(),
            Err(SkillError::DiscoveryLimit { limit: "32 resource directory levels", .. })
        ));
    }
}
