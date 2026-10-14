//! Source-addressed experiment trees with a fixed, explicitly run contract.
use crate::{
    CoreError, Result, Rook,
    shell_undo::{self, File, Snapshot},
};
use rook_contain::files::{RecoveryFiles, file_mode};
use rook_store::{Kind, ObjectId, SessionMeta};
use rook_tools::{
    Tool,
    policy::{Decision, Policy, Risk},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

static CHANGES: std::sync::Mutex<()> = std::sync::Mutex::new(());
static RUNS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
const CONTROL: &str = ".rook-experiment";
const METRICS: &str = ".rook-experiment/metrics.json";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_nodes: usize,
    pub max_runs: usize,
    pub state_bytes: usize,
    pub source_files: usize,
    pub source_entries: usize,
    pub file_bytes: usize,
    pub total_bytes: usize,
    pub artifact_files: usize,
    pub output_bytes: usize,
    pub timeout_secs: u64,
    pub exclude: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_nodes: 32,
            max_runs: 8,
            state_bytes: 4 * 1024 * 1024,
            source_files: 1024,
            source_entries: 4096,
            file_bytes: 8 * 1024 * 1024,
            total_bytes: 32 * 1024 * 1024,
            artifact_files: 32,
            output_bytes: 65536,
            timeout_secs: 120,
            exclude: vec![".git".into(), "target".into(), "node_modules".into(), ".venv".into()],
        }
    }
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, value, low, high) in [
            ("max_nodes", self.max_nodes, 1, 64),
            ("max_runs", self.max_runs, 1, 16),
            ("state_bytes", self.state_bytes, 65536, 8 * 1024 * 1024),
            ("artifact_files", self.artifact_files, 0, 64),
            ("output_bytes", self.output_bytes, 1024, 1024 * 1024),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("experiments.{name}: expected {low}..={high}"));
            }
        }
        if !(1..=3600).contains(&self.timeout_secs) {
            errors.push("experiments.timeout_secs: expected 1..=3600".into());
        }
        errors.extend(
            self.capture().errors().into_iter().map(|e| e.replace("shell_undo.", "experiments capture ")),
        );
        if self.exclude.len() > 31 {
            errors.push(
                "experiments.exclude: at most 31 names; the control directory is always excluded".into(),
            );
        }
        errors
    }
    fn capture(&self) -> shell_undo::Settings {
        let mut exclude = self.exclude.clone();
        if !exclude.iter().any(|n| n == CONTROL) {
            exclude.push(CONTROL.into());
        }
        shell_undo::Settings {
            max_files: self.source_files,
            max_entries: self.source_entries,
            file_bytes: self.file_bytes,
            total_bytes: self.total_bytes,
            state_bytes: self.state_bytes,
            exclude,
            ..Default::default()
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Minimize,
    Maximize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub command: String,
    pub metrics: BTreeMap<String, Direction>,
    #[serde(default)]
    pub artifacts: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Run {
    id: String,
    state: String,
    source: String,
    snapshot: Snapshot,
    contract: String,
    parameters: String,
    environment: String,
    workspace: String,
    session: String,
    started_at: i64,
    took_ms: u64,
    exit_code: Option<i64>,
    metrics: Option<BTreeMap<String, f64>>,
    metrics_source: Option<File>,
    artifacts: BTreeMap<String, File>,
    output: Option<File>,
    error: Option<String>,
    #[serde(default)]
    inspection: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Node {
    id: String,
    parent: Option<String>,
    session: String,
    title: String,
    hypothesis: String,
    parameters: BTreeMap<String, String>,
    source: Snapshot,
    workspace: String,
    state: String,
    error: Option<String>,
    answered: bool,
    revisions: usize,
    runs: Vec<Run>,
}
struct Observation {
    metrics: BTreeMap<String, f64>,
    metrics_source: File,
    artifacts: BTreeMap<String, File>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Project {
    version: u32,
    workspace: String,
    contract: Contract,
    contract_id: String,
    environment: String,
    baseline: String,
    winner: Option<String>,
    nodes: Vec<Node>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    Create {
        title: String,
        hypothesis: String,
        contract: Contract,
        #[serde(default)]
        parameters: BTreeMap<String, String>,
    },
    Branch {
        parent: String,
        title: String,
        hypothesis: String,
        #[serde(default)]
        parameters: Option<BTreeMap<String, String>>,
    },
    Inspect {
        node: String,
    },
    Capture {
        node: String,
    },
    Prepare {
        node: String,
    },
    Run {
        node: String,
        token: String,
        #[serde(default = "wait_by_default")]
        wait: bool,
    },
    Compare {
        #[serde(default)]
        baseline: Option<String>,
    },
    Promote {
        node: String,
        token: String,
    },
    Read {
        node: String,
        run: String,
        #[serde(default)]
        artifact: Option<String>,
        #[serde(default)]
        offset: usize,
    },
    Recover {
        node: String,
        run: String,
        note: String,
    },
    Stop {
        node: String,
        run: String,
    },
}
fn bad(s: impl Into<String>) -> CoreError {
    CoreError::Other(s.into())
}
fn key(owner: u128) -> String {
    format!("experiments/{owner:032x}")
}
fn new_id() -> String {
    rook_store::format_session_id(rook_store::new_session_id())
}
fn valid_path(s: &str) -> bool {
    !s.is_empty()
        && s.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && s.len() <= 1024
        && !s.contains('\\')
        && !s.chars().any(char::is_control)
        && Path::new(s).components().all(|p| matches!(p, std::path::Component::Normal(_)))
}
fn parameters(values: &BTreeMap<String, String>) -> Result<()> {
    if values.len() > 32
        || values.iter().any(|(k, v)| {
            k.is_empty()
                || k.len() > 64
                || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || v.len() > 16384
        })
    {
        return Err(bad(
            "experiment parameters require at most 32 names of 1..64 ASCII bytes and values up to 16 KiB",
        ));
    }
    crate::persistence::encode_with_limit(values, 32768)?;
    Ok(())
}
fn text(title: &str, hypothesis: &str) -> Result<()> {
    if title.trim().is_empty() || title.len() > 128 || hypothesis.trim().is_empty() || hypothesis.len() > 4096
    {
        Err(bad("experiment needs a title of 1..128 bytes and hypothesis of 1..4096 bytes"))
    } else {
        Ok(())
    }
}
fn contract(c: &Contract, s: &Settings) -> Result<()> {
    if c.command.trim().is_empty()
        || c.command.len() > 8192
        || c.metrics.is_empty()
        || c.metrics.len() > 32
        || c.metrics.keys().any(|k| {
            k.is_empty() || k.len() > 64 || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        || c.artifacts.len() > s.artifact_files
        || c.artifacts.iter().any(|p| !valid_path(p) || !p.starts_with(".rook-experiment/artifacts/"))
    {
        return Err(bad(
            "invalid experiment contract: command 1..8192 bytes, 1..32 named metrics and bounded artifact paths under .rook-experiment/artifacts/",
        ));
    }
    if c.artifacts.iter().collect::<BTreeSet<_>>().len() != c.artifacts.len() {
        return Err(bad("duplicate artifact path"));
    }
    Ok(())
}
fn environment() -> Result<String> {
    let mut values = BTreeMap::new();
    for name in ["PATH", "LANG", "LC_ALL", "PYTHONHASHSEED", "CUDA_VISIBLE_DEVICES", "OMP_NUM_THREADS"] {
        let value = std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
        if value.as_ref().is_some_and(|v| v.len() > 16384) {
            return Err(bad("experiment environment observation exceeds 16 KiB per selected variable"));
        }
        values.insert(name, value.as_ref().map(|v| ObjectId::of(v.as_bytes()).to_hex()));
    }
    shell_undo::hash(&(std::env::consts::OS, std::env::consts::ARCH, crate::SHELL, values), 65536)
}
fn validate(p: &Project, s: &Settings) -> Result<()> {
    if p.version != 1 || p.nodes.is_empty() || p.nodes.len() > s.max_nodes || p.workspace.len() > 4096 {
        return Err(bad("unsupported or oversized experiment project"));
    }
    contract(&p.contract, s)?;
    if ObjectId::from_hex(&p.environment).is_none() {
        return Err(bad("invalid experiment environment identity"));
    }
    if p.contract_id != shell_undo::hash(&p.contract, s.state_bytes)? {
        return Err(bad("experiment contract identity mismatch"));
    }
    let mut seen = BTreeSet::new();
    for n in &p.nodes {
        if n.answered != n.runs.iter().any(|r| r.state == "answered") {
            return Err(bad("experiment answer state disagrees with measured receipts"));
        }
        text(&n.title, &n.hypothesis)?;
        parameters(&n.parameters)?;
        if !seen.insert(n.id.clone())
            || rook_store::parse_session_id(&n.id).is_none()
            || rook_store::parse_session_id(&n.session).is_none()
            || n.parent.as_ref().is_some_and(|id| !seen.contains(id) || id == &n.id)
            || n.workspace.len() > 4096
            || n.runs.len() > s.max_runs
            || n.revisions > 8
            || !matches!(n.state.as_str(), "preparing" | "ready" | "failed")
            || n.error.as_ref().is_some_and(|v| v.len() > 4096)
        {
            return Err(bad("invalid experiment lineage or node limits"));
        }
        let snaps = std::iter::once(&n.source).chain(n.runs.iter().map(|r| &r.snapshot));
        for r in &n.runs {
            if rook_store::parse_session_id(&r.id).is_none()
                || rook_store::parse_session_id(&r.session).is_none()
                || !matches!(r.state.as_str(), "started" | "answered" | "failed" | "interrupted" | "reviewed")
                || r.artifacts.len() > s.artifact_files
                || r.error.as_ref().is_some_and(|v| v.len() > 4096)
                || r.inspection.as_ref().is_some_and(|v| v.len() > 4096)
                || r.workspace.len() > 4096
            {
                return Err(bad("invalid experiment run receipt"));
            }
            if r.contract != p.contract_id
                || r.source != r.snapshot.source
                || r.environment != p.environment
                || ObjectId::from_hex(&r.parameters).is_none()
            {
                return Err(bad("experiment run changed its inherited contract"));
            }
            let mut total = 0usize;
            for (path, f) in &r.artifacts {
                total = total.saturating_add(f.bytes);
                if !p.contract.artifacts.contains(path)
                    || ObjectId::from_hex(&f.object).is_none()
                    || f.bytes > s.file_bytes
                    || f.mode > 0o777
                    || total > s.total_bytes
                {
                    return Err(bad("invalid experiment artifact budget"));
                }
            }
            if r.output
                .as_ref()
                .is_some_and(|f| f.bytes > s.output_bytes || ObjectId::from_hex(&f.object).is_none())
            {
                return Err(bad("experiment output exceeds its bound"));
            }
            if r.metrics.as_ref().is_some_and(|m| {
                m.len() != p.contract.metrics.len()
                    || m.iter().any(|(k, v)| !p.contract.metrics.contains_key(k) || !v.is_finite())
            }) {
                return Err(bad("invalid experiment metrics"));
            }
            if r.metrics_source
                .as_ref()
                .is_some_and(|f| f.bytes > 65536 || ObjectId::from_hex(&f.object).is_none())
                || (r.state == "answered"
                    && (r.metrics.is_none()
                        || r.metrics_source.is_none()
                        || r.artifacts.len() != p.contract.artifacts.len()))
            {
                return Err(bad("answered experiment lacks verified metric evidence"));
            }
        }
        for snapshot in snaps {
            let mut total = 0usize;
            if snapshot.files.len() > s.source_files
                || snapshot.source != shell_undo::hash(&snapshot.files, s.state_bytes)?
            {
                return Err(bad("experiment snapshot identity or count mismatch"));
            }
            for (path, f) in &snapshot.files {
                total = total.saturating_add(f.bytes);
                if !valid_path(path)
                    || ObjectId::from_hex(&f.object).is_none()
                    || f.bytes > s.file_bytes
                    || f.mode > 0o777
                    || total > s.total_bytes
                {
                    return Err(bad("experiment source exceeds its file budget"));
                }
            }
        }
    }
    if !seen.contains(&p.baseline) || p.winner.as_ref().is_some_and(|id| !seen.contains(id)) {
        return Err(bad("experiment baseline or winner missing"));
    }
    if node(p, &p.baseline)?.parent.is_some()
        || p.nodes.iter().filter(|n| n.parent.is_none()).count() != 1
        || p.winner.as_ref().is_some_and(|id| node(p, id).is_ok_and(|n| !n.answered))
    {
        return Err(bad("invalid experiment baseline or unmeasured winner"));
    }
    Ok(())
}
fn load(rook: &Rook, owner: u128) -> Result<Option<Project>> {
    let s = &rook.config.experiments;
    if !s.errors().is_empty() {
        return Err(bad("invalid experiments configuration; run rook config check"));
    }
    let Some(bytes) = rook.store.kv_get_limited(&key(owner), s.state_bytes)? else { return Ok(None) };
    let p: Project = serde_json::from_slice(&bytes)?;
    validate(&p, s)?;
    for n in &p.nodes {
        if Path::new(&n.workspace) != directory(rook, owner, &n.id, None) {
            return Err(bad("experiment candidate workspace identity mismatch"));
        }
        for r in &n.runs {
            if Path::new(&r.workspace) != directory(rook, owner, &n.id, Some(&r.id)) {
                return Err(bad("experiment run workspace identity mismatch"));
            }
        }
    }
    Ok(Some(p))
}
fn save(rook: &Rook, owner: u128, p: &Project) -> Result<()> {
    validate(p, &rook.config.experiments)?;
    let bytes = crate::persistence::encode_with_limit(p, rook.config.experiments.state_bytes)?;
    rook.store
        .kv_update_session(owner, &key(owner), rook.config.experiments.state_bytes, None, |_| Ok(bytes))?;
    Ok(())
}
// Admit every active run's largest terminal receipt before any command starts.
// Concurrent completion must not consume space already promised to another run.
fn admit(rook: &Rook, p: &Project) -> Result<()> {
    validate(p, &rook.config.experiments)?;
    let mut reserved = p.clone();
    for n in &mut reserved.nodes {
        if n.state == "preparing" {
            n.error = Some("\0".repeat(4096));
        }
        for r in n.runs.iter_mut().filter(|r| r.state == "started") {
            let f = File { object: "f".repeat(64), bytes: usize::MAX, mode: 0o777 };
            r.metrics = Some(p.contract.metrics.keys().map(|k| (k.clone(), -f64::MAX)).collect());
            r.metrics_source = Some(f.clone());
            r.output = Some(f.clone());
            r.artifacts = p.contract.artifacts.iter().map(|path| (path.clone(), f.clone())).collect();
            r.error = Some("\0".repeat(4096));
            r.inspection = Some("\0".repeat(4096));
            r.took_ms = u64::MAX;
            r.exit_code = Some(i64::MIN);
        }
    }
    crate::persistence::encode_with_limit(&reserved, rook.config.experiments.state_bytes)?;
    Ok(())
}
fn node<'a>(p: &'a Project, id: &str) -> Result<&'a Node> {
    p.nodes.iter().find(|n| n.id == id).ok_or_else(|| bad("experiment node does not belong to this project"))
}
fn node_mut<'a>(p: &'a mut Project, id: &str) -> Result<&'a mut Node> {
    p.nodes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| bad("experiment node does not belong to this project"))
}
fn show(rook: &Rook, owner: u128, n: &Node) -> Value {
    let held = OWNERS.lock().unwrap_or_else(|e| e.into_inner());
    let runs: Vec<_> = n
        .runs
        .iter()
        .map(|r| {
            let mut value = json!(r);
            let live = r.state == "started"
                && held.contains_key(&(rook.store.root().to_path_buf(), owner, r.id.clone()));
            value["live"] = json!(live);
            value["uncertain"] = json!(r.state == "interrupted" || (r.state == "started" && !live));
            value
        })
        .collect();
    json!({"id":n.id,"parent":n.parent,"session":n.session,"title":n.title,"hypothesis":n.hypothesis,"parameters":n.parameters,"source":n.source.source,"workspace":n.workspace,"state":n.state,"error":n.error,"answered":n.answered,"revisions":n.revisions,"runs":runs})
}
fn mutable(n: &Node) -> Result<()> {
    if n.state != "ready" {
        return Err(bad(
            "candidate publication failed or was interrupted; inspect the saved source and create a fresh project",
        ));
    }
    if n.answered {
        return Err(bad("this node's answered source is frozen; branch a child for another hypothesis"));
    }
    if n.runs.iter().any(|r| r.state == "started" || r.state == "interrupted") {
        return Err(bad(
            "a prior run is active or uncertain; inspect it and record recovery before changing or rerunning",
        ));
    }
    Ok(())
}
fn token(owner: u128, p: &Project, n: &Node, s: &Settings) -> Result<String> {
    shell_undo::hash(
        &(owner, &p.contract_id, &p.environment, &n.id, &n.source.source, &n.parameters, n.runs.len()),
        s.state_bytes,
    )
}
struct PreparedRun {
    engine: Rook,
    run_id: String,
    node_id: String,
    session: u128,
    contract: Contract,
    cancel: Arc<tokio::sync::Notify>,
    _lease: RunLease,
}
type RunIdentity = (PathBuf, u128, String);
static OWNERS: std::sync::Mutex<BTreeMap<RunIdentity, Arc<tokio::sync::Notify>>> =
    std::sync::Mutex::new(BTreeMap::new());
struct RunLease {
    key: (PathBuf, u128, String),
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for RunLease {
    fn drop(&mut self) {
        OWNERS.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
    }
}
fn directory(rook: &Rook, owner: u128, node: &str, run: Option<&str>) -> PathBuf {
    let base = rook.store.root().join("experiments").join(rook_store::format_session_id(owner)).join(node);
    match run {
        Some(id) => base.join("runs").join(id),
        None => base.join("source"),
    }
}
fn materialize(rook: &Rook, root: &Path, source: &Snapshot) -> Result<()> {
    let relative =
        root.strip_prefix(rook.store.root()).map_err(|_| bad("experiment directory escaped its store"))?;
    let output = RecoveryFiles::open(rook.store.root(), relative, true).map_err(|e| bad(e.to_string()))?;
    // Check every source object before the first file publication.
    for f in source.files.values() {
        let object = ObjectId::from_hex(&f.object).ok_or_else(|| bad("invalid source object"))?;
        let bytes = rook.store.get_range(&object, 0, f.bytes + 1)?;
        if bytes.len() != f.bytes || ObjectId::of(&bytes) != object {
            return Err(bad("experiment source object missing or corrupt"));
        }
    }
    for (path, f) in &source.files {
        let object = ObjectId::from_hex(&f.object).ok_or_else(|| bad("invalid source object"))?;
        let bytes = rook.store.get_range(&object, 0, f.bytes + 1)?;
        if !output
            .replace_checked(Path::new(path), None, None, Some(&bytes), f.mode)
            .map_err(|e| bad(e.to_string()))?
        {
            return Err(bad("experiment checkout contains unexpected files; inspect it before retrying"));
        }
    }
    Ok(())
}
fn make_node(
    rook: &Rook,
    owner: u128,
    parent: Option<&Node>,
    title: String,
    hypothesis: String,
    parameters: BTreeMap<String, String>,
    source: Snapshot,
) -> Result<Node> {
    let id = new_id();
    let workspace = directory(rook, owner, &id, None);
    let session = rook_store::new_session_id();
    Ok(Node {
        id,
        parent: parent.map(|n| n.id.clone()),
        session: rook_store::format_session_id(session),
        title,
        hypothesis,
        parameters,
        source,
        workspace: workspace.display().to_string(),
        state: "preparing".into(),
        error: None,
        answered: false,
        revisions: 0,
        runs: Vec::new(),
    })
}
fn publish_node(rook: &Rook, owner: u128, id: &str, p: &mut Project) -> Result<()> {
    admit(rook, p)?;
    let n = node(p, id)?;
    shell_undo::pin(&rook.store, owner, Path::new(&n.workspace), &n.source, "experiment-source")?;
    save(rook, owner, p)?;
    let result = (|| -> Result<()> {
        let n = node(p, id)?;
        materialize(rook, Path::new(&n.workspace), &n.source)?;
        let session = rook_store::parse_session_id(&n.session).ok_or_else(|| bad("invalid node session"))?;
        let mut meta = SessionMeta::new(session, &n.title, &n.workspace, rook_store::now_unix());
        meta.parent = Some(
            n.parent
                .as_deref()
                .map(|id| node(p, id))
                .transpose()?
                .and_then(|n| rook_store::parse_session_id(&n.session))
                .unwrap_or(owner),
        );
        meta.tags = vec!["experiment".into()];
        rook.store.create_session(&meta)?;
        let bytes = serde_json::to_vec(&json!({"owner":rook_store::format_session_id(owner),"node":n.id}))?;
        rook.store.kv_update_session(
            session,
            &format!("experiment-owner/{session:032x}"),
            1024,
            None,
            |_| Ok(bytes),
        )?;
        Ok(())
    })();
    let n = node_mut(p, id)?;
    n.state = if result.is_ok() { "ready" } else { "failed" }.into();
    n.error = result.as_ref().err().map(|e| rook_tools::elide_middle(&e.to_string(), 4096));
    save(rook, owner, p)?;
    result?;
    Ok(())
}
fn answered(n: &Node) -> Result<&Run> {
    n.runs.iter().rev().find(|r| r.state == "answered").ok_or_else(|| bad("node has no measured answer"))
}
fn compare(p: &Project, baseline: &str, s: &Settings) -> Result<Value> {
    let base = node(p, baseline)?;
    let measured = answered(base)?;
    let metrics = measured.metrics.as_ref().ok_or_else(|| bad("baseline metrics missing"))?;
    let mut rows = Vec::new();
    for n in p.nodes.iter().filter(|n| n.id == baseline || n.parent.as_deref() == Some(baseline)) {
        let Ok(run) = answered(n) else {
            rows.push(json!({"node":n.id,"parent":n.parent,"title":n.title,
                "status":"no_measured_answer","runs":n.runs.iter().map(|r|json!({"id":r.id,"state":r.state,"error":r.error})).collect::<Vec<_>>()}));
            continue;
        };
        let values = run.metrics.as_ref().ok_or_else(|| bad("measured metrics missing"))?;
        let mut deltas = BTreeMap::new();
        let mut wins = 0;
        let mut losses = 0;
        for (name, direction) in &p.contract.metrics {
            let before = metrics[name];
            let after = values[name];
            let change = after - before;
            let improved = match direction {
                Direction::Minimize => after < before,
                Direction::Maximize => after > before,
            };
            if improved {
                wins += 1;
            } else if after != before {
                losses += 1;
            }
            deltas.insert(name, json!({"baseline":before,"value":after,"delta":change.is_finite().then_some(change),"delta_overflow":!change.is_finite(),"direction":direction,"improved":improved}));
        }
        rows.push(json!({"node":n.id,"parent":n.parent,"title":n.title,"run":run.id,"source":run.source,
            "deltas":deltas,"wins":wins,"losses":losses,"promotable":wins>0 && losses==0}));
    }
    let token = shell_undo::hash(&(p.contract_id.clone(), baseline, &rows), s.state_bytes)?;
    Ok(
        json!({"baseline":baseline,"selection_baseline":p.winner.as_ref().unwrap_or(&p.baseline),"contract":p.contract_id,"rows":rows,"token":token,"winner":p.winner}),
    )
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 65536 {
        return Err(bad("experiment request exceeds 64 KiB"));
    }
    if text.trim().is_empty() {
        return Ok(Request::List);
    }
    Ok(serde_json::from_str(text)?)
}
fn wait_by_default() -> bool {
    true
}
pub(crate) fn frozen(rook: &Rook, session: u128) -> Result<bool> {
    let Some(bytes) = rook.store.kv_get_limited(&format!("experiment-owner/{session:032x}"), 1024)? else {
        return Ok(false);
    };
    let mapping: Value = serde_json::from_slice(&bytes)?;
    let owner = mapping["owner"]
        .as_str()
        .and_then(rook_store::parse_session_id)
        .ok_or_else(|| bad("experiment source ownership is invalid"))?;
    let id = mapping["node"].as_str().ok_or_else(|| bad("experiment node ownership is invalid"))?;
    let project = load(rook, owner)?.ok_or_else(|| {
        bad("experiment owner is missing; inspect the retained source before changing files")
    })?;
    Ok(node(&project, id)?.answered
        || node(&project, id)?.runs.iter().any(|r| matches!(r.state.as_str(), "started" | "interrupted")))
}
impl Rook {
    /// Shared frontend contract; runs continue as owned tasks after disconnect.
    pub async fn experiment_request(
        &self,
        owner: u128,
        request: Request,
        vault: Arc<crate::Vault>,
    ) -> Result<Value> {
        if let Request::Run { node, token, wait } = request {
            let prepared = self.prepare_run(owner, &node, &token)?;
            let admission = json!({"node":prepared.node_id,"run":prepared.run_id,"session":rook_store::format_session_id(prepared.session),"state":"started"});
            let task = tokio::spawn(async move {
                let PreparedRun { mut engine, run_id, node_id, session, contract, cancel, _lease } = prepared;
                let _lease = _lease;
                let result = tokio::select! {
                    result = engine.execute_experiment(session, &contract, &vault) => result,
                    _ = cancel.notified() => Err(bad("experiment cancelled; inspect execution recovery before rerunning")),
                };
                engine.finish_experiment(owner, &node_id, &run_id, result)
            });
            if !wait {
                return Ok(admission);
            }
            return task
                .await
                .map_err(|e| bad(format!("experiment worker interrupted; inspect its saved run: {e}")))?;
        }
        let _changing = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let s = &self.config.experiments;
        let meta = self
            .store
            .get_session_limited(owner, 256 * 1024)?
            .ok_or_else(|| bad("experiment owner missing"))?;
        let root = self.workspace.canonicalize().map_err(|e| bad(e.to_string()))?;
        if Path::new(&meta.workspace).canonicalize().ok().as_ref() != Some(&root) {
            return Err(bad("experiment owner belongs to another workspace"));
        }
        let loaded = load(self, owner)?;
        if let Request::Create { title, hypothesis, contract: definition, parameters: values } = request {
            if loaded.is_some() {
                return Err(bad("this session already owns an experiment project"));
            }
            text(&title, &hypothesis)?;
            parameters(&values)?;
            contract(&definition, s)?;
            if self.store.root().canonicalize().map_err(|e| bad(e.to_string()))?.starts_with(&root) {
                return Err(bad("experiment store must be outside its source workspace"));
            }
            let source = shell_undo::capture(&self.store, &root, &s.capture(), true)?;
            let baseline = make_node(self, owner, None, title, hypothesis, values, source)?;
            let mut p = Project {
                version: 1,
                workspace: root.display().to_string(),
                contract_id: shell_undo::hash(&definition, s.state_bytes)?,
                environment: environment()?,
                contract: definition,
                baseline: baseline.id.clone(),
                winner: None,
                nodes: vec![baseline],
            };
            let id = p.baseline.clone();
            publish_node(self, owner, &id, &mut p)?;
            return Ok(show(self, owner, &p.nodes[0]));
        }
        let Some(mut p) = loaded else {
            return match request {
                Request::List => Ok(json!({"items":[]})),
                _ => Err(bad("create an experiment baseline in this session first")),
            };
        };
        if p.workspace != root.display().to_string() {
            return Err(bad("experiment project workspace changed"));
        }
        match request {
            Request::List => Ok(
                json!({"baseline":p.baseline,"winner":p.winner,"contract":p.contract,"items":p.nodes.iter().map(|n| show(self, owner, n)).collect::<Vec<_>>() }),
            ),
            Request::Inspect { node: id } => Ok(show(self, owner, node(&p, &id)?)),
            Request::Branch { parent, title, hypothesis, parameters: values } => {
                text(&title, &hypothesis)?;
                if p.nodes.len() >= s.max_nodes {
                    return Err(bad("experiments.max_nodes reached"));
                }
                let parent = node(&p, &parent)?;
                answered(parent)?;
                let values = values.unwrap_or_else(|| parent.parameters.clone());
                parameters(&values)?;
                let child =
                    make_node(self, owner, Some(parent), title, hypothesis, values, parent.source.clone())?;
                let id = child.id.clone();
                p.nodes.push(child);
                publish_node(self, owner, &id, &mut p)?;
                Ok(show(self, owner, node(&p, &id)?))
            }
            Request::Capture { node: id } => {
                let n = node_mut(&mut p, &id)?;
                mutable(n)?;
                if n.revisions >= 8 {
                    return Err(bad(
                        "eight source captures reached; create a fresh project rather than discard evidence",
                    ));
                }
                n.source = shell_undo::capture(&self.store, Path::new(&n.workspace), &s.capture(), true)?;
                n.revisions += 1;
                admit(self, &p)?;
                let n = node(&p, &id)?;
                shell_undo::pin(&self.store, owner, Path::new(&n.workspace), &n.source, "experiment-source")?;
                save(self, owner, &p)?;
                Ok(show(self, owner, node(&p, &id)?))
            }
            Request::Prepare { node: id } => {
                let n = node(&p, &id)?;
                mutable(n)?;
                if n.runs.len() >= s.max_runs {
                    return Err(bad("experiments.max_runs reached"));
                }
                Ok(
                    json!({"node":id,"token":token(owner,&p,n,s)?,"source":n.source.source,"command":p.contract.command,"parameters":n.parameters,"parameters_path":".rook-experiment/parameters.json","metrics_path":METRICS,"artifacts":p.contract.artifacts,"timeout_secs":s.timeout_secs,"environment":p.environment}),
                )
            }
            Request::Compare { baseline } => compare(&p, baseline.as_deref().unwrap_or(&p.baseline), s),
            Request::Promote { node: id, token: expected } => {
                let baseline = p.winner.as_ref().unwrap_or(&p.baseline).clone();
                let report = compare(&p, &baseline, s)?;
                if report["token"].as_str() != Some(&expected) {
                    return Err(bad("comparison changed; inspect a fresh comparison before promotion"));
                }
                let row = report["rows"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|r| r["node"] == id))
                    .ok_or_else(|| bad("promotion node missing"))?;
                if row["promotable"] != true {
                    return Err(bad("promotion requires a measured improvement without a metric regression"));
                }
                p.winner = Some(id.clone());
                save(self, owner, &p)?;
                Ok(
                    json!({"winner":id,"baseline":baseline,"meaning":"winner selection only; no user workspace files changed"}),
                )
            }
            Request::Read { node: id, run, artifact, offset } => {
                let n = node(&p, &id)?;
                let r = n
                    .runs
                    .iter()
                    .find(|r| r.id == run)
                    .ok_or_else(|| bad("run does not belong to this node"))?;
                let f = match artifact.as_ref() {
                    Some(path) if path == METRICS => r.metrics_source.as_ref(),
                    Some(path) => r.artifacts.get(path),
                    None => r.output.as_ref(),
                }
                .ok_or_else(|| bad("retained run output/artifact missing"))?;
                if offset > f.bytes {
                    return Err(bad("output cursor is beyond this retained source"));
                }
                let object = ObjectId::from_hex(&f.object).ok_or_else(|| bad("invalid artifact object"))?;
                let retained = self.store.get_range(&object, 0, f.bytes + 1)?;
                if retained.len() != f.bytes || ObjectId::of(&retained) != object {
                    return Err(bad("retained output/artifact is missing or corrupt"));
                }
                let bytes = retained
                    .get(offset..offset.saturating_add(4096).min(retained.len()))
                    .ok_or_else(|| bad("invalid artifact cursor"))?;
                let next = (offset + bytes.len() < f.bytes).then_some(offset + bytes.len());
                Ok(
                    json!({"node":id,"run":run,"artifact":artifact,"source":f.object,"offset":offset,"next":next,"text":vault.redact(&String::from_utf8_lossy(bytes))}),
                )
            }
            Request::Recover { node: id, run, note } => {
                if note.trim().is_empty() || note.len() > 4096 {
                    return Err(bad("record an inspection note of 1..4096 bytes"));
                }
                let n = node_mut(&mut p, &id)?;
                let r = n.runs.iter_mut().find(|r| r.id == run).ok_or_else(|| bad("run missing"))?;
                let session =
                    rook_store::parse_session_id(&r.session).ok_or_else(|| bad("run session invalid"))?;
                if OWNERS.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&(
                    self.store.root().to_path_buf(),
                    owner,
                    run.clone(),
                )) {
                    return Err(bad("the experiment worker is still live; wait for its terminal receipt"));
                }
                if crate::execution::is_active(self, session) {
                    return Err(bad("this run is still live; stop its process owner before recovery"));
                }
                if let Some(reason) = self.recovery_block(session)? {
                    return Err(bad(reason));
                }
                if !matches!(r.state.as_str(), "started" | "interrupted") {
                    return Err(bad("this run has no unknown terminal result to review"));
                }
                r.state = "reviewed".into();
                r.inspection = Some(note);
                save(self, owner, &p)?;
                Ok(show(self, owner, node(&p, &id)?))
            }
            Request::Stop { node: id, run } => {
                let n = node(&p, &id)?;
                let r = n.runs.iter().find(|r| r.id == run).ok_or_else(|| bad("run missing"))?;
                let held = OWNERS.lock().unwrap_or_else(|e| e.into_inner());
                let cancel =
                    held.get(&(self.store.root().to_path_buf(), owner, run.clone())).ok_or_else(|| {
                        bad("this run has no live process owner; inspect it and record recovery")
                    })?;
                cancel.notify_one();
                Ok(json!({"node":id,"run":r.id,"state":"stop_requested"}))
            }
            Request::Create { .. } | Request::Run { .. } => unreachable!("creation and run handled above"),
        }
    }
    fn prepare_run(&self, owner: u128, id: &str, expected: &str) -> Result<PreparedRun> {
        let _changing = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let s = &self.config.experiments;
        let mut p = load(self, owner)?.ok_or_else(|| bad("experiment project missing"))?;
        let meta = self
            .store
            .get_session_limited(owner, 256 * 1024)?
            .ok_or_else(|| bad("experiment owner missing"))?;
        let root = self.workspace.canonicalize().map_err(|e| bad(e.to_string()))?;
        if Path::new(&meta.workspace).canonicalize().ok().as_ref() != Some(&root)
            || p.workspace != root.display().to_string()
        {
            return Err(bad("experiment owner belongs to another workspace"));
        }
        let n = node(&p, id)?;
        mutable(n)?;
        if n.runs.len() >= s.max_runs {
            return Err(bad("experiments.max_runs reached"));
        }
        if token(owner, &p, n, s)? != expected {
            return Err(bad("experiment source or parameters changed; inspect a fresh prepare token"));
        }
        if environment()? != p.environment {
            return Err(bad(
                "observed runtime environment differs from the fixed contract; create a new baseline",
            ));
        }
        let sandbox = &self.config.sandbox;
        let (policy, _) = Policy::compile(sandbox.stance, &sandbox.allow, &sandbox.ask, &sandbox.deny);
        match policy.decide(&Risk::Execute(p.contract.command.clone())) {
            Decision::Deny(reason) => return Err(bad(reason)),
            Decision::Ask => {
                return Err(bad(
                    "experiment command needs configured execute permission; add a matching sandbox.allow rule before launching",
                ));
            }
            Decision::Allow => {}
        }
        let permit = RUNS.clone().try_acquire_owned().map_err(|_| {
            bad("experiment run capacity is busy; inspect running nodes before launching more")
        })?;
        let run_id = new_id();
        let path = directory(self, owner, &n.id, Some(&run_id));
        let session = rook_store::new_session_id();
        let mut meta = SessionMeta::new(
            session,
            format!("Run {}", n.title),
            path.display().to_string(),
            rook_store::now_unix(),
        );
        meta.parent = rook_store::parse_session_id(&n.session);
        meta.tags = vec!["experiment-run".into()];
        let values = n.parameters.clone();
        let definition = p.contract.clone();
        let source = n.source.source.clone();
        let snapshot = n.source.clone();
        let parameter_id = shell_undo::hash(&values, s.state_bytes)?;
        let contract_id = p.contract_id.clone();
        let environment_id = p.environment.clone();
        node_mut(&mut p, id)?.runs.push(Run {
            id: run_id.clone(),
            state: "started".into(),
            source,
            snapshot,
            contract: contract_id,
            parameters: parameter_id,
            environment: environment_id,
            workspace: path.display().to_string(),
            session: rook_store::format_session_id(session),
            started_at: rook_store::now_unix(),
            took_ms: 0,
            exit_code: None,
            metrics: None,
            metrics_source: None,
            artifacts: BTreeMap::new(),
            output: None,
            error: None,
            inspection: None,
        });
        admit(self, &p)?;
        save(self, owner, &p)?;
        let r = node(&p, id)?.runs.last().ok_or_else(|| bad("run admission missing"))?;
        materialize(self, &path, &r.snapshot)?;
        let output = RecoveryFiles::open(&path, Path::new("."), false).map_err(|e| bad(e.to_string()))?;
        let data = crate::persistence::encode_with_limit(&values, 32768)?;
        if !output
            .write_new(Path::new(".rook-experiment/parameters.json"), &data)
            .map_err(|e| bad(e.to_string()))?
        {
            return Err(bad("run parameter file already exists"));
        }
        self.store.create_session(&meta)?;
        save(self, owner, &p)?;
        let identity = (self.store.root().to_path_buf(), owner, run_id.clone());
        let cancel = Arc::new(tokio::sync::Notify::new());
        OWNERS.lock().unwrap_or_else(|e| e.into_inner()).insert(identity.clone(), cancel.clone());
        Ok(PreparedRun {
            engine: self.for_workspace(path),
            run_id,
            node_id: id.into(),
            session,
            contract: definition,
            cancel,
            _lease: RunLease { key: identity, _permit: permit },
        })
    }
    async fn execute_experiment(
        &mut self,
        session: u128,
        c: &Contract,
        vault: &Arc<crate::Vault>,
    ) -> Result<(rook_tools::ToolOutcome, u64)> {
        if let Some(reason) = self.recovery_block(session)? {
            return Err(bad(reason));
        }
        let journal = crate::execution::Journal::start(self, session, None, false)?;
        journal.admit("Execute the explicitly selected fixed experiment contract")?;
        let args = json!({"command":c.command,"timeout_secs":self.config.experiments.timeout_secs});
        journal.begin("run_command", &vault.redact(&args.to_string()), true, false, None)?;
        let mut context = rook_tools::ToolContext::new(self.workspace.clone());
        context.require_command_cleanup = true;
        context.max_output_bytes = self.config.experiments.output_bytes;
        context.command_timeout = std::time::Duration::from_secs(self.config.experiments.timeout_secs);
        context.isolate = self.config.sandbox.isolate;
        context.isolation.network = self.config.sandbox.network;
        context.secrets = Some(vault.clone());
        let start = std::time::Instant::now();
        let result = rook_tools::exec::RunCommand.call(&context, &args).await;
        match result {
            Ok(mut outcome) => {
                outcome.content = vault.redact(&outcome.content);
                journal.complete(&outcome.content, None, None)?;
                journal.finish("evaluated", None)?;
                Ok((outcome, start.elapsed().as_millis().min(u64::MAX as u128) as u64))
            }
            Err(error) => {
                journal.complete(&vault.redact(&error.to_string()), None, None)?;
                journal.finish("failed", None)?;
                Err(bad(error.to_string()))
            }
        }
    }
    fn finish_experiment(
        &self,
        owner: u128,
        id: &str,
        run_id: &str,
        result: Result<(rook_tools::ToolOutcome, u64)>,
    ) -> Result<Value> {
        let _changing = CHANGES.lock().unwrap_or_else(|e| e.into_inner());
        let s = &self.config.experiments;
        let mut p = load(self, owner)?.ok_or_else(|| bad("experiment owner/project was deleted"))?;
        let position =
            p.nodes.iter().position(|n| n.id == id).ok_or_else(|| bad("experiment node disappeared"))?;
        let run_position = p.nodes[position]
            .runs
            .iter()
            .position(|r| r.id == run_id)
            .ok_or_else(|| bad("run receipt disappeared"))?;
        let observed = (|| -> Result<Observation> {
            let (outcome, _) = result.as_ref().map_err(|e| bad(e.to_string()))?;
            if outcome.is_error || outcome.meta.get("processes_stopped") != Some(&json!(true)) {
                return Err(bad("command failed or owned process cleanup was not certified"));
            }
            let reader = RecoveryFiles::open(&self.workspace, Path::new("."), false)
                .map_err(|e| bad(e.to_string()))?;
            for (path, f) in &p.nodes[position].runs[run_position].snapshot.files {
                let bytes = reader
                    .read(Path::new(path), f.bytes)
                    .map_err(|e| bad(format!("run modified its frozen source {path}: {e}")))?;
                if ObjectId::of(&bytes).to_hex() != f.object
                    || file_mode(&reader.metadata(Path::new(path)).map_err(|e| bad(e.to_string()))?) != f.mode
                {
                    return Err(bad("run modified its frozen source; metrics are not a verified answer"));
                }
            }
            let bytes = reader.read(Path::new(METRICS), 65536).map_err(|e| bad(format!("metrics: {e}")))?;
            let metrics: BTreeMap<String, f64> = serde_json::from_slice(&bytes)?;
            if metrics.len() != p.contract.metrics.len()
                || metrics.iter().any(|(k, v)| !p.contract.metrics.contains_key(k) || !v.is_finite())
            {
                return Err(bad("metrics must contain exactly the declared finite numeric values"));
            }
            let metrics_file = File {
                object: self.store.put(Kind::FileBlob, &bytes)?.to_hex(),
                bytes: bytes.len(),
                mode: 0o600,
            };
            let mut artifacts = BTreeMap::new();
            let mut total = 0usize;
            for path in &p.contract.artifacts {
                let bytes = reader
                    .read(Path::new(path), s.file_bytes.min(s.total_bytes.saturating_sub(total)))
                    .map_err(|e| bad(format!("artifact {path}: {e}")))?;
                total += bytes.len();
                let mode = file_mode(&reader.metadata(Path::new(path)).map_err(|e| bad(e.to_string()))?);
                let object = self.store.put(Kind::FileBlob, &bytes)?;
                artifacts.insert(path.clone(), File { object: object.to_hex(), bytes: bytes.len(), mode });
            }
            Ok(Observation { metrics, metrics_source: metrics_file, artifacts })
        })();
        let uncertain = result.as_ref().is_err()
            || result
                .as_ref()
                .is_ok_and(|(outcome, _)| outcome.meta.get("processes_stopped") != Some(&json!(true)));
        let r = &mut p.nodes[position].runs[run_position];
        match result {
            Ok((outcome, took)) => {
                r.took_ms = took;
                r.exit_code = outcome.meta.get("exit_code").and_then(Value::as_i64);
                let content = rook_tools::elide_middle(&outcome.content, s.output_bytes);
                let bytes = content.as_bytes();
                let object = self.store.put(Kind::Message, bytes)?;
                r.output = Some(File { object: object.to_hex(), bytes: bytes.len(), mode: 0o600 });
                self.store.append_event(
                    owner,
                    rook_store::NewEvent::new(rook_store::EventKind::Note, Kind::Message, bytes)
                        .label("experiment-output"),
                )?;
            }
            Err(error) => {
                r.error = Some(rook_tools::elide_middle(&error.to_string(), 4096));
            }
        }
        match observed {
            Ok(Observation { metrics, metrics_source, artifacts }) => {
                r.state = "answered".into();
                r.metrics = Some(metrics);
                r.metrics_source = Some(metrics_source);
                r.artifacts = artifacts;
                p.nodes[position].answered = true;
            }
            Err(error) => {
                r.state = if uncertain { "interrupted" } else { "failed" }.into();
                r.error = Some(rook_tools::elide_middle(&error.to_string(), 4096));
            }
        }
        let receipt = &p.nodes[position].runs[run_position];
        let mut retained = receipt.artifacts.clone();
        if let Some(metrics) = &receipt.metrics_source {
            retained.insert(METRICS.into(), metrics.clone());
        }
        let snapshot = Snapshot { source: shell_undo::hash(&retained, s.state_bytes)?, files: retained };
        shell_undo::pin(&self.store, owner, &self.workspace, &snapshot, "experiment-artifacts")?;
        save(self, owner, &p)?;
        Ok(show(self, owner, &p.nodes[position]))
    }
}
