//! Per-window backpressure, including encoded bytes held by the socket writer.
use std::io::Write;
use std::sync::Arc;

use rook_proto::ChatEvent;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

#[derive(Clone)]
pub(super) struct Sender {
    frames: mpsc::Sender<Frame>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    limit: usize,
    failed: watch::Sender<bool>,
}

pub(super) struct Receiver {
    frames: mpsc::Receiver<Frame>,
    failed: watch::Receiver<bool>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

pub(super) struct Frame {
    pub(super) text: String,
    // Permits stay with the frame until the socket finishes sending it.
    _slot: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

pub(super) fn channel(events: usize, bytes: usize) -> (Sender, Receiver) {
    // Also guard hand-written configurations that bypass offline validation.
    let events = events.clamp(1, 4096);
    let limit = bytes.clamp(4096, 32 * 1024 * 1024);
    let slots = Arc::new(Semaphore::new(events));
    let bytes = Arc::new(Semaphore::new(limit));
    let (frames, incoming) = mpsc::channel(events);
    let (failed, failure) = watch::channel(false);
    (
        Sender { frames, slots: slots.clone(), bytes: bytes.clone(), limit, failed },
        Receiver { frames: incoming, failed: failure, slots, bytes },
    )
}

impl Sender {
    pub(super) async fn send(&self, event: ChatEvent) -> Result<(), ()> {
        let slot = self.slots.clone().acquire_owned().await.map_err(|_| ())?;
        // Count escaped JSON without allocating it; byte admission precedes
        // encoding, so waiting producers cannot accumulate encoded frames.
        let mut count = Counter { remaining: self.limit, used: 0 };
        if serde_json::to_writer(&mut count, &event).is_err() {
            // A partial event is not an approval or a terminal result. Close
            // this view instead of silently dropping an oversized event.
            self.failed.send_replace(true);
            self.slots.close();
            self.bytes.close();
            return Err(());
        }
        let bytes = self.bytes.clone().acquire_many_owned(count.used as u32).await.map_err(|_| ())?;
        let mut encoded = Vec::with_capacity(count.used);
        serde_json::to_writer(&mut encoded, &event).map_err(|_| ())?;
        let text = String::from_utf8(encoded).map_err(|_| ())?;
        self.frames.send(Frame { text, _slot: slot, _bytes: bytes }).await.map_err(|_| ())
    }
}

impl Receiver {
    pub(super) async fn recv(&mut self) -> Option<Frame> {
        if *self.failed.borrow() {
            return None;
        }
        tokio::select! {
            biased;
            Ok(()) = self.failed.changed() => None,
            frame = self.frames.recv() => frame,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // Wake producers waiting on permits when a disconnected writer leaves.
        self.slots.close();
        self.bytes.close();
    }
}

struct Counter {
    remaining: usize,
    used: usize,
}

impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other("chat event exceeds server.chat_queue_bytes"));
        }
        self.remaining -= bytes.len();
        self.used += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> ChatEvent {
        ChatEvent::Text { text: value.into() }
    }

    #[tokio::test]
    async fn in_flight_frames_count_toward_the_event_limit() {
        let (out, mut incoming) = channel(2, 4096);
        out.send(text("one")).await.unwrap();
        out.send(text("two")).await.unwrap();
        let sending = incoming.recv().await.unwrap();
        let third = out.send(text("three"));
        tokio::pin!(third);
        assert!(futures_util::poll!(&mut third).is_pending(), "the in-flight frame still owns a slot");
        drop(sending);
        third.await.unwrap();
    }

    #[tokio::test]
    async fn escaped_json_bytes_apply_backpressure_before_encoding() {
        let (out, mut incoming) = channel(8, 4096);
        let event = text(&"\n".repeat(1100));
        let size = serde_json::to_vec(&event).unwrap().len();
        assert!(size * 2 > 4096 && size < 4096, "two frames exceed the configured byte budget");
        out.send(event.clone()).await.unwrap();
        let second = out.send(event);
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        // Tokio reserves the remaining permits for the FIFO waiter, but does
        // not admit its encoding until the whole requested amount is free.
        assert_eq!(out.bytes.available_permits(), 0);
        let in_flight = incoming.recv().await.unwrap();
        assert!(futures_util::poll!(&mut second).is_pending());
        drop(in_flight);
        second.await.unwrap();
        assert_eq!(out.bytes.available_permits(), 4096 - size);
    }

    #[tokio::test]
    async fn an_oversized_event_closes_the_view_instead_of_truncating_it() {
        let (out, mut incoming) = channel(8, 4096);
        assert!(out.send(text(&"x".repeat(4096))).await.is_err());
        assert!(incoming.recv().await.is_none());
        assert!(out.send(text("later")).await.is_err());
    }

    #[tokio::test]
    async fn a_disconnected_receiver_releases_waiting_producers() {
        let (out, incoming) = channel(1, 4096);
        out.send(text("one")).await.unwrap();
        let second = out.send(text("two"));
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        drop(incoming);
        assert!(second.await.is_err());
    }

    #[tokio::test]
    async fn dropping_the_last_sender_drains_queued_events() {
        let (out, mut incoming) = channel(2, 4096);
        out.send(ChatEvent::Cancelled).await.unwrap();
        drop(out);
        assert!(incoming.recv().await.unwrap().text.contains("cancelled"));
        assert!(incoming.recv().await.is_none());
    }
}
