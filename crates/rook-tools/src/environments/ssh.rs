use super::*;
use rook_contain::files::RecoveryFiles;
use serde_json::{Value, json};
use std::{
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};
const WORKER: &str = include_str!("ssh_worker.py");
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    identity: String,
    host: String,
    base: String,
    remote_owner: String,
    local_owner: String,
    pid: u32,
    attached: bool,
    status: String,
    call: Option<String>,
    source: Option<String>,
    pending: Option<String>,
    completed: usize,
    history: Vec<String>,
    error: Option<String>,
}
pub(super) struct Resource {
    host: String,
    program: String,
    base: String,
    root: PathBuf,
    state_root: PathBuf,
    identity: String,
    owner: String,
    settings: Settings,
    gate: Arc<tokio::sync::Semaphore>,
    uncertain: AtomicBool,
    initialized: AtomicBool,
    cancelled: AtomicBool,
    socket: PathBuf,
    socket_dir: PathBuf,
    remote_owner: Mutex<String>,
    changing: Mutex<()>,
}
impl Resource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: String,
        program: String,
        base: String,
        root: PathBuf,
        state_root: PathBuf,
        identity: String,
        settings: Settings,
    ) -> Result<Self> {
        let owner = ulid::Ulid::generate().to_string();
        let socket_dir = std::env::temp_dir().join(format!("rook-ssh-{}", &owner[10..]));
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&socket_dir).map_err(|e| invalid(e.to_string()))?;
        let socket = socket_dir.join("c");
        if socket.as_os_str().len() > 90 {
            let _ = std::fs::remove_dir(&socket_dir);
            return Err(invalid(
                "SSH control socket path exceeds 90 bytes; configure a shorter temporary directory",
            ));
        }
        Ok(Self {
            host,
            program,
            base,
            root,
            state_root,
            identity,
            remote_owner: Mutex::new(owner.clone()),
            owner,
            settings,
            gate: Arc::new(tokio::sync::Semaphore::new(1)),
            uncertain: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            socket,
            socket_dir,
            changing: Mutex::new(()),
        })
    }
    pub fn disposable(self: &Arc<Self>) -> bool {
        Arc::strong_count(self) == 1
            && !self.initialized.load(Ordering::Acquire)
            && !self.uncertain.load(Ordering::Acquire)
    }
    fn files(&self) -> Result<RecoveryFiles> {
        RecoveryFiles::open(&self.state_root, Path::new("execution-environments"), true)
            .map_err(|e| invalid(e.to_string()))
    }
    fn load(&self) -> Result<Option<State>> {
        let bytes = match self.files()?.read(Path::new(&format!("{}.json", self.identity)), 16384) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(invalid(e.to_string())),
        };
        let state: State = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        if state.version != 1
            || state.identity != self.identity
            || state.host != self.host
            || state.base != self.base
            || state.remote_owner.len() != 26
            || state.local_owner.len() != 26
            || state.completed > self.settings.max_entries * 2
            || state.history.len() > self.settings.max_calls
        {
            return Err(invalid("retained SSH resource identity or progress bound changed"));
        }
        Ok(Some(state))
    }
    fn save(&self, state: &State) -> Result<()> {
        let files = self.files()?;
        let path = PathBuf::from(format!("{}.json", self.identity));
        let old = match files.read(&path, 16384) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(invalid(e.to_string())),
        };
        let bytes = serde_json::to_vec(state).map_err(|e| invalid(e.to_string()))?;
        if bytes.len() > 16384 {
            return Err(invalid("SSH state exceeds 16 KiB"));
        }
        if !files
            .replace_checked(&path, old.as_deref(), old.as_ref().map(|_| 0o600), Some(&bytes), 0o600)
            .map_err(|e| invalid(e.to_string()))?
        {
            return Err(invalid("SSH state changed concurrently; inspect it"));
        }
        Ok(())
    }
    fn fresh(&self) -> State {
        State {
            version: 1,
            identity: self.identity.clone(),
            host: self.host.clone(),
            base: self.base.clone(),
            remote_owner: self.remote_owner.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            local_owner: self.owner.clone(),
            pid: std::process::id(),
            attached: true,
            status: "starting".into(),
            call: None,
            source: None,
            pending: None,
            completed: 0,
            history: Vec::new(),
            error: None,
        }
    }
    fn config(&self, action: &str, state: &State) -> Value {
        json!({"action":action,"base":self.base,"identity":self.identity,"owner":state.remote_owner,"token":state.call,"previous":state.source,"limits":{"file_bytes":self.settings.file_bytes,"total_bytes":self.settings.total_bytes,"max_files":self.settings.max_files,"max_entries":self.settings.max_entries,"exclude":self.settings.exclude}})
    }
    fn args(&self, remote: Option<String>) -> Vec<String> {
        let mut args = vec![
            "-T".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            format!("ConnectTimeout={}", self.settings.setup_timeout_secs.min(30)),
            "-o".into(),
            "ConnectionAttempts=1".into(),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            "ControlPersist=60".into(),
            "-o".into(),
            format!("ControlPath={}", self.socket.display()),
            "-o".into(),
            "ServerAliveInterval=5".into(),
            "-o".into(),
            "ServerAliveCountMax=2".into(),
            self.host.clone(),
        ];
        if let Some(remote) = remote {
            args.push(remote);
        }
        args
    }
    fn remote(&self, action: &str, state: &State) -> Result<String> {
        let config =
            serde_json::to_string(&self.config(action, state)).map_err(|e| invalid(e.to_string()))?;
        Ok(["python3", "-I", "-c", WORKER, &config]
            .iter()
            .map(|part| quoted(part))
            .collect::<Vec<_>>()
            .join(" "))
    }
    fn invoke(
        &self,
        action: &str,
        state: &State,
        input: Option<Arc<Vec<u8>>>,
        cap: usize,
    ) -> Result<Vec<u8>> {
        let mut command = Command::new(&self.program);
        rook_contain::quietly(&mut command);
        command.args(self.args(Some(self.remote(action, state)?)));
        let output = transport::run(command, input, cap, self.settings.setup_timeout_secs)?;
        if output.code != 0 {
            return Err(invalid(format!(
                "SSH {action} refused (exit {}): {}",
                output.code,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output.stdout)
    }
    fn inspect_remote(&self, state: &State) -> Result<Value> {
        let bytes = self.invoke("inspect", state, None, 8192)?;
        serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
    }
    fn snapshot_path(&self, call: &str, which: &str) -> Result<PathBuf> {
        if call.len() != 26 || !call.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(invalid("invalid SSH call identity"));
        }
        Ok(PathBuf::from(format!("ssh/{}/{call}/{which}.json", self.identity)))
    }
    fn save_snapshot(&self, call: &str, which: &str, snapshot: &sync::Snapshot) -> Result<()> {
        let files = self.files()?;
        let path = self.snapshot_path(call, which)?;
        let cap = sync::response_cap(&self.settings);
        let bytes = serde_json::to_vec(snapshot).map_err(|e| invalid(e.to_string()))?;
        if bytes.len() > cap {
            return Err(invalid("SSH retained snapshot exceeded its encoded bound"));
        }
        let old = match files.read(&path, cap) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(invalid(e.to_string())),
        };
        if !files
            .replace_checked(&path, old.as_deref(), old.as_ref().map(|_| 0o600), Some(&bytes), 0o600)
            .map_err(|e| invalid(e.to_string()))?
        {
            return Err(invalid("SSH snapshot changed concurrently"));
        }
        Ok(())
    }
    fn load_snapshot(&self, call: &str, which: &str) -> Result<sync::Snapshot> {
        let bytes = self
            .files()?
            .read(&self.snapshot_path(call, which)?, sync::response_cap(&self.settings))
            .map_err(|e| invalid(e.to_string()))?;
        let snapshot: sync::Snapshot = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        snapshot.validate(&self.settings)?;
        Ok(snapshot)
    }
    pub async fn prepare(
        self: &Arc<Self>,
        profile: &str,
        command: &str,
        cwd: &Path,
        secrets: &[(String, String)],
    ) -> Result<Prepared> {
        let permit =
            self.gate.clone().try_acquire_owned().map_err(|_| {
                invalid("SSH workspace is busy; wait for its command or stop its background job")
            })?;
        if self.uncertain.load(Ordering::Acquire) {
            return Err(invalid(
                "SSH resource has retained uncertain effects; inspect/recover it before another command",
            ));
        }
        let cwd = cwd.to_str().ok_or_else(|| invalid("SSH cwd is not UTF-8"))?;
        if command.len() > 128 * 1024 {
            return Err(invalid("SSH command exceeds 128 KiB"));
        }
        if secrets.len() > 32
            || secrets
                .iter()
                .any(|(name, value)| name.len() > 128 || value.len() > 65536 || value.contains('\0'))
            || secrets.iter().map(|(_, value)| value.len()).sum::<usize>() > 256 * 1024
        {
            return Err(invalid("SSH per-call credentials exceed their admission bounds"));
        }
        let input = serde_json::to_vec(
            &json!({"command":command,"cwd":cwd,"env":secrets.iter().cloned().collect::<BTreeMap<_,_>>()}),
        )
        .map_err(|e| invalid(e.to_string()))?;
        if input.len() > 1024 * 1024 {
            return Err(invalid("SSH command/credential input exceeds 1 MiB"));
        }
        let permit = Arc::new(permit);
        let before = sync::Snapshot::capture(&self.root, &self.settings)?;
        let saved = self.load()?;
        if !self.initialized.load(Ordering::Acquire) && saved.as_ref().is_some_and(|s| s.status != "released")
        {
            return Err(invalid(
                "a previous process retained this SSH workspace; inspect/recover/release it explicitly",
            ));
        }
        let mut state = saved.unwrap_or_else(|| self.fresh());
        self.cancelled.store(false, Ordering::Release);
        if state.status == "released" {
            let history = state.history.clone();
            *self.remote_owner.lock().unwrap_or_else(|e| e.into_inner()) = ulid::Ulid::generate().to_string();
            state = self.fresh();
            state.history = history;
            *self.remote_owner.lock().unwrap_or_else(|e| e.into_inner()) = state.remote_owner.clone();
        }
        let token = ulid::Ulid::generate().to_string();
        while state.history.len() >= self.settings.max_calls {
            let old = state.history.remove(0);
            let files = self.files()?;
            for which in ["before", "after"] {
                let path = self.snapshot_path(&old, which)?;
                match files.read(&path, sync::response_cap(&self.settings)) {
                    Ok(bytes) => {
                        if !files
                            .replace_checked(&path, Some(&bytes), Some(0o600), None, 0o600)
                            .map_err(|e| invalid(e.to_string()))?
                        {
                            return Err(invalid("SSH retained source changed while pruning"));
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(invalid(e.to_string())),
                }
            }
            files
                .remove_directory(Path::new(&format!("ssh/{}/{}", self.identity, old)))
                .map_err(|e| invalid(e.to_string()))?;
        }
        state.history.push(token.clone());
        self.save_snapshot(&token, "before", &before)?;
        state.call = Some(token.clone());
        state.local_owner = self.owner.clone();
        state.pid = std::process::id();
        state.status = "preparing".into();
        state.pending = None;
        state.completed = 0;
        state.error = None;
        self.save(&state)?;
        let mut setup = Setup { resource: self.clone(), armed: true };
        let expected_source = before.hash()?;
        let resource = self.clone();
        let saved = state.clone();
        let upload_input = Arc::new(before.frames(&self.settings)?);
        let setup_permit = permit.clone();
        let prepared = tokio::task::spawn_blocking(move || -> Result<()> {
            let _permit = setup_permit;
            if resource.cancelled.load(Ordering::Acquire) {
                return Err(invalid("SSH setup was cancelled"));
            }
            if !resource.initialized.load(Ordering::Acquire) {
                resource.invoke("init", &saved, None, 8192)?;
            }
            if resource.cancelled.load(Ordering::Acquire) {
                return Err(invalid("SSH setup was cancelled after connection"));
            }
            let bytes = resource.invoke("upload", &saved, Some(upload_input), 8192)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
            if value["prepared"] != true || value["source"] != expected_source {
                return Err(invalid("SSH upload did not certify its source"));
            }
            Ok(())
        })
        .await
        .map_err(|e| ToolError::ExecutionUnknown(e.to_string()))?;
        if let Err(error) = prepared {
            return Err(ToolError::ExecutionUnknown(error.to_string()));
        }
        state.status = "running".into();
        self.save(&state)?;
        self.initialized.store(true, Ordering::Release);
        setup.armed = false;
        let remote = self.remote("run", &state)?;
        let wrapped = std::iter::once(self.program.clone())
            .chain(self.args(Some(remote)))
            .map(|part| quoted(&part))
            .collect::<Vec<_>>()
            .join(" ");
        let lease = Arc::new(Lease {
            resource: self.clone(),
            _permit: permit,
            call: token.clone(),
            finished: AtomicBool::new(false),
            stopped: Mutex::new(None),
            effect_unknown: AtomicBool::new(false),
        });
        Ok(Prepared {
            command: wrapped,
            input: Some(Arc::new(input)),
            lease: Arc::new(super::Lease::Ssh(lease)),
            description: json!({"profile":profile,"kind":"ssh","resource":format!("ssh-{}",state.remote_owner),"call":token,"host":self.host,"workspace":format!("{}/.rook-exec/{}/{}/workspace",self.base,self.identity,state.remote_owner),"cwd":cwd,"exclude":self.settings.exclude}),
        })
    }
    fn finish(&self, call: &str, exit: Option<i32>) -> Result<()> {
        let _changing = self.changing.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.load()?.ok_or_else(|| invalid("SSH state disappeared"))?;
        if state.call.as_deref() != Some(call) {
            return Err(invalid("SSH call ownership changed"));
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err(invalid("SSH writeback was cancelled; inspect its retained sources"));
        }
        let raw = self.invoke("dump", &state, None, sync::response_cap(&self.settings))?;
        let (meta, after) = sync::Snapshot::parse(&raw, &self.settings)?;
        if meta["identity"] != self.identity
            || meta["owner"] != state.remote_owner
            || meta["state"]["token"] != call
            || meta["state"]["stopped"] != true
            || meta["state"]["terminal"] != true
        {
            return Err(invalid("SSH remote completion certificate mismatch"));
        }
        if let Some(exit) = exit
            && (exit < 0 || meta["state"]["code"].as_i64() != Some(i64::from(exit)))
        {
            return Err(invalid(
                "SSH client exit does not match the actual remote command; inspect its retained state",
            ));
        }
        if exit.is_none() && self.load_snapshot(call, "after")?.hash()? != after.hash()? {
            return Err(invalid(
                "remote file source changed after recovery preview; inspect and prepare recovery again",
            ));
        }
        self.save_snapshot(call, "after", &after)?;
        let before = self.load_snapshot(call, "before")?;
        state.status = "publishing".into();
        self.save(&state)?;
        let result = after.publish(&before, &self.root, &self.settings, exit.is_none(), |path, done| {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(invalid("SSH writeback cancelled; inspect its retained pending path"));
            }
            if done {
                state.completed += 1;
                state.pending = None;
            } else {
                state.pending = Some(path.into());
            }
            self.save(&state)
        });
        if let Err(error) = result {
            state.status = "writeback_unknown".into();
            state.error = Some(crate::elide_middle(&error.to_string(), 2048));
            self.uncertain.store(true, Ordering::Release);
            self.save(&state)?;
            return Err(error);
        }
        state.status = "ready".into();
        state.source = Some(after.hash()?);
        state.pending = None;
        state.local_owner = self.owner.clone();
        state.pid = std::process::id();
        state.attached = true;
        self.save(&state)?;
        self.uncertain.store(false, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }
    pub async fn manage(self: &Arc<Self>, request: Request) -> Result<Value> {
        let resource = self.clone();
        tokio::task::spawn_blocking(move || resource.manage_sync(request))
            .await
            .map_err(|e| ToolError::ExecutionUnknown(e.to_string()))?
    }
    fn view(&self) -> Result<Value> {
        let state = self.load()?;
        let (mut observed, inspection_error) =
            if let Some(state) = state.as_ref().filter(|s| s.status != "released") {
                match self.inspect_remote(state) {
                    Ok(value) => (Some(value), None),
                    Err(error) => (None, Some(crate::elide_middle(&error.to_string(), 2048))),
                }
            } else {
                (None, None)
            };
        if let (Some(state), Some(observed)) = (&state, &mut observed) {
            observed["id"] = json!(format!("ssh-{}", state.remote_owner));
            observed["running"] = observed["state"]["running"].clone();
        }
        let foreign = state.as_ref().is_some_and(|s| {
            s.attached && s.local_owner != self.owner && s.status != "released" && process_alive(s.pid)
        });
        Ok(
            json!({"identity":self.identity,"workspace":self.root,"saved":state,"observed":observed,"inspection_error":inspection_error,"busy":self.gate.available_permits()==0,"foreign_owner_active":foreign}),
        )
    }
    fn read(&self, call: &str, which: &str, path: &str, offset: u64) -> Result<Value> {
        let state = self.load()?.ok_or_else(|| invalid("SSH resource is missing"))?;
        if !state.history.iter().any(|id| id == call)
            || !matches!(which, "before" | "after")
            || !sync::valid(path)
        {
            return Err(invalid("SSH source selection is unavailable or outside its retained history"));
        }
        let source = self.load_snapshot(call, which)?;
        let file =
            source.files.get(path).ok_or_else(|| invalid("selected file is absent from that source"))?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.data)
            .map_err(|e| invalid(e.to_string()))?;
        let offset = usize::try_from(offset).map_err(|e| invalid(e.to_string()))?;
        if offset > bytes.len() {
            return Err(invalid("SSH source offset exceeds file size"));
        }
        let end = (offset + 4096).min(bytes.len());
        Ok(
            json!({"call":call,"which":which,"path":path,"mode":file.mode,"offset":offset,"total_bytes":bytes.len(),"next_offset":(end<bytes.len()).then_some(end),"bytes_base64":base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end])}),
        )
    }
    fn changes(&self, call: &str, after: Option<&str>) -> Result<Value> {
        let state = self.load()?.ok_or_else(|| invalid("SSH resource is missing"))?;
        if !state.history.iter().any(|id| id == call) || after.is_some_and(|p| !sync::valid(p)) {
            return Err(invalid("SSH change source/cursor is unavailable"));
        }
        let before = self.load_snapshot(call, "before")?;
        let target = self.load_snapshot(call, "after")?;
        let paths = before
            .files
            .keys()
            .chain(target.files.keys())
            .chain(before.dirs.iter())
            .chain(target.dirs.iter())
            .collect::<std::collections::BTreeSet<_>>();
        let mut items = Vec::new();
        let mut next = None;
        for path in paths {
            if after.is_some_and(|cursor| path.as_str() <= cursor) {
                continue;
            }
            let old = before.files.get(path);
            let new = target.files.get(path);
            let old_dir = before.dirs.contains(path);
            let new_dir = target.dirs.contains(path);
            if old.map(|f| (&f.data, f.mode)) == new.map(|f| (&f.data, f.mode)) && old_dir == new_dir {
                continue;
            }
            if items.len() == 16 {
                next = items.last().and_then(|v: &Value| v["path"].as_str()).map(str::to_owned);
                break;
            }
            items.push(json!({"path":path,"before":old.is_some(),"after":new.is_some(),"directory":old_dir||new_dir,"before_mode":old.map(|f|f.mode),"after_mode":new.map(|f|f.mode)}));
        }
        Ok(json!({"call":call,"changes":items,"after":next}))
    }
    fn manage_sync(&self, request: Request) -> Result<Value> {
        if let Request::Read { ref call, ref which, ref path, offset } = request {
            return self.read(call, which, path, offset);
        }
        if let Request::Changes { ref call, ref after } = request {
            return self.changes(call, after.as_deref());
        }
        let view = self.view()?;
        if !matches!(request, Request::Inspect) && !view["inspection_error"].is_null() {
            return Err(invalid(
                "SSH observation is unavailable; restore the verified connection before preparing a change",
            ));
        }
        match request {
            Request::NativeInspect { .. } | Request::NativeUndo { .. } => {
                Err(invalid("native receipt controls cannot manage a replica"))
            }
            Request::Inspect => Ok(view),
            Request::PrepareRelease => {
                if view["busy"] == true || view["foreign_owner_active"] == true {
                    return Err(invalid(
                        "SSH resource still has an active owner; stop it through that frontend",
                    ));
                }
                if view["saved"].is_null() || view["saved"]["status"] == "released" {
                    return Err(invalid("no retained SSH resource to release"));
                }
                let token = identity(&view)?;
                Ok(
                    json!({"token":token,"action":"release","inspected":view,"saved":view["saved"],"observed":view["observed"]}),
                )
            }
            Request::PrepareStop => {
                if view["busy"] == true || view["foreign_owner_active"] == true {
                    return Err(invalid(
                        "stop through the active owner frontend before managing this resource",
                    ));
                }
                if view["saved"].is_null() || view["saved"]["status"] == "released" {
                    return Err(invalid("no retained SSH call to stop"));
                }
                Ok(
                    json!({"token":identity(&view)?,"action":"stop","inspected":view,"saved":view["saved"],"observed":view["observed"]}),
                )
            }
            Request::Stop { token, note } => {
                let _permit =
                    self.gate.clone().try_acquire_owned().map_err(|_| invalid("SSH resource is busy"))?;
                let mut current = self.view()?;
                current["busy"] = json!(false);
                if current["foreign_owner_active"] == true
                    || identity(&current)? != token
                    || note.trim().is_empty()
                    || note.len() > 4096
                {
                    return Err(invalid("SSH stop source changed or note is invalid"));
                }
                let mut state = self.load()?.ok_or_else(|| invalid("SSH stop source missing"))?;
                if state.status == "released" {
                    return Err(invalid("SSH resource already released"));
                }
                let result: Value = serde_json::from_slice(&self.invoke("stop", &state, None, 8192)?)
                    .map_err(|e| invalid(e.to_string()))?;
                if result["stopped"] != true {
                    return Err(invalid("remote stop was not certified"));
                }
                state.status = "interrupted_stopped".into();
                self.save(&state)?;
                self.uncertain.store(true, Ordering::Release);
                Ok(
                    json!({"state":"interrupted_stopped","identity":self.identity,"replayed":false,"note":note,"call":state.call}),
                )
            }
            Request::PrepareRecover => {
                if view["busy"] == true || view["foreign_owner_active"] == true {
                    return Err(invalid("SSH recovery requires an idle or lost owner"));
                }
                let state = self.load()?.ok_or_else(|| invalid("no retained SSH call to recover"))?;
                if view["observed"]["state"]["command_started"] != true {
                    return Err(invalid(
                        "SSH setup did not start a command; release its partial replica instead of publishing upload omissions",
                    ));
                }
                if state.call.is_none() || state.status == "released" || state.status == "ready" {
                    return Err(invalid("SSH resource has no interrupted writeback to recover"));
                }
                if view["observed"]["state"]["stopped"] != true
                    || view["observed"]["state"]["terminal"] != true
                {
                    return Err(invalid(
                        "stop the owned remote command and inspect its terminal state before previewing file recovery",
                    ));
                }
                let raw = self.invoke("dump", &state, None, sync::response_cap(&self.settings))?;
                let (meta, after) = sync::Snapshot::parse(&raw, &self.settings)?;
                if meta["identity"] != self.identity
                    || meta["owner"] != state.remote_owner
                    || meta["state"]["token"] != state.call.as_deref().unwrap_or("")
                {
                    return Err(invalid("SSH recovery source ownership changed"));
                }
                self.save_snapshot(state.call.as_deref().unwrap_or(""), "after", &after)?;
                let view = self.view()?;
                Ok(
                    json!({"token":identity(&view)?,"action":"recover","inspected":view,"saved":view["saved"],"observed":view["observed"],"call":state.call}),
                )
            }
            Request::Recover { token, note } => {
                let _permit =
                    self.gate.clone().try_acquire_owned().map_err(|_| invalid("SSH resource is busy"))?;
                let mut current = self.view()?;
                current["busy"] = json!(false);
                if current["foreign_owner_active"] == true
                    || identity(&current)? != token
                    || note.trim().is_empty()
                    || note.len() > 4096
                {
                    return Err(invalid("SSH recovery source changed or its note is invalid"));
                }
                let state = self.load()?.ok_or_else(|| invalid("SSH recovery source missing"))?;
                if state.status == "ready" || state.status == "released" {
                    return Err(invalid("SSH call already recovered or released"));
                }
                let call =
                    state.call.clone().ok_or_else(|| invalid("SSH setup retained no command source"))?;
                if current["observed"]["state"]["command_started"] != true {
                    return Err(invalid(
                        "SSH transfer has no executed command to recover; release its partial replica",
                    ));
                }
                self.invoke("stop", &state, None, 8192)?;
                self.cancelled.store(false, Ordering::Release);
                self.finish(&call, None)?;
                *self.remote_owner.lock().unwrap_or_else(|e| e.into_inner()) = state.remote_owner;
                Ok(json!({"state":"ready","identity":self.identity,"note":note,"replayed":false,"call":call}))
            }
            Request::Read { call, which, path, offset } => self.read(&call, &which, &path, offset),
            Request::Changes { call, after } => self.changes(&call, after.as_deref()),
            Request::Release { token, note } => {
                let _permit =
                    self.gate.clone().try_acquire_owned().map_err(|_| invalid("SSH resource is busy"))?;
                let mut current = self.view()?;
                current["busy"] = json!(false);
                if current["foreign_owner_active"] == true
                    || identity(&current)? != token
                    || note.trim().is_empty()
                    || note.len() > 4096
                {
                    return Err(invalid("SSH release source changed or its note is invalid; inspect again"));
                }
                let mut state = self.load()?.ok_or_else(|| invalid("no SSH resource to release"))?;
                if state.status == "released" {
                    return Err(invalid("SSH resource was already released"));
                }
                let output = self.invoke("release", &state, None, 8192)?;
                let observed: Value = serde_json::from_slice(&output).map_err(|e| invalid(e.to_string()))?;
                if observed["stopped"] != true
                    && !(observed["missing"] == true
                        && matches!(
                            state.status.as_str(),
                            "ready"
                                | "interrupted_stopped"
                                | "writeback_unknown"
                                | "setup_unknown"
                                | "preparing"
                        ))
                {
                    return Err(invalid(
                        "missing SSH control state cannot certify an unfinished remote command; inspect its processes manually",
                    ));
                }
                state.status = "released".into();
                state.local_owner = self.owner.clone();
                state.pid = std::process::id();
                self.save(&state)?;
                self.uncertain.store(false, Ordering::Release);
                self.initialized.store(false, Ordering::Release);
                Ok(
                    json!({"state":"released","note":note,"identity":self.identity,"replayed":false,"retained_source":state.call}),
                )
            }
        }
    }
}
struct Setup {
    resource: Arc<Resource>,
    armed: bool,
}
impl Drop for Setup {
    fn drop(&mut self) {
        if self.armed {
            self.resource.cancelled.store(true, Ordering::Release);
            self.resource.uncertain.store(true, Ordering::Release);
            if let Ok(Some(mut state)) = self.resource.load() {
                let _ = self.resource.invoke("stop", &state, None, 8192);
                state.status = "setup_unknown".into();
                let _ = self.resource.save(&state);
            }
        }
    }
}
pub(crate) struct Lease {
    resource: Arc<Resource>,
    _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    call: String,
    finished: AtomicBool,
    stopped: Mutex<Option<bool>>,
    effect_unknown: AtomicBool,
}
impl Lease {
    pub fn complete(&self, exit: i32) -> Result<()> {
        if self.stopped.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            return Err(invalid("SSH call was interrupted before writeback"));
        }
        match self.resource.finish(&self.call, Some(exit)) {
            Ok(()) => {
                self.finished.store(true, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.effect_unknown.store(true, Ordering::Release);
                self.resource.uncertain.store(true, Ordering::Release);
                Err(error)
            }
        }
    }
    pub fn uncertain_effects(&self) -> bool {
        self.effect_unknown.load(Ordering::Acquire)
    }
    pub fn stop(&self) -> bool {
        // Remote quiescence does not observe or publish the interrupted file effects.
        self.effect_unknown.store(true, Ordering::Release);
        self.resource.cancelled.store(true, Ordering::Release);
        let mut stopped = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stopped) = *stopped {
            return stopped;
        }
        let observed = (|| -> Result<bool> {
            let mut state =
                self.resource.load()?.ok_or_else(|| invalid("SSH cancellation lost its state"))?;
            if state.call.as_deref() != Some(&self.call) {
                return Err(invalid("SSH cancellation ownership changed"));
            }
            let bytes = self.resource.invoke("stop", &state, None, 8192)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
            let observed = value["stopped"] == true;
            state.status = if observed { "interrupted_stopped" } else { "interrupted_unknown" }.into();
            self.resource.save(&state)?;
            Ok(observed)
        })();
        let observed = observed.unwrap_or(false);
        if !observed && let Ok(Some(mut state)) = self.resource.load() {
            state.status = "interrupted_unknown".into();
            let _ = self.resource.save(&state);
        }
        self.resource.uncertain.store(true, Ordering::Release);
        *stopped = Some(observed);
        observed
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::Acquire) {
            self.stop();
        }
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        if let Ok(Some(mut state)) = self.load()
            && state.local_owner == self.owner
        {
            state.attached = false;
            if state.status == "ready" && self.invoke("release", &state, None, 8192).is_ok() {
                state.status = "released".into();
            }
            let _ = self.save(&state);
        }
        let mut command = Command::new(&self.program);
        rook_contain::quietly(&mut command);
        command
            .args(["-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-S"])
            .arg(&self.socket)
            .args(["-O", "exit", &self.host]);
        let _ = transport::run(command, None, 1024, 5);
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}
fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return true;
    }
    #[cfg(unix)]
    {
        (unsafe { libc::kill(pid as i32, 0) } == 0)
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        true
    }
}
