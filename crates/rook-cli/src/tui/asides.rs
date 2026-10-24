//! One auxiliary question per window, with no foreground completion events.
use super::*;
use futures_util::FutureExt;
use rook_proto::aside::Reply;

struct Pending {
    session: u128,
    worker: tokio::task::JoinHandle<Result<Reply>>,
}

#[derive(Default)]
pub(super) struct Asides {
    pending: Option<Pending>,
}

impl Drop for Asides {
    fn drop(&mut self) {
        self.close();
    }
}

impl Asides {
    pub(super) fn close(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.worker.abort();
        }
    }

    pub(super) fn submit(
        &mut self,
        source: &crate::source::Source,
        runtime: &tokio::runtime::Runtime,
        session: Option<u128>,
        request: rook_proto::aside::Request,
    ) -> Result<u128> {
        anyhow::ensure!(self.pending.is_none(), "a side question is already running; your draft is kept");
        request.validate().map_err(anyhow::Error::msg)?;
        let local = source.here().cloned();
        let base = source.daemon_base().map(str::to_owned);
        let session = match session {
            Some(session) => session,
            None => local
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("open a conversation before asking /btw; your draft is kept"))?
                .start_session("")?,
        };
        let worker = runtime.spawn(async move {
            match local {
                Some(rook) => Ok(rook_core::asides::reply(&rook, session, request).await),
                None => {
                    remote(
                        base.as_deref()
                            .ok_or_else(|| anyhow::anyhow!("side question backend unavailable"))?,
                        session,
                        request,
                    )
                    .await
                }
            }
        });
        self.pending = Some(Pending { session, worker });
        Ok(session)
    }

    pub(super) fn poll(&mut self, session: Option<u128>) -> Option<Reply> {
        let pending = self.pending.as_ref()?;
        if session != Some(pending.session) {
            self.close();
            return None;
        }
        if !pending.worker.is_finished() {
            return None;
        }
        let pending = self.pending.take()?;
        let result = match pending.worker.now_or_never() {
            Some(Ok(result)) => result,
            _ => Err(anyhow::anyhow!("side question stopped; the foreground turn is unchanged")),
        };
        Some(result.unwrap_or_else(|error| Reply::Failed { question: None, error: error.to_string() }))
    }
}

async fn remote(base: &str, session: u128, request: rook_proto::aside::Request) -> Result<Reply> {
    rook_llm::init_tls();
    let client =
        reqwest::Client::builder().timeout(rook_core::asides::TIMEOUT + Duration::from_secs(10)).build()?;
    let mut response = client
        .post(format!("{base}/api/sessions/{}/aside", rook_store::format_session_id(session)))
        .header("X-Rook-Api", rook_proto::API_VERSION)
        .json(&request)
        .send()
        .await?;
    anyhow::ensure!(
        response.status() != reqwest::StatusCode::NOT_FOUND,
        "rookd does not support this side question or session; check the session and daemon version"
    );
    anyhow::ensure!(
        response.status().is_success(),
        "rookd refused the side question ({})",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            chunk.len() <= rook_proto::aside::MAX_REPLY_BYTES.saturating_sub(bytes.len()),
            "side question reply exceeds 256 KiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

impl App {
    pub(super) fn send_aside(&mut self, question: String) {
        if self.chat.session.is_none() && self.chat.busy {
            self.chat
                .push("err", "Wait for the starting conversation before asking /btw; your draft is kept.");
            return;
        }
        let request = rook_proto::aside::Request { question, model: self.shared.model.borrow().clone() };
        match self.asides.submit(&self.source, &self.runtime, self.chat.session, request) {
            Ok(session) => {
                if self.chat.session != Some(session) {
                    self.chat.session = Some(session);
                    self.refresh_extension_ui();
                }
                let prompt = self.chat.input.take();
                if self.chat.history.last() != Some(&prompt) {
                    self.chat.history.push(prompt.clone());
                    remember_prompt(&prompt);
                }
                self.chat.recalled = None;
                self.chat.push("stat", "Side question submitted; it waits if the selected provider is busy.");
            }
            Err(error) => self.chat.push("err", &error.to_string()),
        }
    }

    pub(super) fn poll_asides(&mut self) {
        if let Some(reply) = self.asides.poll(self.chat.session) {
            let (question, kind, body) = match reply {
                Reply::Complete { question, answer } => (question, "stat", format!("A: {answer}")),
                Reply::Failed { question, error } => (question, "err", error),
            };
            // Only the backend knows all values registered by its provider.
            // A transport failure or older backend supplies no safe question;
            // never fall back to echoing the original request.
            let heading = question.map(|q| format!("[aside]\nQ: {q}")).unwrap_or_else(|| "[aside]".into());
            self.chat.push(kind, &format!("{heading}\n{body}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_side_question_accepts_only_bounded_supported_replies_and_never_uses_the_chat_route() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let session = 42;
        let cases = [
            ("200 OK", br#"{"status":"complete","answer":"OWNED_REMOTE_ANSWER"}"#.to_vec(), None),
            (
                "200 OK",
                br#"{"status":"failed","error":"OWNED_REMOTE_FAILURE"}"#.to_vec(),
                Some("OWNED_REMOTE_FAILURE"),
            ),
            ("404 Not Found", b"unsupported".to_vec(), Some("does not support")),
            ("503 Service Unavailable", b"busy".to_vec(), Some("refused the side question")),
            ("200 OK", b"{".to_vec(), Some("EOF")),
            ("200 OK", vec![b'x'; rook_proto::aside::MAX_REPLY_BYTES + 1], Some("exceeds 256 KiB")),
        ];
        for (status, reply, expected_error) in cases {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let (header_end, length) = loop {
                    let mut bytes = [0; 1024];
                    let size = socket.read(&mut bytes).await.unwrap();
                    assert_ne!(size, 0);
                    request.extend_from_slice(&bytes[..size]);
                    assert!(request.len() < 65536);
                    if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                        assert!(headers.contains("x-rook-api:"));
                        break (end + 4, length);
                    }
                };
                while request.len() < header_end + length {
                    let mut bytes = [0; 1024];
                    let size = socket.read(&mut bytes).await.unwrap();
                    assert_ne!(size, 0);
                    request.extend_from_slice(&bytes[..size]);
                    assert!(request.len() < 65536);
                }
                let start = format!(
                    "POST /api/sessions/{}/aside HTTP/1.1\r\n",
                    rook_store::format_session_id(session)
                );
                assert!(request.starts_with(start.as_bytes()));
                let body: rook_proto::aside::Request =
                    serde_json::from_slice(&request[header_end..]).unwrap();
                assert_eq!(body.question, "OWNED_REMOTE_QUESTION");
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                    reply.len()
                );
                socket.write_all(headers.as_bytes()).await.unwrap();
                let sent = socket.write_all(&reply).await;
                if reply.len() <= rook_proto::aside::MAX_REPLY_BYTES {
                    sent.unwrap();
                }
            });
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                remote(
                    &format!("http://{address}"),
                    session,
                    rook_proto::aside::Request { question: "OWNED_REMOTE_QUESTION".into(), model: None },
                ),
            )
            .await;
            if result.is_err() {
                server.abort();
            }
            let result = result.expect("owned remote request must finish");
            let result = result.and_then(|reply| match reply {
                Reply::Complete { answer, .. } => Ok(answer),
                Reply::Failed { error, .. } => Err(anyhow::anyhow!(error)),
            });
            tokio::time::timeout(Duration::from_secs(5), server).await.unwrap().unwrap();
            match expected_error {
                Some(expected) => assert!(result.unwrap_err().to_string().contains(expected), "{expected}"),
                None => assert_eq!(result.unwrap(), "OWNED_REMOTE_ANSWER"),
            }
        }
    }

    fn app() -> (tempfile::TempDir, App, u128) {
        let home = tempfile::tempdir().unwrap();
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(home.path().join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            home.path().into(),
        );
        let session = rook.start_session("owned side question").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut app = App::new(crate::source::Source::Local(rook.into()), runtime, true);
        app.chat.session = Some(session);
        app.chat.busy = true;
        (home, app, session)
    }

    #[test]
    fn second_side_question_keeps_its_draft_and_foreground_identity() {
        let (_home, mut app, session) = app();
        let worker = app.runtime.spawn(std::future::pending());
        app.asides.pending = Some(Pending { session, worker });
        app.chat.input.set("/btw second");
        app.send();
        assert_eq!(app.chat.input.as_str(), "/btw second");
        assert!(app.chat.busy && app.chat.session == Some(session));
        assert!(app.chat.log.iter().any(|(_, line)| line.contains("already running")));
    }

    #[test]
    fn first_local_side_question_creates_a_conversation_without_starting_a_foreground_turn() {
        let (_home, mut app, old_session) = app();
        let before = app.source.sessions().unwrap().len();
        app.chat.session = None;
        app.chat.busy = false;
        app.chat.input.set("/btw Which port?");
        app.send();
        let session = app.chat.session.unwrap();
        assert_ne!(session, old_session);
        assert_eq!(app.source.sessions().unwrap().len(), before + 1);
        assert_eq!(app.asides.pending.as_ref().unwrap().session, session);
        assert!(!app.chat.busy && app.turn.is_none());
        assert!(app.chat.input.is_empty());
    }

    #[test]
    fn invalid_first_question_and_starting_foreground_keep_their_drafts_without_creating_a_session() {
        let (_home, mut app, _) = app();
        let before = app.source.sessions().unwrap().len();
        app.chat.session = None;
        app.chat.busy = false;
        app.chat.input.set("/btw");
        app.send();
        assert_eq!(app.chat.input.as_str(), "/btw");
        assert!(app.chat.session.is_none());
        app.chat.busy = true;
        app.chat.input.set("/btw Which port?");
        app.send();
        assert_eq!(app.chat.input.as_str(), "/btw Which port?");
        assert!(app.chat.session.is_none() && app.asides.pending.is_none());
        assert_eq!(app.source.sessions().unwrap().len(), before);
    }

    #[test]
    fn completing_a_side_question_never_finishes_the_foreground_turn_or_replaces_its_draft() {
        let (_home, mut app, session) = app();
        app.turn = Some(app.runtime.spawn(std::future::pending()));
        let foreground = app.turn.as_ref().unwrap().id();
        let worker = app.runtime.spawn(async {
            Ok(Reply::Complete { question: Some("which port?".into()), answer: "SIDE ANSWER".into() })
        });
        app.asides.pending = Some(Pending { session, worker });
        app.runtime.block_on(tokio::task::yield_now());
        app.chat.input.set("UNSENT FOREGROUND DRAFT");
        app.poll_asides();
        assert!(app.chat.busy && app.chat.session == Some(session));
        assert_eq!(app.turn.as_ref().unwrap().id(), foreground);
        assert!(!app.turn.as_ref().unwrap().is_finished());
        assert_eq!(app.chat.input.as_str(), "UNSENT FOREGROUND DRAFT");
        assert!(app.chat.log.iter().any(|(_, line)| line.contains("SIDE ANSWER")));
    }

    #[test]
    fn changing_session_cancels_only_the_owned_side_question_and_does_not_display_its_answer() {
        let (_home, mut app, session) = app();
        let worker = app.runtime.spawn(std::future::pending());
        let abort = worker.abort_handle();
        app.asides.pending = Some(Pending { session, worker });
        app.chat.session = Some(session.wrapping_add(1));
        app.poll_asides();
        app.runtime.block_on(tokio::task::yield_now());
        assert!(abort.is_finished());
        assert!(app.chat.busy);
        assert!(!app.chat.log.iter().any(|(_, line)| line.contains("OLD SESSION QUESTION")));
    }

    #[test]
    fn side_question_result_never_repeats_an_unmasked_pending_question() {
        let (_home, mut app, session) = app();
        let worker = app.runtime.spawn(async {
            Ok(Reply::Complete { question: Some("Which ${secret}?".into()), answer: "SIDE ANSWER".into() })
        });
        app.asides.pending = Some(Pending { session, worker });
        app.runtime.block_on(tokio::task::yield_now());
        app.poll_asides();
        assert!(app.chat.log.iter().any(|(_, line)| line.contains("SIDE ANSWER")));
        assert!(!app.chat.log.iter().any(|(_, line)| line.contains("owned-ui-question-key-52718364")));
    }

    #[test]
    fn failed_side_question_does_not_fill_an_empty_foreground_answer_field() {
        let (_home, mut app, session) = app();
        let worker = app.runtime.spawn(async { Err(anyhow::anyhow!("owned side failure")) });
        app.asides.pending = Some(Pending { session, worker });
        app.runtime.block_on(tokio::task::yield_now());
        app.poll_asides();
        assert!(app.chat.input.is_empty());
        assert!(app.chat.busy);
        assert!(app.chat.log.iter().any(|(_, line)| line.contains("owned side failure")));
        assert!(!app.chat.log.iter().any(|(_, line)| line.contains("Q:")));
    }

    #[test]
    fn failed_side_reply_uses_the_backend_masked_question() {
        let (_home, mut app, session) = app();
        let worker = app.runtime.spawn(async {
            Ok(Reply::Failed {
                question: Some("Which ${secret}?".into()),
                error: "owned side failure".into(),
            })
        });
        app.asides.pending = Some(Pending { session, worker });
        app.runtime.block_on(tokio::task::yield_now());
        app.poll_asides();
        assert!(app.chat.busy);
        assert!(app.chat.log.iter().any(|(_, line)| line.contains("Q: Which ${secret}?")));
        assert!(app.chat.log.iter().any(|(_, line)| line.contains("owned side failure")));
    }
}
