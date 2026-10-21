//! A retry keeps its controls and watchdog, but need not reserve an executor.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use crate::AppState;

#[derive(Default)]
pub(super) struct Slot {
    state: Weak<AppState>,
    released: AtomicBool,
}

impl Slot {
    pub(super) fn new(state: &Arc<AppState>) -> Self {
        Self { state: Arc::downgrade(state), released: AtomicBool::new(false) }
    }

    pub(super) fn held(&self) -> bool {
        !self.released.load(Ordering::Acquire)
    }

    pub(super) fn release(&self) {
        // Freeing a slot can only make an admission conservative if it races
        // the count. Claiming one again is serialized with every other claimant.
        self.released.store(true, Ordering::Release);
    }

    /// Call without an engine read guard: admission takes the work lock before
    /// reading configuration, and an inverted order can deadlock on a reload.
    pub(super) async fn acquire(&self) -> Result<bool, String> {
        if self.held() {
            return Ok(true);
        }
        let state = self.state.upgrade().ok_or("daemon admission owner has stopped")?;
        let tasks = state.work.0.lock().await;
        let cap = state.rook.read().await.config.work.max_parallel_runs;
        let legacy = tasks.values().filter(|running| !running.is_finished()).count();
        if crate::work::occupied(&state, legacy).await >= cap {
            return Ok(false);
        }
        self.released.store(false, Ordering::Release);
        Ok(true)
    }
}
