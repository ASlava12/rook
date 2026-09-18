//! Provider stream consumption, progress and partial-answer preservation.

use super::delegation::Nursery;
use super::{AgentLoop, Progress, SAY_IT_STILL_WAITS_EVERY, SAY_IT_WAITS_AFTER};
use crate::error::{CoreError, Result};
use futures_util::StreamExt;
use rook_llm::{Assembler, ResponseStream};
use rook_store::EventKind;

impl AgentLoop<'_> {
    pub(super) async fn receive<F: FnMut(Progress<'_>)>(
        &self,
        mut stream: ResponseStream,
        nursery: &mut Nursery<'_>,
        nursery_steps: &mut tokio::sync::mpsc::UnboundedReceiver<(usize, String)>,
        carrying: &mut tokio::time::Interval,
        patience: std::time::Duration,
        on_progress: &mut F,
    ) -> Result<Assembler> {
        let mut assembler = Assembler::default();
        // The model call is the long wait in a step, so it is where started
        // sub-agents get to run. Without this they would only advance while
        // the parent was blocked on them, which is the thing being undone.
        let mut carried: Vec<String> = Vec::new();
        // Kept rather than returned from inside the loop, so what the model
        // had already said is logged before the error goes up. `?` there
        // dropped it: a provider that died two paragraphs in left a session
        // whose prompt is followed by nothing, which reads as an agent that
        // said nothing rather than a connection that went away — and the
        // window had shown those two paragraphs.
        let mut broke: Option<CoreError> = None;
        // The other half of the same silence, and on a local server the
        // half that is long: the stream opens the moment the request lands
        // and says nothing at all while the prompt is read. Nothing below
        // fires once a first token has arrived — after that the stream's
        // own idle timeout is what watches it.
        let quiet_since = tokio::time::Instant::now();
        let mut nothing_yet = true;
        let mut saying = tokio::time::interval_at(
            tokio::time::Instant::now() + SAY_IT_WAITS_AFTER,
            SAY_IT_STILL_WAITS_EVERY,
        );
        loop {
            tokio::select! {
                biased;
                Some((at, doing)) = nursery_steps.recv() => {
                    on_progress(Progress::Delegating { at, doing: &doing });
                }
                _ = carrying.tick() => nursery.relay(&self.interjections, &mut carried),
                Some(()) = nursery.collect_next(), if nursery.busy() => {}
                delta = stream.next() => {
                    nothing_yet = false;
                    let Some(delta) = delta else { break };
                    match delta {
                        Ok(delta) => {
                            on_progress(Progress::Delta(&delta));
                            if let Err(e) = assembler.push(delta) {
                                broke = Some(CoreError::Other(e.to_string()));
                                break;
                            }
                        }
                        Err(e) => {
                            broke = Some(CoreError::Other(e.to_string()));
                            break;
                        }
                    }
                }
                _ = saying.tick(), if nothing_yet => on_progress(Progress::Waiting {
                    secs: quiet_since.elapsed().as_secs(),
                    patience: patience.as_secs(),
                }),
            }
        }
        // The model has stopped talking, so the endpoint is free — but the
        // stream still holds its place in that endpoint's queue until it is
        // dropped, and everything below this line can want that place. A
        // turn on an endpoint allowing one request at a time deadlocked
        // here for four and a half hours: it finished its answer, took a
        // goal check, and the checker it spawned waited for a slot its own
        // parent was holding open. Every configured endpoint allows one
        // unless it says otherwise, and every autonomous turn takes a goal
        // check, so this was every such run.
        drop(stream);
        for text in carried {
            self.interjections.say(&text);
        }
        if let Some(e) = broke {
            // Both halves, because either can be the whole of what a turn
            // managed: a model that thought for a page and was cut before
            // its first word said something, and it is not the error.
            let reasoning = assembler.reasoning().to_string();
            if !reasoning.is_empty() {
                self.rook.log(self.session, EventKind::Reasoning, "", &reasoning).ok();
            }
            let partial = assembler.finish();
            if !partial.message.content.is_empty() {
                let said = partial.message.content;
                self.rook.log(self.session, EventKind::AssistantMessage, "cut off", &said).ok();
            }
            // Why it stops here, in the session rather than only on the
            // screen of whoever was watching.
            self.rook.log(self.session, EventKind::Note, "failed", &e.to_string()).ok();
            return Err(e);
        }
        Ok(assembler)
    }
}
