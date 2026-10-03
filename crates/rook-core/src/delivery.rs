//! Shared chat transport admission; leases survive forwarding until a view consumes the frame.
use std::io::Write;
use std::sync::Arc;

use rook_proto::ChatEvent;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

#[derive(Debug, thiserror::Error)]
#[error("chat delivery closed or frame exceeds server.chat_queue_bytes")]
pub struct Closed;

#[derive(Clone)]
pub struct Sender {
    frames: mpsc::Sender<Frame>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    limit: usize,
    failed: watch::Sender<bool>,
}

pub struct Receiver {
    frames: mpsc::Receiver<Frame>,
    failed: watch::Receiver<bool>,
    slots: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

pub struct Frame {
    pub text: String,
    // Permits stay with the frame until the socket finishes sending it.
    _slot: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

pub fn channel(events: usize, bytes: usize) -> (Sender, Receiver) {
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
    pub fn byte_limit(&self) -> usize {
        self.limit
    }

    /// A synchronous observer cannot await capacity while the runtime is
    /// borrowing it. Close that connection explicitly instead of losing a
    /// lifecycle/permission update or retaining an unbounded second queue.
    pub fn try_send_serialized(&self, value: &impl serde::Serialize) -> Result<(), Closed> {
        let admitted = (|| {
            let slot = self.slots.clone().try_acquire_owned().map_err(|_| Closed)?;
            let size = encoded_size(value, self.limit).ok_or(Closed)?;
            let bytes = self.bytes.clone().try_acquire_many_owned(size as u32).map_err(|_| Closed)?;
            let mut encoded = Vec::with_capacity(size);
            serde_json::to_writer(&mut encoded, value).map_err(|_| Closed)?;
            let text = String::from_utf8(encoded).map_err(|_| Closed)?;
            self.frames.try_send(Frame { text, _slot: slot, _bytes: bytes }).map_err(|_| Closed)
        })();
        if admitted.is_err() {
            self.failed.send_replace(true);
            self.slots.close();
            self.bytes.close();
        }
        admitted
    }

    /// Admission precedes copying a received frame. The websocket separately
    /// caps its one incoming message at this same byte limit.
    pub async fn send_text(&self, text: &str) -> Result<(), Closed> {
        let slot = self.slots.clone().acquire_owned().await.map_err(|_| Closed)?;
        if text.len() > self.limit {
            self.failed.send_replace(true);
            self.slots.close();
            self.bytes.close();
            return Err(Closed);
        }
        let bytes = self.bytes.clone().acquire_many_owned(text.len() as u32).await.map_err(|_| Closed)?;
        self.frames
            .send(Frame { text: text.to_owned(), _slot: slot, _bytes: bytes })
            .await
            .map_err(|_| Closed)
    }

    pub async fn send(&self, event: ChatEvent) -> Result<(), Closed> {
        let slot = self.slots.clone().acquire_owned().await.map_err(|_| Closed)?;
        // Count escaped JSON without allocating it; byte admission precedes
        // encoding, so waiting producers cannot accumulate encoded frames.
        let Some(size) = encoded_size(&event, self.limit) else {
            // A partial event is not an approval or a terminal result. Close
            // this view instead of silently dropping an oversized event.
            self.failed.send_replace(true);
            self.slots.close();
            self.bytes.close();
            return Err(Closed);
        };
        let bytes = self.bytes.clone().acquire_many_owned(size as u32).await.map_err(|_| Closed)?;
        let mut encoded = Vec::with_capacity(size);
        serde_json::to_writer(&mut encoded, &event).map_err(|_| Closed)?;
        let text = String::from_utf8(encoded).map_err(|_| Closed)?;
        self.frames.send(Frame { text, _slot: slot, _bytes: bytes }).await.map_err(|_| Closed)
    }
}

impl Receiver {
    pub async fn recv(&mut self) -> Option<Frame> {
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

pub fn encoded_size(event: &impl serde::Serialize, limit: usize) -> Option<usize> {
    let mut count = Counter { remaining: limit, used: 0 };
    serde_json::to_writer(&mut count, event).ok()?;
    Some(count.used)
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
    async fn synchronous_lifecycle_delivery_fails_explicitly_while_a_frame_is_in_flight() {
        let (out, mut incoming) = channel(1, 4096);
        out.try_send_serialized(&serde_json::json!({"status":"in_progress"})).unwrap();
        let writing = incoming.recv().await.unwrap();
        assert_eq!(out.slots.available_permits(), 0, "the writer still owns the sole slot");
        assert!(out.try_send_serialized(&serde_json::json!({"status":"completed"})).is_err());
        drop(writing);
        assert!(incoming.recv().await.is_none(), "a missing lifecycle event closes the view");
        assert!(out.try_send_serialized(&serde_json::json!({"status":"later"})).is_err());
    }

    #[tokio::test]
    async fn synchronous_json_admission_counts_escaping_and_in_flight_bytes() {
        let (out, mut incoming) = channel(8, 4096);
        let event = serde_json::json!({"text":"\n".repeat(1100)});
        let size = serde_json::to_vec(&event).unwrap().len();
        assert!(size < 4096 && size * 2 > 4096);
        out.try_send_serialized(&event).unwrap();
        let writing = incoming.recv().await.unwrap();
        assert_eq!(out.bytes.available_permits(), 4096 - size);
        assert!(out.try_send_serialized(&event).is_err());
        drop(writing);
        assert!(incoming.recv().await.is_none());

        let (out, mut incoming) = channel(8, 4096);
        let oversized = serde_json::json!({"text":"\0".repeat(800)});
        assert!(serde_json::to_vec(&oversized).unwrap().len() > 4096);
        assert!(out.try_send_serialized(&oversized).is_err());
        assert_eq!(out.bytes.available_permits(), 4096, "encoding was never admitted");
        assert!(incoming.recv().await.is_none());
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

    #[tokio::test]
    async fn forwarding_a_frame_keeps_its_admission_until_the_view_consumes_it() {
        let (send, mut receive) = channel(1, 4096);
        send.send_text("first").await.unwrap();
        let frame = receive.recv().await.unwrap();
        // The relay itself is unbounded, but cannot accumulate leased frames.
        let (relay, mut view) = mpsc::unbounded_channel();
        assert!(relay.send(frame).is_ok());
        let second = send.send_text("second");
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        let processing = view.recv().await.unwrap();
        assert!(futures_util::poll!(&mut second).is_pending());
        drop(processing);
        second.await.unwrap();
    }

    #[tokio::test]
    async fn received_text_obeys_the_shared_byte_budget_before_copying() {
        let (send, mut receive) = channel(8, 4096);
        let text = "x".repeat(3000);
        assert!(text.len() * 2 > 4096);
        send.send_text(&text).await.unwrap();
        let next = send.send_text(&text);
        tokio::pin!(next);
        assert!(futures_util::poll!(&mut next).is_pending());
        let processing = receive.recv().await.unwrap();
        assert!(futures_util::poll!(&mut next).is_pending());
        drop(processing);
        next.await.unwrap();
    }
}
