//! Editor declarations are admitted before project discovery or process startup.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rook_core::Rook;
use serde::Deserialize;
use serde_json::Value;

use crate::protocol::Error;

pub(super) struct Prepared {
    pub workspace: PathBuf,
    pub declarations: Vec<Value>,
    pub names: HashSet<String>,
}

#[derive(Deserialize)]
struct Pair {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct Stdio {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<Pair>,
}

#[derive(Deserialize)]
struct Http {
    name: String,
    url: String,
    #[serde(default)]
    headers: Vec<Pair>,
}

fn pairs(values: Vec<Pair>) -> Result<HashMap<String, String>, Error> {
    let mut result = HashMap::new();
    for value in values {
        if value.name.is_empty()
            || value.name.len() > 256
            || value.value.len() > 16384
            || result.insert(value.name, value.value).is_some()
        {
            return Err(Error::invalid_params(
                "MCP environment/header names must be unique and bounded (256-byte names, 16384-byte values)",
            ));
        }
    }
    Ok(result)
}

pub(super) fn prepare(rook: &Rook, session: Option<u128>, params: &Value) -> Result<Prepared, Error> {
    if rook_core::delivery::encoded_size(params, rook.config.server.acp_setup_bytes).is_none() {
        return Err(Error::invalid_params(
            "ACP setup exceeds server.acp_setup_bytes; reduce declarations or raise that explicit limit",
        ));
    }
    if params
        .get("additionalDirectories")
        .is_some_and(|value| value.as_array().is_none_or(|roots| !roots.is_empty()))
    {
        return Err(Error::invalid_params(
            "additional workspace roots are not advertised by this ACP server",
        ));
    }
    let stored = session
        .map(|session| {
            rook.store
                .get_session_limited(session, 64 * 1024)
                .map_err(|_| Error::internal("cannot read the saved session workspace"))?
                .ok_or_else(|| Error::invalid_params("no such session"))
        })
        .transpose()?;
    let requested = match params.get("cwd") {
        Some(Value::String(cwd)) if cwd.len() <= 4096 => {
            // The legacy spelling "." names the server project, not process cwd.
            if cwd == "." {
                rook.workspace.clone()
            } else if Path::new(cwd).is_absolute() {
                PathBuf::from(cwd)
            } else {
                return Err(Error::invalid_params(
                    "cwd must be an absolute existing directory (legacy '.' names the server workspace)",
                ));
            }
        }
        None if stored.is_some() => {
            PathBuf::from(stored.as_ref().map(|m| m.workspace.as_str()).unwrap_or_default())
        }
        _ => return Err(Error::invalid_params("cwd must be a directory path of at most 4096 bytes")),
    };
    let workspace = requested
        .canonicalize()
        .map_err(|_| Error::invalid_params("cwd is not an accessible existing directory"))?;
    if !workspace.is_dir() {
        return Err(Error::invalid_params("cwd must name a directory"));
    }
    if let Some(stored) = stored {
        let previous = Path::new(&stored.workspace).canonicalize().map_err(|_| {
            Error::invalid_params(
                "saved workspace is unavailable; restore/move it explicitly before ACP resume",
            )
        })?;
        if previous != workspace {
            return Err(Error::invalid_params("cwd does not match the session's saved workspace"));
        }
    }
    let declarations = match params.get("mcpServers") {
        None => Vec::new(),
        Some(Value::Array(values)) if values.len() <= rook.config.mcp_connections.max_servers => {
            values.clone()
        }
        _ => {
            return Err(Error::invalid_params(
                "mcpServers must be an array within mcp_connections.max_servers",
            ));
        }
    };
    let mut names = HashSet::new();
    for declaration in &declarations {
        let name = declaration["name"]
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 64)
            .ok_or_else(|| Error::invalid_params("MCP names must contain 1..=64 bytes"))?;
        if !names.insert(name.to_owned()) {
            return Err(Error::invalid_params("MCP names must be unique within the session"));
        }
        if !matches!(declaration.get("type"), None | Some(Value::String(_))) {
            return Err(Error::invalid_params("invalid MCP transport type"));
        }
        if let Some(kind) = declaration["type"].as_str()
            && kind != "http"
        {
            return Err(Error::invalid_params(
                "only stdio and streamable HTTP MCP are supported; legacy SSE is not advertised",
            ));
        }
    }
    Ok(Prepared { workspace, declarations, names })
}

impl Prepared {
    pub(super) fn build(&self, base: &Rook) -> Result<Arc<Rook>, Error> {
        let mut rook = base.for_workspace(self.workspace.clone());
        let mut client = Vec::new();
        for declaration in &self.declarations {
            let mut server = rook_mcp::ServerConfig {
                cwd: Some(self.workspace.display().to_string()),
                ..Default::default()
            };
            if declaration["type"] == "http" {
                let spec: Http = serde_json::from_value(declaration.clone()).map_err(|_| {
                    Error::invalid_params(
                        "invalid HTTP MCP declaration; expected name, url and bounded header pairs",
                    )
                })?;
                if spec.url.len() > 4096 {
                    return Err(Error::invalid_params("MCP URL exceeds 4096 bytes"));
                }
                server.name = spec.name;
                server.url = Some(spec.url);
                server.headers = pairs(spec.headers)?;
            } else {
                let spec: Stdio = serde_json::from_value(declaration.clone()).map_err(|_| {
                    Error::invalid_params(
                        "invalid stdio MCP declaration; expected name, command, args and environment pairs",
                    )
                })?;
                if spec.command.is_empty()
                    || spec.command.len() > 4096
                    || spec.args.len() > 128
                    || spec.args.iter().any(|arg| arg.len() > 16384)
                {
                    return Err(Error::invalid_params("MCP command/arguments exceed their explicit bounds"));
                }
                server.name = spec.name;
                server.command = spec.command;
                server.args = spec.args;
                server.env = pairs(spec.env)?;
            }
            client.push(server);
        }
        // A client declaration cannot silently replace an operator/plugin server.
        for server in &client {
            if rook.mcp_servers().any(|configured| configured.name == server.name) {
                return Err(Error::invalid_params(
                    "client MCP name conflicts with a configured server; choose a distinct name",
                ));
            }
        }
        rook.config.mcp.extend(client);
        if rook.mcp_servers().count() > rook.config.mcp_connections.max_servers {
            return Err(Error::invalid_params(
                "combined MCP declarations exceed mcp_connections.max_servers",
            ));
        }
        Ok(Arc::new(rook))
    }
}

/// Only explicitly credential-shaped fields are hidden; ordinary environment
/// values remain useful task data. The setup envelope already bounds this list.
pub(super) fn secrets(rook: &Rook) -> Vec<String> {
    fn credential(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        [
            "authorization",
            "token",
            "secret",
            "password",
            "credential",
            "cookie",
            "api_key",
            "api-key",
            "access_key",
        ]
        .iter()
        .any(|word| name.contains(word))
    }
    let mut values = Vec::new();
    for server in rook.mcp_servers() {
        for (name, value) in server.env.iter().chain(&server.headers) {
            if credential(name) && value.len() >= 6 {
                values.push(value.clone());
                if name.eq_ignore_ascii_case("authorization")
                    && let Some((scheme, token)) = value.split_once(' ')
                    && scheme.eq_ignore_ascii_case("bearer")
                    && token.len() >= 6
                {
                    values.push(token.to_owned());
                }
            }
        }
    }
    values
}
