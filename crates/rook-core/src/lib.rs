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
pub mod attachments;
pub mod branches;
pub mod calls;
pub mod catalog;
pub mod changes;
pub mod chat_submission;
mod completion;
pub mod config;
pub mod context;
pub mod delivery;
pub mod diagnostics;
pub mod docs;
pub mod error;
pub mod evaluation;
pub mod execution;
pub mod fileset;
pub mod hooks;
pub mod install;
pub mod instructions;
pub mod keybindings;
pub mod lsp;
pub mod mcp_auth;
pub mod mcp_connections;
pub mod mcp_server;
pub mod memory;
pub mod mention;
pub mod message_queue;
pub mod model_catalog;
pub mod model_route;
pub mod models;
mod output;
pub mod paths;
mod persistence;
mod phase_routing;
pub mod plugins;
mod recipes;
mod results;
pub mod schedules;
pub mod script;
pub mod search;
pub mod secrets;
pub mod service;
mod sources;
pub mod telemetry;
pub mod transcript;
pub mod turns;
pub mod upgrade;
pub mod work;
mod worktrees;

pub use config::{ApiEndpoint, Config, ConfigError, ModelSource};
pub use docs::{DocSet, Kept};
pub use error::{CoreError, Result};
pub use fileset::{CaptureLimits, Change, FileSet};
pub use mcp_connections::McpSession;
pub use memory::{Fact, MemoryBook, Scope};
pub use secrets::{Named, Source, Vault};
pub use service::{
    AGENT_VERSION, AuthoredSkill, ContextUsage, KindUsage, MaintenanceReport, MemoryVersion, Refreshed,
    Rewind, Rollback, Rook, SessionSummary, SkillCandidate, SkillVersionRecord, SkillWhy, TranscriptEntry,
    session_named,
};

mod provider_history;
mod tool_changes;
mod tool_details;
mod tool_images;
