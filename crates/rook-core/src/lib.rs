//! The engine the CLI, the TUI and the web UI all sit on.
//!
//! Nothing user-facing lives here and nothing here is front-end specific: the
//! three interfaces are views over the same [`service::Rook`], which is what
//! keeps them from drifting into three subtly different products.

pub mod agent;
pub mod changes;
pub mod config;
pub mod context;
pub mod error;
pub mod fileset;
pub mod hooks;
pub mod lsp;
pub mod memory;
pub mod paths;
pub mod search;
pub mod service;
pub mod telemetry;

pub use config::{Config, ConfigError};
pub use error::{CoreError, Result};
pub use fileset::{CaptureLimits, Change, FileSet};
pub use memory::{Fact, MemoryBook, Scope};
pub use service::{
    AGENT_VERSION, AuthoredSkill, ContextUsage, KindUsage, MaintenanceReport, McpSession, MemoryVersion,
    Rewind, Rollback, Rook, SessionSummary, SkillVersionRecord, TranscriptEntry,
};
