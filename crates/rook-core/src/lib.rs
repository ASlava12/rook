//! The engine the CLI, the TUI and the web UI all sit on.
//!
//! Nothing user-facing lives here and nothing here is front-end specific: the
//! three interfaces are views over the same [`service::Rook`], which is what
//! keeps them from drifting into three subtly different products.

pub mod agent;
pub mod config;
pub mod context;
pub mod error;
pub mod fileset;
pub mod paths;
pub mod service;

pub use config::Config;
pub use error::{CoreError, Result};
pub use fileset::{CaptureLimits, Change, FileSet};
pub use service::{AGENT_VERSION, MaintenanceReport, Rollback, Rook, SkillVersionRecord, TranscriptEntry};
