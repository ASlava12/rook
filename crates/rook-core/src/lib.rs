//! The engine the CLI, the TUI and the web UI all sit on.
//!
//! Nothing user-facing lives here and nothing here is front-end specific: the
//! three interfaces are views over the same [`service::Rook`], which is what
//! keeps them from drifting into three subtly different products.

/// The shell `run_command` runs a line through.
///
/// One place, so the system prompt and the spawner cannot disagree about it —
/// and they are the two that must not.
#[cfg(windows)]
pub const SHELL: &str = "cmd.exe (`cmd /C`)";
#[cfg(not(windows))]
pub const SHELL: &str = "/bin/sh";

pub mod agent;
pub mod catalog;
pub mod changes;
pub mod config;
pub mod context;
pub mod error;
pub mod fileset;
pub mod hooks;
pub mod install;
pub mod instructions;
pub mod lsp;
pub mod mcp_server;
pub mod memory;
pub mod paths;
pub mod plugins;
pub mod script;
pub mod search;
pub mod service;
pub mod telemetry;

pub use config::{Config, ConfigError};
pub use error::{CoreError, Result};
pub use fileset::{CaptureLimits, Change, FileSet};
pub use memory::{Fact, MemoryBook, Scope};
pub use service::{
    AGENT_VERSION, AuthoredSkill, ContextUsage, KindUsage, MaintenanceReport, McpSession, MemoryVersion,
    Rewind, Rollback, Rook, SessionSummary, SkillVersionRecord, TranscriptEntry, session_named,
};
