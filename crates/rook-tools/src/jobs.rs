//! Commands that outlive the call that started them.
//!
//! `run_command` waits, caps and kills at a timeout, which is right for a build
//! and wrong for a server: a watch process or a dev server has no end, so the
//! only way to run one was not to. Here it is started, its output accumulates in
//! the same bounded ends, and the turn carries on.
//!
//! Owned by the front end rather than by a turn, for the reason the language
//! server pool is: a new `AgentLoop` is built per turn, and a registry built
//! there would kill everything in it between one turn and the next.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;

use crate::{Result, ToolError};

/// What one background command has printed and whether it is still going.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub command: String,
    pub started_at: i64,
    pub output: String,
    /// `None` while it runs.
    pub exit_code: Option<i32>,
}

struct Running {
    command: String,
    started_at: i64,
    /// Woken to stop it. The child lives in the task that reads it, so this is
    /// how anyone else reaches it — and it works where a process group is not a
    /// thing that can be signalled.
    stop: Arc<tokio::sync::Notify>,
    /// The same thing without waiting, for `Drop`, which cannot.
    group: Option<u32>,
    text: Arc<Mutex<String>>,
    exit: Arc<Mutex<Option<i32>>>,
}

/// Every background command this front end started.
pub struct Jobs {
    running: Mutex<BTreeMap<String, Running>>,
    started: std::sync::atomic::AtomicU64,
    most: usize,
    /// Per job, so one runaway server cannot spend the memory of all of them.
    max_output_bytes: usize,
}

impl Jobs {
    pub fn new(most: usize, max_output_bytes: usize) -> Self {
        Self {
            running: Mutex::new(BTreeMap::new()),
            started: std::sync::atomic::AtomicU64::new(0),
            most,
            max_output_bytes,
        }
    }

    /// Start one and return its id.
    ///
    /// Refuses past the cap rather than evicting: what would be evicted is a
    /// process somebody is waiting on, and the message says which to stop.
    pub fn start(&self, command: &str, cwd: &std::path::Path) -> Result<String> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        // A finished job is kept so its output can still be read, and a turn
        // that starts a thousand short ones would otherwise keep all thousand.
        // Ids are zero-padded, so the map's own order is the order they started.
        let finished: Vec<String> =
            running.iter().filter(|(_, r)| r.finished()).map(|(id, _)| id.clone()).collect();
        for id in finished.iter().rev().skip(self.most) {
            running.remove(id);
        }

        let live: Vec<&str> =
            running.iter().filter(|(_, r)| !r.finished()).map(|(id, _)| id.as_str()).collect();
        if live.len() >= self.most {
            return Err(ToolError::Invalid {
                tool: "run_command".into(),
                message: format!(
                    "{} background commands are already running, which is `[sandbox] \
                     max_background_jobs` — stop one first: {}",
                    live.len(),
                    live.join(", ")
                ),
            });
        }

        let mut child = crate::exec::spawn_shell(command, cwd)?;
        let id = format!("job{:03}", self.started.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1);
        let text = Arc::new(Mutex::new(String::new()));
        let exit = Arc::new(Mutex::new(None));
        let stop = Arc::new(tokio::sync::Notify::new());
        let group = child.id();

        let (into, code, cap, stopped) = (text.clone(), exit.clone(), self.max_output_bytes, stop.clone());
        tokio::spawn(async move {
            let mut out = child.stdout.take();
            let mut err = child.stderr.take();
            // Both together, for the reason `run_command` reads them together: a
            // command that fills the stderr pipe while stdout is drained blocks
            // on the write and never finishes either.
            let reading = async {
                tokio::join!(drain(&mut out, &into, cap), drain(&mut err, &into, cap));
            };
            tokio::select! {
                _ = reading => {}
                _ = stopped.notified() => {
                    crate::exec::kill_tree(&mut child).await;
                }
            }
            let status = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
            *code.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
        });

        running.insert(
            id.clone(),
            Running { command: command.to_string(), started_at: now(), stop, group, text, exit },
        );
        Ok(id)
    }

    pub fn list(&self) -> Vec<Job> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.iter().map(|(id, r)| r.seen_as(id)).collect()
    }

    pub fn get(&self, id: &str) -> Option<Job> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.get(id).map(|r| r.seen_as(id))
    }

    /// Ask one to stop and keep what it printed: the output is why anyone asked.
    ///
    /// True means there is such a job, not that it has already died — the kill
    /// happens in the task that owns the child, and `job` reports the exit code
    /// once it has one.
    pub fn stop(&self, id: &str) -> bool {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let Some(job) = running.get(id) else { return false };
        // `notify_one` rather than `notify_waiters`: the latter wakes whoever is
        // already waiting and is otherwise lost, so stopping a job in the same
        // breath as starting it would do nothing at all.
        job.stop.notify_one();
        true
    }
}

impl Running {
    fn finished(&self) -> bool {
        self.exit.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn seen_as(&self, id: &str) -> Job {
        Job {
            id: id.to_string(),
            command: self.command.clone(),
            started_at: self.started_at,
            output: self.text.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            exit_code: *self.exit.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// Nothing here outlives the front end, so a `Jobs` going away takes the
/// processes with it: a dev server that survived the agent that started it is
/// one nobody knows to stop.
impl Drop for Jobs {
    fn drop(&mut self) {
        for job in self.running.lock().unwrap_or_else(|e| e.into_inner()).values() {
            // Both, and the signal is the fallback rather than the plan: dropping
            // this usually means the process is on its way out, and the task that
            // would answer the signal may never be scheduled again. Killing the
            // group needs nobody's cooperation; where there is no group to kill,
            // the signal is all there is.
            crate::exec::kill_group(job.group);
            job.stop.notify_one();
        }
    }
}

/// Appends until the cap, then drops the head rather than the tail: a server's
/// interesting line is the one it printed most recently.
async fn drain(
    stream: &mut Option<impl tokio::io::AsyncRead + Unpin>,
    into: &Arc<Mutex<String>>,
    cap: usize,
) {
    let Some(stream) = stream else { return };
    let mut chunk = vec![0u8; 16 * 1024];
    while let Ok(n) = stream.read(&mut chunk).await {
        if n == 0 {
            return;
        }
        let mut held = into.lock().unwrap_or_else(|e| e.into_inner());
        held.push_str(&String::from_utf8_lossy(&chunk[..n]));
        if held.len() > cap {
            let from = crate::boundary_at_or_after(&held, held.len() - cap);
            *held = held[from..].to_string();
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Reading and stopping what `run_command` left running.
///
/// Registered only where there is a registry to read, the way `ask` is
/// registered only where somebody can answer: advertising it otherwise spends a
/// schema on a tool that has nothing to say.
pub struct JobTool;

#[async_trait::async_trait]
impl crate::Tool for JobTool {
    fn name(&self) -> &str {
        "job"
    }

    fn spec(&self) -> rook_llm::ToolSpec {
        rook_llm::ToolSpec {
            name: "job".into(),
            description: "Read what a background command has printed, or stop it. No id lists them.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "stop": { "type": "boolean", "description": "Kill it; what it printed is kept." }
                }
            }),
        }
    }

    async fn call(&self, ctx: &crate::ToolContext, args: &serde_json::Value) -> Result<crate::ToolOutcome> {
        let Some(jobs) = &ctx.jobs else {
            return Ok(crate::ToolOutcome::error("nothing is running in the background".to_string()));
        };
        let Some(id) = args.get("id").and_then(|i| i.as_str()) else {
            let listed: Vec<String> = jobs.list().iter().map(describe).collect();
            return Ok(match listed.is_empty() {
                true => crate::ToolOutcome::ok("no background commands".to_string()),
                false => crate::ToolOutcome::ok(listed.join("\n")),
            });
        };
        if args.get("stop").and_then(|s| s.as_bool()).unwrap_or(false) && !jobs.stop(id) {
            return Ok(crate::ToolOutcome::error(format!("{id} is not running")));
        }
        match jobs.get(id) {
            Some(job) => Ok(crate::ToolOutcome::ok(format!("{}\n{}", describe(&job), job.output))
                .with("running", job.exit_code.is_none())),
            None => Ok(crate::ToolOutcome::error(format!("no background command {id}"))),
        }
    }
}

fn describe(job: &Job) -> String {
    let state = match job.exit_code {
        Some(code) => format!("exit {code}"),
        None => format!("running for {}s", now().saturating_sub(job.started_at)),
    };
    format!("{} [{state}] {}", job.id, job.command)
}
