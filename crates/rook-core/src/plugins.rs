//! Agent Plugins: one directory packaging skills and MCP servers together.
//!
//! The format is the ecosystem's rather than Rook's — a `plugin.json`, skills in
//! `skills/`, servers in `.mcp.json` — so a plugin written for another agent
//! works here unchanged, and a skill authored today packages without being
//! rewritten. [ADR-0003](../../../docs/adr/0003-agent-skills-format.md) is the
//! same argument for the skill format itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::paths;

pub struct Plugin {
    pub name: String,
    pub dir: PathBuf,
    pub description: String,
    pub version: String,
    pub mcp: Vec<rook_mcp::ServerConfig>,
}

impl Plugin {
    pub fn skills_dir(&self) -> PathBuf {
        self.dir.join("skills")
    }
}

/// Every plugin installed for the user or vendored into the workspace, and what
/// went wrong with the ones that could not be read.
pub fn discover(workspace: &Path) -> (Vec<Plugin>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for root in [paths::user_plugins_dir(), paths::project_plugins_dir(workspace)] {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten().filter(|e| e.path().is_dir()) {
            match load(&entry.path()) {
                Ok(Some(plugin)) => found.push(plugin),
                Ok(None) => {}
                Err(e) => errors.push(e),
            }
        }
    }
    (found, errors)
}

/// `None` for a directory that is not a plugin at all, which is not an error:
/// the plugins directory is the user's and may hold anything.
fn load(dir: &Path) -> std::result::Result<Option<Plugin>, String> {
    // `.claude-plugin/plugin.json` is where the spec puts it; the plain one is
    // accepted too, because that is what people write when hand-rolling.
    let manifest_path =
        [dir.join(".claude-plugin/plugin.json"), dir.join("plugin.json")].into_iter().find(|p| p.is_file());
    let Some(manifest_path) = manifest_path else { return Ok(None) };

    let text =
        std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let name = manifest.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| dir_name.to_string());

    let mut declared = manifest.mcp_servers;
    if let Ok(text) = std::fs::read_to_string(dir.join(".mcp.json")) {
        let sidecar: Sidecar =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", dir.join(".mcp.json").display()))?;
        declared.extend(sidecar.mcp_servers);
    }

    Ok(Some(Plugin {
        mcp: declared
            .into_iter()
            .map(|(server, config)| rook_mcp::ServerConfig {
                // The map key is the name; namespaced by plugin so two plugins
                // shipping a `github` server do not collide in the tool names
                // the model sees.
                name: format!("{name}__{server}"),
                cwd: config.cwd.or_else(|| Some(dir.display().to_string())),
                ..config
            })
            .collect(),
        name,
        dir: dir.to_path_buf(),
        description: manifest.description.unwrap_or_default(),
        version: manifest.version.unwrap_or_else(|| "0.0.0".into()),
    }))
}

#[derive(Default, Deserialize)]
struct Manifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, rook_mcp::ServerConfig>,
}

#[derive(Default, Deserialize)]
struct Sidecar {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, rook_mcp::ServerConfig>,
}
