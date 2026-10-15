//! Trusted execution profiles and per-workspace resource ownership.
mod container;
mod manage;
pub mod native;
mod ssh;
mod sync;
mod transport;
use crate::{Result, ToolContext, ToolError};

pub use manage::Request;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub selected: String,
    pub native_workspace: bool,
    pub profiles: BTreeMap<String, Profile>,
    pub max_resources: usize,
    pub setup_timeout_secs: u64,
    pub file_bytes: usize,
    pub total_bytes: usize,
    pub max_files: usize,
    pub max_entries: usize,
    pub max_calls: usize,
    pub exclude: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            selected: "local".into(),
            native_workspace: false,
            profiles: BTreeMap::new(),
            max_resources: 4,
            setup_timeout_secs: 30,
            file_bytes: 1024 * 1024,
            total_bytes: 16 * 1024 * 1024,
            max_files: 1024,
            max_entries: 4096,
            max_calls: 8,
            exclude: vec![".git".into(), "target".into(), "node_modules".into(), ".venv".into()],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Local,
    Container {
        image: String,
        #[serde(default = "docker")]
        program: String,
        #[serde(default = "memory")]
        memory_mb: usize,
        #[serde(default = "pids")]
        pids: usize,
    },
    Ssh {
        host: String,
        #[serde(default = "ssh")]
        program: String,
        remote_workspace: String,
    },
}
fn docker() -> String {
    "docker".into()
}
fn ssh() -> String {
    "ssh".into()
}
fn memory() -> usize {
    512
}
fn pids() -> usize {
    256
}
impl Settings {
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.native_workspace && !matches!(self.profiles.get(&self.selected), Some(Profile::Ssh { .. })) {
            errors.push("execution_environment.native_workspace requires a selected SSH profile".into());
        }
        if !(1..=16).contains(&self.max_resources) {
            errors.push("execution_environment.max_resources: expected 1..=16".into());
        }
        if !(1..=300).contains(&self.setup_timeout_secs) {
            errors.push("execution_environment.setup_timeout_secs: expected 1..=300".into());
        }
        for (name, value, low, high) in [
            ("file_bytes", self.file_bytes, 1024, 8 * 1024 * 1024),
            ("total_bytes", self.total_bytes, 1024, 64 * 1024 * 1024),
            ("max_files", self.max_files, 1, 8192),
            ("max_entries", self.max_entries, 1, 32768),
            ("max_calls", self.max_calls, 1, 32),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("execution_environment.{name}: expected {low}..={high}"));
            }
        }
        if self.max_entries < self.max_files {
            errors.push("execution_environment.max_entries must cover max_files".into());
        }
        if self.exclude.len() > 32
            || self.exclude.iter().any(|entry| {
                entry.is_empty()
                    || entry.len() > 128
                    || entry.contains(['/', '\\'])
                    || entry.chars().any(char::is_control)
            })
        {
            errors.push("execution_environment.exclude needs at most 32 simple entry names".into());
        }
        if self.profiles.len() > 16 || !name(&self.selected) || self.profiles.keys().any(|n| !name(n)) {
            errors.push("execution profiles need at most 16 names of 1..64 ASCII bytes".into());
        }
        if self.selected != "local" && !self.profiles.contains_key(&self.selected) {
            errors.push("execution_environment.selected must name a configured profile".into());
        }
        if self.profiles.get("local").is_some_and(|profile| !matches!(profile, Profile::Local)) {
            errors.push("execution_environment.profiles.local is reserved for local execution".into());
        }
        for (name, p) in &self.profiles {
            match p {
                Profile::Local => {}
                Profile::Container { image, program, memory_mb, pids } => {
                    if image.is_empty()
                        || image.len() > 256
                        || image.starts_with('-')
                        || image.chars().any(char::is_whitespace)
                        || image.chars().any(char::is_control)
                        || !program_ok(program)
                        || !(32..=65536).contains(memory_mb)
                        || !(16..=4096).contains(pids)
                    {
                        errors.push(format!(
                            "execution_environment.profiles.{name}: invalid image, launcher or resource limits"
                        ));
                    }
                }
                Profile::Ssh { host, program, remote_workspace } => {
                    if !program_ok(program)
                        || host.is_empty()
                        || host.len() > 256
                        || host.starts_with('-')
                        || host.chars().any(|c| c.is_control() || c.is_whitespace())
                        || !remote_workspace.starts_with('/')
                        || remote_workspace.len() > 1024
                        || remote_workspace.split('/').any(|part| matches!(part, "." | ".."))
                        || remote_workspace.chars().any(char::is_control)
                    {
                        errors.push(format!("execution_environment.profiles.{name}: SSH needs a trusted host alias and absolute remote workspace"));
                    }
                }
            }
        }
        errors
    }
    pub fn local(&self) -> bool {
        self.selected == "local" || matches!(self.profiles.get(&self.selected), Some(Profile::Local))
    }
}
fn name(v: &str) -> bool {
    !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn program_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= 1024 && !v.chars().any(char::is_control)
}
pub(crate) fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::Invalid { tool: "run_command".into(), message: message.into() }
}
pub(crate) fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub(crate) fn identity(value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|e| invalid(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
#[derive(Default)]
pub struct Pool {
    resources: Mutex<BTreeMap<String, Arc<Resource>>>,
}
impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionPool").finish_non_exhaustive()
    }
}
pub struct Context {
    pub settings: Settings,
    pub pool: Arc<Pool>,
    pub state_root: PathBuf,
}
impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionContext").field("selected", &self.settings.selected).finish_non_exhaustive()
    }
}
pub(crate) struct Prepared {
    pub command: String,
    pub lease: Arc<Lease>,
    pub input: Option<Arc<Vec<u8>>>,
    pub description: serde_json::Value,
}

enum Resource {
    Container(Arc<container::Resource>),
    Ssh(Arc<ssh::Resource>),
}
impl Resource {
    fn disposable(self: &Arc<Self>) -> bool {
        if Arc::strong_count(self) != 1 {
            return false;
        }
        match self.as_ref() {
            Self::Container(r) => r.disposable(),
            Self::Ssh(r) => r.disposable(),
        }
    }
}
pub(crate) enum Lease {
    Container(Arc<container::Lease>),
    Ssh(Arc<ssh::Lease>),
}
impl Lease {
    pub(crate) fn complete(&self, exit: i32) -> Result<()> {
        match self {
            Self::Container(r) => r.complete(exit),
            Self::Ssh(r) => r.complete(exit),
        }
    }
    pub(crate) fn stop(&self) -> bool {
        match self {
            Self::Container(r) => r.stop(),
            Self::Ssh(r) => r.stop(),
        }
    }
    pub(crate) fn uncertain_effects(&self) -> bool {
        match self {
            Self::Container(_) => false,
            Self::Ssh(r) => r.uncertain_effects(),
        }
    }
}
#[doc(hidden)]
pub struct Transport {
    pub(crate) input: Option<Arc<Vec<u8>>>,
    pub(crate) lease: Arc<Lease>,
    pub(crate) description: serde_json::Value,
    pub(crate) display: String,
}

impl Pool {
    fn resource(&self, ctx: &ToolContext, env: &Context, retain: bool) -> Result<Arc<Resource>> {
        if !cfg!(unix) {
            return Err(invalid(
                "managed execution currently requires a Unix client; local execution remains available",
            ));
        }
        if !env.settings.errors().is_empty() {
            return Err(invalid("invalid execution configuration; run rook config check"));
        }
        let profile = env
            .settings
            .profiles
            .get(&env.settings.selected)
            .ok_or_else(|| invalid("execution profile missing"))?;
        std::fs::create_dir_all(&env.state_root).map_err(|e| invalid(e.to_string()))?;
        let root = ctx.workspace.canonicalize().map_err(|e| invalid(e.to_string()))?;
        let id = if matches!(profile, Profile::Ssh { .. }) {
            identity(&(
                profile,
                &root,
                &env.state_root,
                ctx.isolation.network,
                env.settings.file_bytes,
                env.settings.total_bytes,
                env.settings.max_files,
                env.settings.max_entries,
                env.settings.max_calls,
                &env.settings.exclude,
            ))?
        } else {
            identity(&(profile, &root, &env.state_root, ctx.isolation.network))?
        };
        let resource = {
            let mut resources = self.resources.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(resource) = resources.get(&id) {
                resource.clone()
            } else {
                resources.retain(|_, resource| !resource.disposable());
                if retain && resources.len() >= env.settings.max_resources {
                    return Err(invalid(
                        "execution_environment.max_resources reached; stop or release an existing environment",
                    ));
                }
                let resource = Arc::new(match profile {
                    Profile::Container { image, program, memory_mb, pids } => {
                        Resource::Container(Arc::new(container::Resource::new(
                            program.clone(),
                            image.clone(),
                            root,
                            env.state_root.clone(),
                            id.clone(),
                            *memory_mb,
                            *pids,
                            ctx.isolation.network,
                            env.settings.setup_timeout_secs,
                        )))
                    }
                    Profile::Ssh { host, program, remote_workspace } => {
                        Resource::Ssh(Arc::new(ssh::Resource::new(
                            host.clone(),
                            program.clone(),
                            remote_workspace.clone(),
                            root,
                            env.state_root.clone(),
                            id.clone(),
                            env.settings.clone(),
                        )?))
                    }
                    Profile::Local => return Err(invalid("local execution has no managed resource")),
                });
                if retain {
                    resources.insert(id.clone(), resource.clone());
                }
                resource
            }
        };

        Ok(resource)
    }
    pub async fn request(
        &self,
        ctx: &ToolContext,
        env: &Context,
        request: Request,
    ) -> Result<serde_json::Value> {
        if env.settings.native_workspace {
            return match request {
                request @ (Request::NativeInspect { .. } | Request::NativeUndo { .. }) => {
                    native::manage(ctx, request).await
                }
                Request::Inspect => {
                    let Some(Profile::Ssh { host, remote_workspace, .. }) =
                        env.settings.profiles.get(&env.settings.selected)
                    else {
                        return Err(invalid("native workspace requires a selected SSH profile"));
                    };
                    Ok(
                        serde_json::json!({"profile":env.settings.selected,"kind":"ssh_native","workspace":remote_workspace,"host":host,"state":"configured","notice":"direct workspace; use ssh_workspace checkpoint/undo in its owning session, and session stop for an active call"}),
                    )
                }
                _ => Err(invalid(
                    "replica recovery/release cannot manage an SSH-native workspace; use its owning session's native receipts and stop control",
                )),
            };
        }
        if matches!(request, Request::NativeInspect { .. } | Request::NativeUndo { .. }) {
            return Err(invalid("native receipt controls require the original selected SSH-native profile"));
        }
        if env.settings.local() {
            return match request {
                Request::Inspect => Ok(
                    serde_json::json!({"profile":env.settings.selected,"kind":"local","state":"local","workspace":ctx.workspace}),
                ),
                _ => Err(invalid("local execution has no managed resource to release")),
            };
        }
        let resource = self.resource(ctx, env, matches!(request, Request::Recover { .. }))?;
        let (mut result, kind) = match resource.as_ref() {
            Resource::Container(r) => (r.manage(request).await?, "container"),
            Resource::Ssh(r) => (r.manage(request).await?, "ssh"),
        };
        result["profile"] = serde_json::json!(env.settings.selected);
        result["kind"] = serde_json::json!(kind);
        Ok(result)
    }

    pub(crate) async fn prepare(
        &self,
        ctx: &ToolContext,
        env: &Context,
        command: &str,
        cwd: &Path,
        secret_values: &[(String, String)],
    ) -> Result<Prepared> {
        if !env.settings.errors().is_empty() {
            return Err(invalid("invalid execution configuration; run rook config check"));
        }
        let profile = env
            .settings
            .profiles
            .get(&env.settings.selected)
            .ok_or_else(|| invalid("execution profile missing"))?;
        match profile {
            Profile::Local => Err(invalid("local execution does not need a remote resource")),
            Profile::Container { .. } => {
                let root = ctx.workspace.canonicalize().map_err(|e| invalid(e.to_string()))?;
                if root.to_string_lossy().contains(',')
                    || root.to_string_lossy().chars().any(char::is_control)
                {
                    return Err(invalid("Docker bind paths cannot contain commas or control characters"));
                }
                let state_root = crate::through_symlinks(&crate::normalize(&env.state_root));
                if state_root.starts_with(&root) || root.starts_with(&state_root) {
                    return Err(invalid("execution state must stay outside the bound workspace"));
                }
                for path in &ctx.isolation.unreadable {
                    if path.canonicalize().ok().is_some_and(|p| p.starts_with(&root) || root.starts_with(&p))
                    {
                        return Err(invalid(
                            "container bind would expose an unreadable state path; use a workspace outside the agent store",
                        ));
                    }
                }
                let real_cwd = cwd.canonicalize().map_err(|e| invalid(e.to_string()))?;
                let relative = real_cwd
                    .strip_prefix(&root)
                    .map_err(|_| invalid("container cwd must stay within the bound workspace"))?;
                if relative.components().any(|p| !matches!(p, std::path::Component::Normal(_))) {
                    return Err(invalid("container cwd has an invalid relative component"));
                }
                let resource = self.resource(ctx, env, true)?;
                let Resource::Container(resource) = resource.as_ref() else {
                    return Err(invalid("configured resource kind changed"));
                };
                let was_uncertain = resource.uncertain.load(std::sync::atomic::Ordering::Acquire);
                let lease = match resource.acquire().await {
                    Ok(lease) => lease,
                    Err(error)
                        if !was_uncertain
                            && resource.uncertain.load(std::sync::atomic::Ordering::Acquire) =>
                    {
                        return Err(ToolError::ExecutionUnknown(error.to_string()));
                    }
                    Err(error) => return Err(error),
                };
                let remote = Path::new("/workspace").join(relative).to_string_lossy().into_owned();
                let command = container::command(
                    resource,
                    lease.token(),
                    &remote,
                    command,
                    &secret_values.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>(),
                );
                Ok(Prepared {
                    command,
                    lease: Arc::new(Lease::Container(lease)),
                    input: None,
                    description: serde_json::json!({"profile":env.settings.selected,"kind":"container","resource":resource.container(),"workspace":"/workspace","cwd":remote,"image":resource.image}),
                })
            }
            Profile::Ssh { .. } => {
                if ctx.isolate == crate::isolate::Mode::Required || !ctx.isolation.network {
                    return Err(invalid(
                        "SSH needs an allowed control connection and currently cannot enforce mandatory remote OS containment; choose a container or adjust trusted configuration",
                    ));
                }
                let root = ctx.workspace.canonicalize().map_err(|e| invalid(e.to_string()))?;
                let state = crate::through_symlinks(&crate::normalize(&env.state_root));
                if state.starts_with(&root) || root.starts_with(&state) {
                    return Err(invalid("execution state must stay outside the mirrored workspace"));
                }
                for path in &ctx.isolation.unreadable {
                    let path = crate::through_symlinks(&crate::normalize(path));
                    if path.starts_with(&root) || root.starts_with(&path) {
                        return Err(invalid("SSH transfer would expose an unreadable state path"));
                    }
                }
                let cwd = cwd.canonicalize().map_err(|e| invalid(e.to_string()))?;
                let relative = cwd
                    .strip_prefix(&root)
                    .map_err(|_| invalid("SSH cwd must stay within the mirrored workspace"))?;
                let resource = self.resource(ctx, env, true)?;
                let Resource::Ssh(resource) = resource.as_ref() else {
                    return Err(invalid("configured resource kind changed"));
                };
                resource.prepare(&env.settings.selected, command, relative, secret_values).await
            }
        }
    }
}
