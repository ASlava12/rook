//! Opt-in process-local timings. Elapsed waits are observations, not causal blame.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub interval_ms: u64,
    pub slow_ms: u64,
    pub max_samples: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, interval_ms: 1000, slow_ms: 100, max_samples: 8 }
    }
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !(50..=10000).contains(&self.interval_ms) {
            errors.push("telemetry.runtime_diagnostics.interval_ms: expected 50..=10000".into());
        }
        if !(1..=60000).contains(&self.slow_ms) {
            errors.push("telemetry.runtime_diagnostics.slow_ms: expected 1..=60000".into());
        }
        if !(1..=128).contains(&self.max_samples) {
            errors.push("telemetry.runtime_diagnostics.max_samples: expected 1..=128".into());
        }
        errors
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Scheduler,
    HistoryRead,
    ModelSelection,
    Maintenance,
    TurnAdmission,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    SchedulingLag,
    BlockingQueue,
    BlockingElapsed,
    EngineLockWait,
    AdmissionWait,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub count: u64,
    pub last_ms: u64,
    pub max_ms: u64,
    pub total_ms: u64,
    pub slow: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub operation: Operation,
    pub metric: Metric,
    pub elapsed_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Aggregate {
    pub operation: Operation,
    pub metric: Metric,
    pub stats: Stats,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub slow_ms: u64,
    pub interval_ms: u64,
    pub aggregates: Vec<Aggregate>,
    pub recent: Vec<Sample>,
    pub omitted_samples: u64,
    pub coalesced_probes: u64,
}
#[derive(Default)]
struct Data {
    stats: BTreeMap<(Operation, Metric), Stats>,
    recent: VecDeque<Sample>,
    omitted: u64,
    coalesced: u64,
}
struct State {
    settings: Settings,
    data: Mutex<Data>,
    enabled: AtomicBool,
    pending: AtomicBool,
}
impl State {
    fn record(&self, operation: Operation, metric: Metric, elapsed: Duration) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let stats = data.stats.entry((operation, metric)).or_default();
        stats.count = stats.count.saturating_add(1);
        stats.last_ms = ms;
        stats.max_ms = stats.max_ms.max(ms);
        stats.total_ms = stats.total_ms.saturating_add(ms);
        if ms >= self.settings.slow_ms {
            stats.slow = stats.slow.saturating_add(1);
            if data.recent.len() == self.settings.max_samples {
                data.recent.pop_front();
                data.omitted = data.omitted.saturating_add(1);
            }
            data.recent.push_back(Sample { operation, metric, elapsed_ms: ms });
        }
    }
    fn snapshot(&self) -> Snapshot {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        Snapshot {
            slow_ms: self.settings.slow_ms,
            interval_ms: self.settings.interval_ms,
            aggregates: data
                .stats
                .iter()
                .map(|(&(operation, metric), stats)| Aggregate { operation, metric, stats: stats.clone() })
                .collect(),
            recent: data.recent.iter().cloned().collect(),
            omitted_samples: data.omitted,
            coalesced_probes: data.coalesced,
        }
    }
}
static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<Arc<State>>> = Mutex::new(None);

pub struct Sampler {
    state: Arc<State>,
    stop: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}
/// Start once at daemon startup. Disabled is cheap and opens no thread or writer.
pub fn start(settings: &Settings) -> crate::Result<Option<Sampler>> {
    if !settings.enabled {
        return Ok(None);
    }
    if !settings.errors().is_empty() {
        return Err(crate::CoreError::Other(settings.errors().join("; ")));
    }
    let handle = tokio::runtime::Handle::try_current().map_err(|e| crate::CoreError::Other(e.to_string()))?;
    let state = Arc::new(State {
        settings: settings.clone(),
        data: Mutex::new(Data::default()),
        enabled: AtomicBool::new(true),
        pending: AtomicBool::new(false),
    });
    let (stop, receiver) = mpsc::channel();
    let thread_state = state.clone();
    let interval = Duration::from_millis(settings.interval_ms);
    let worker = std::thread::Builder::new()
        .name("rook-runtime-probe".into())
        .spawn(move || {
            while matches!(receiver.recv_timeout(interval), Err(mpsc::RecvTimeoutError::Timeout)) {
                if thread_state.pending.swap(true, Ordering::AcqRel) {
                    let mut data = thread_state.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.coalesced = data.coalesced.saturating_add(1);
                    continue;
                }
                let queued = Instant::now();
                let state = thread_state.clone();
                // At most one queued probe, even when every Tokio worker is occupied.
                handle.spawn(async move {
                    state.record(Operation::Scheduler, Metric::SchedulingLag, queued.elapsed());
                    state.pending.store(false, Ordering::Release);
                });
            }
        })
        .map_err(|e| crate::CoreError::Other(e.to_string()))?;
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
    ACTIVE.store(true, Ordering::Release);
    Ok(Some(Sampler { state, stop, worker: Some(worker) }))
}
impl Drop for Sampler {
    fn drop(&mut self) {
        self.state.enabled.store(false, Ordering::Release);
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.state)) {
            *state = None;
            ACTIVE.store(false, Ordering::Release);
        }
    }
}
pub struct Queued {
    state: Arc<State>,
    operation: Operation,
    since: Instant,
}
pub struct Running {
    state: Arc<State>,
    operation: Operation,
    since: Instant,
}
/// Call before enqueueing blocking work; no request identifiers or payloads enter the recorder.
pub fn queued(operation: Operation) -> Option<Queued> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone().map(|state| Queued {
        state,
        operation,
        since: Instant::now(),
    })
}
impl Queued {
    pub fn waited(self, metric: Metric) {
        self.state.record(self.operation, metric, self.since.elapsed());
    }
    pub fn start(self) -> Running {
        self.state.record(self.operation, Metric::BlockingQueue, self.since.elapsed());
        Running { state: self.state, operation: self.operation, since: Instant::now() }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.state.record(self.operation, Metric::BlockingElapsed, self.since.elapsed());
    }
}
pub(crate) fn snapshot() -> Option<Snapshot> {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|state| state.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_probe_observes_a_blocked_executor_without_accumulating_queued_tasks() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let sampler =
                start(&Settings { enabled: true, interval_ms: 50, slow_ms: 20, ..Default::default() })
                    .unwrap()
                    .unwrap();
            // This deliberately blocks the sole executor, not the independent probe thread.
            std::thread::sleep(Duration::from_millis(220));
            tokio::time::sleep(Duration::from_millis(10)).await;
            let observed = sampler.state.snapshot();
            let scheduler = observed.aggregates.iter().find(|a| a.metric == Metric::SchedulingLag).unwrap();
            assert!(scheduler.stats.max_ms >= 20, "{observed:?}");
            assert!(observed.coalesced_probes > 0, "only one probe may queue while the executor is blocked");
            assert!(!sampler.state.pending.load(Ordering::Acquire));
            drop(sampler);
        });
    }
    #[test]
    fn slow_observations_have_fixed_labels_and_a_reached_storage_bound() {
        let state = State {
            settings: Settings { max_samples: 2, slow_ms: 10, ..Default::default() },
            data: Mutex::new(Data::default()),
            enabled: AtomicBool::new(true),
            pending: AtomicBool::new(false),
        };
        state.record(Operation::Scheduler, Metric::SchedulingLag, Duration::from_millis(1));
        for _ in 0..5 {
            state.record(Operation::HistoryRead, Metric::BlockingElapsed, Duration::from_millis(30));
        }
        let snapshot = state.snapshot();
        assert_eq!(snapshot.recent.len(), 2);
        assert_eq!(snapshot.omitted_samples, 3);
        assert_eq!(snapshot.aggregates.iter().map(|a| a.stats.count).sum::<u64>(), 6);
        assert_eq!(snapshot.aggregates.iter().map(|a| a.stats.slow).sum::<u64>(), 5);
        state.enabled.store(false, Ordering::Release);
        state.record(Operation::ModelSelection, Metric::BlockingQueue, Duration::from_secs(1));
        assert_eq!(state.snapshot().aggregates.len(), 2);
    }
}
