use rook_proto::{SecretPrompt, SecretStatus};
use rook_tools::{
    pending::Limits,
    private_input::{PrivateInput, SecretSink},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;

#[derive(Default)]
struct Sink {
    saves: AtomicUsize,
    closes: AtomicUsize,
}
impl SecretSink for Sink {
    fn save(&self, value: &str) -> SecretStatus {
        assert_eq!(value, "private-test-value");
        self.saves.fetch_add(1, Ordering::SeqCst);
        SecretStatus::Saved
    }
    fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}
fn prompt() -> SecretPrompt {
    SecretPrompt {
        id: String::new(),
        session: "owner".into(),
        name: "deploy".into(),
        reason: "Deploy the requested change".into(),
    }
}
fn start(input: Arc<PrivateInput>, sink: Arc<Sink>) -> tokio::task::JoinHandle<SecretStatus> {
    tokio::spawn(async move { input.request(prompt(), sink).await })
}

#[tokio::test]
async fn private_responses_bind_the_owner_and_are_claimed_once() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let input = Arc::new(PrivateInput::new(tx, Duration::from_secs(2), Limits::default()));
    let sink = Arc::new(Sink::default());
    let request = start(input.clone(), sink.clone());
    let prompt = rx.recv().await.unwrap();
    assert!(input.accepts(&prompt.id, "owner"));
    assert!(!input.accepts(&prompt.id, "other"));
    assert_eq!(input.respond(&prompt.id, "other", SecretStatus::Saved, Some("private-test-value")), None);
    assert_eq!(sink.saves.load(Ordering::SeqCst), 0);
    assert!(input.accepts(&prompt.id, "owner"));
    assert_eq!(
        input.respond(&prompt.id, "owner", SecretStatus::Saved, Some("private-test-value")),
        Some(SecretStatus::Saved)
    );
    assert_eq!(input.respond(&prompt.id, "owner", SecretStatus::Saved, Some("private-test-value")), None);
    assert_eq!(request.await.unwrap(), SecretStatus::Saved);
    assert_eq!(sink.saves.load(Ordering::SeqCst), 1);
    assert!(input.current().is_empty());
}

#[tokio::test]
async fn declined_skipped_expired_cancelled_and_disconnected_requests_cannot_save_later() {
    for outcome in
        [SecretStatus::Declined, SecretStatus::Skipped, SecretStatus::Expired, SecretStatus::Cancelled]
    {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let input = Arc::new(PrivateInput::new(tx, Duration::from_millis(30), Limits::default()));
        let sink = Arc::new(Sink::default());
        let request = start(input.clone(), sink.clone());
        let prompt = rx.recv().await.unwrap();
        match outcome {
            SecretStatus::Expired => assert_eq!(request.await.unwrap(), outcome),
            SecretStatus::Cancelled => {
                request.abort();
                assert!(request.await.unwrap_err().is_cancelled());
            }
            _ => {
                assert_eq!(input.respond(&prompt.id, "owner", outcome, None), Some(outcome));
                assert_eq!(request.await.unwrap(), outcome);
            }
        }
        assert_eq!(input.respond(&prompt.id, "owner", SecretStatus::Saved, Some("private-test-value")), None);
        assert_eq!(sink.saves.load(Ordering::SeqCst), 0);
        assert!(sink.closes.load(Ordering::SeqCst) > 0);
        assert!(input.current().is_empty());
    }
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let input = Arc::new(PrivateInput::new(tx, Duration::from_secs(2), Limits::default()));
    let sink = Arc::new(Sink::default());
    assert_eq!(start(input, sink.clone()).await.unwrap(), SecretStatus::Unavailable);
    assert_eq!(sink.saves.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn capacity_and_invalid_values_do_not_mutate_a_waiting_request() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let input = Arc::new(PrivateInput::new(
        tx,
        Duration::from_secs(2),
        Limits { max_requests: 1, ..Limits::default() },
    ));
    let sink = Arc::new(Sink::default());
    let first = start(input.clone(), sink.clone());
    let prompt = rx.recv().await.unwrap();
    assert_eq!(input.current().len(), 1, "the configured capacity is reached");
    assert_eq!(start(input.clone(), sink.clone()).await.unwrap(), SecretStatus::Busy);
    for value in [String::new(), "x".repeat(8193)] {
        assert_eq!(input.respond(&prompt.id, "owner", SecretStatus::Saved, Some(&value)), None);
    }
    assert_eq!(input.respond(&prompt.id, "owner", SecretStatus::Declined, Some("private-test-value")), None);
    assert!(input.accepts(&prompt.id, "owner"));
    assert_eq!(input.respond(&prompt.id, "owner", SecretStatus::Skipped, None), Some(SecretStatus::Skipped));
    assert_eq!(first.await.unwrap(), SecretStatus::Skipped);
    assert_eq!(sink.saves.load(Ordering::SeqCst), 0);
}
