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

pub mod acp_executor;
pub mod activity;
pub mod agent;
pub mod attachments;
pub mod branches;
pub mod browser;
pub mod calls;
pub mod catalog;
pub mod changes;
pub mod chat_submission;
mod completion;
pub mod composite_recipes;
pub mod config;
pub mod context;
mod correlation;
pub mod delivery;
pub mod diagnostics;
pub mod docs;
pub mod environments;
pub mod error;
pub mod evaluation;
pub mod execution;
pub mod experiments;
pub mod extension_ui;
pub mod fileset;
pub mod hooks;
pub mod inbound;
pub mod install;
pub mod instructions;
pub mod keybindings;
pub mod learning;
pub mod lsp;
pub mod mcp_auth;
pub mod mcp_connections;
pub mod mcp_server;
pub mod memory;
pub mod mention;
pub mod message_queue;
mod model_accounting;
mod model_attempt;
pub mod model_catalog;
mod model_delegation;
pub mod model_route;
pub mod models;
mod output;
pub mod paths;
mod persistence;
mod phase_routing;
pub mod plugins;
pub mod price_catalog;
mod recipes;
mod results;
pub mod review;
pub mod schedules;
pub mod script;
pub mod search;
pub mod secrets;
pub mod service;
pub mod session_list;
pub mod session_recall;
pub mod shell_undo;
mod sources;
pub mod telegram;
pub mod telemetry;
pub mod transcript;
pub mod turns;
pub mod upgrade;
pub mod work;
pub mod worktree_recovery;
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
