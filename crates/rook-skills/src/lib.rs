//! Skills: portable, versioned, environment-aware units of agent capability.
//!
//! Rook reads the Agent Skills format (`SKILL.md` with YAML frontmatter), so
//! skills written for other agents work unchanged. On top of it, two problems
//! specific to a long-lived local agent are addressed:
//!
//! * **Environment drift.** A skill written against Rust 1.75, GNU `sed` or
//!   Docker 24 quietly breaks elsewhere. [`manifest::Requirements`] lets a skill
//!   state what it needs, and [`index::SkillIndex::resolve`] picks the newest
//!   version that actually applies — reporting why the others did not.
//! * **Platform forks.** Instead of `deploy-linux` and `deploy-windows` drifting
//!   apart, one skill carries [`manifest::Variant`] bodies selected by the same
//!   predicate.

pub mod env;
pub mod error;
pub mod index;
pub mod manifest;

pub use env::Environment;
pub use error::{Result, SkillError};
pub use index::{Resolved, Skill, SkillCard, SkillDetail, SkillIndex, SkillSource};
pub use manifest::{Mismatch, Requirements, SkillManifest, Variant, parse as parse_manifest, usable_name};
