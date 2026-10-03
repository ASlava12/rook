//! Display hints coalesce; tool receipts and child outcomes use other paths.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const MAX_TEXT_BYTES: usize = 2048;

struct State {
    pending: VecDeque<(usize, String)>,
    limit: usize,
    closed: bool,
    #[cfg(test)]
    high_water: usize,
}

#[derive(Clone)]
pub(super) struct Sender {
    state: Arc<Mutex<State>>,
    wake: mpsc::Sender<()>,
}

pub(super) struct Receiver {
    state: Arc<Mutex<State>>,
    wake: mpsc::Receiver<()>,
}

pub(super) fn channel(limit: usize) -> (Sender, Receiver) {
    let state = Arc::new(Mutex::new(State {
        pending: VecDeque::new(),
        limit: limit.clamp(1, 128),
        closed: false,
        #[cfg(test)]
        high_water: 0,
    }));
    // A signal owns no producer text. Tokio also supplies sender closure and
    // cancellation-safe wakeups without a second queue of display messages.
    let (send, recv) = mpsc::channel(1);
    (Sender { state: state.clone(), wake: send }, Receiver { state, wake: recv })
}

impl Sender {
    pub(super) fn send(&self, at: usize, text: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        let position = state.pending.iter().position(|(child, _)| *child == at);
        if let Some(position) = position {
            state.pending.remove(position);
        } else if state.pending.len() == state.limit {
            state.pending.pop_front();
        }
        let mut end = text.len().min(MAX_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let text = text.get(..end).unwrap_or_default().to_owned();
        // Admit the slot before copying, keeping its place so one busy child
        // cannot postpone another child's hint.
        let position = position.unwrap_or(state.pending.len());
        state.pending.insert(position, (at, text));
        #[cfg(test)]
        {
            state.high_water = state.high_water.max(state.pending.len());
        }
        drop(state);
        let _ = self.wake.try_send(());
    }
}

impl Receiver {
    pub(super) fn try_recv(&mut self) -> Result<(usize, String), ()> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).pending.pop_front().ok_or(())
    }

    pub(super) async fn recv(&mut self) -> Option<(usize, String)> {
        loop {
            if let Ok(update) = self.try_recv() {
                return Some(update);
            }
            self.wake.recv().await?;
        }
    }

    #[cfg(test)]
    pub(super) fn high_water(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).high_water
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_and_defensive_limits_bound_actual_child_cardinality() {
        let mut config = crate::Config::default();
        assert_eq!(config.agent.delegation_progress_entries, 32);
        for limit in [0, 129] {
            config.agent.delegation_progress_entries = limit;
            assert!(config.validation_errors().iter().any(|e| e.contains("delegation_progress_entries")));
        }
        let (send, recv) = channel(usize::MAX);
        for child in 0..129 {
            send.send(child, "hint");
        }
        assert_eq!(recv.high_water(), 128, "invalid direct configuration still has a reached ceiling");
    }

    #[tokio::test]
    async fn a_slow_reader_keeps_latest_bounded_hints_and_drains_before_closure() {
        let (send, mut recv) = channel(2);
        for child in 0..3 {
            for step in 0..256 {
                send.send(child, &format!("{child}/{step}"));
            }
        }
        assert_eq!(recv.high_water(), 2, "three children actually exceed the two-slot limit");
        send.send(1, "latest child one");
        drop(send);
        assert_eq!(recv.recv().await, Some((1, "latest child one".into())));
        assert_eq!(recv.recv().await, Some((2, "2/255".into())));
        assert_eq!(recv.recv().await, None);
    }

    #[tokio::test]
    async fn cancellation_and_unicode_admission_do_not_lose_the_next_hint() {
        let (send, mut recv) = channel(0);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(1), recv.recv()).await.is_err());
        let text = "€".repeat(4096);
        assert!(text.len() > MAX_TEXT_BYTES);
        assert!(
            !text.is_char_boundary(MAX_TEXT_BYTES),
            "admission actually cuts inside a multibyte character"
        );
        send.send(7, &text);
        let (_, bounded) = recv.recv().await.unwrap();
        assert_eq!(bounded.len(), MAX_TEXT_BYTES - MAX_TEXT_BYTES % "€".len());
        assert!(bounded.capacity() <= MAX_TEXT_BYTES);
        send.send(8, "next");
        assert_eq!(recv.high_water(), 1);
        assert_eq!(recv.recv().await, Some((8, "next".into())));
        drop(recv);
        for _ in 0..256 {
            send.send(9, &text);
        }
        assert!(send.state.lock().unwrap().pending.is_empty());
    }
}
