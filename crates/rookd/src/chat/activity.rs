//! Owned by the daemon turn, independent of both work and attached sockets.
use rook_core::activity::{self, Tracker};
use rook_proto::{ChatEvent, activity::Snapshot};
use rook_tools::{ask::ChannelAsker, policy::ChannelApprover};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

impl rook_core::agent::observe::Observer for Observation {
    fn observe(&self, event: rook_core::agent::observe::Event<'_>) {
        use rook_core::agent::{Progress, observe::Event};
        use rook_proto::activity::Stage;
        // A child is live only while this observer owns its admitted future.
        // No task text or model/tool payload is copied into this projection.
        {
            let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
            match &event {
                Event::ChildStarted { child, .. } => {
                    if children.len() >= 128 {
                        let finished = children.iter().find_map(|(id, (_, live))| (!live).then_some(*id));
                        if let Some(id) = finished {
                            children.remove(&id);
                        }
                    }
                    if children.len() < 128 {
                        children.insert(*child, (Tracker::new(*child, 0), true));
                    }
                }
                Event::ChildEnded { child, .. } => {
                    if let Some((tracker, live)) = children.get_mut(child) {
                        *live = false;
                        tracker.observe_phase(Stage::Finished);
                    }
                }
                Event::ModelQueued { session }
                | Event::ModelStarted { session }
                | Event::ToolOutput { session }
                | Event::CompactionStarted { session, .. }
                | Event::CompactionEnded { session, .. }
                | Event::Progress { session, .. } => {
                    if let Some((tracker, live)) = children.get_mut(session)
                        && *live
                    {
                        match &event {
                            Event::ModelQueued { .. } => tracker.observe_phase(Stage::Queued),
                            Event::ToolOutput { .. } => tracker.observe_phase(Stage::Tool),
                            Event::Progress { workspace, progress, .. } => {
                                if !matches!(
                                    progress,
                                    Progress::Delegating { .. }
                                        | Progress::Working { .. }
                                        | Progress::Waiting { .. }
                                ) && let Some(event) = super::as_event(**progress, workspace)
                                {
                                    tracker.observe(&event);
                                }
                            }
                            _ => tracker.observe_phase(Stage::Model),
                        }
                    }
                }
            }
        }
        let stage = match event {
            Event::ModelQueued { session } if session == self.session => Some(Stage::Queued),
            Event::ModelStarted { session } if session == self.session => Some(Stage::Model),
            Event::ToolOutput { .. } => Some(Stage::Tool),
            Event::Progress { session, workspace, progress } if session == self.session => {
                // Delegated display hints may contain periodic waiting prose.
                // Their underlying observers report actual child work instead.
                if !matches!(
                    progress,
                    Progress::Delegating { .. } | Progress::Working { .. } | Progress::Waiting { .. }
                ) && let Some(event) = super::as_event(*progress, workspace)
                {
                    self.observe(&event);
                }
                None
            }
            Event::Progress { progress, .. } => match progress {
                Progress::Delta(rook_llm::Delta::Text(text) | rook_llm::Delta::Reasoning(text))
                    if !text.is_empty() =>
                {
                    Some(Stage::Tool)
                }
                Progress::Delta(rook_llm::Delta::ToolCall(_)) | Progress::ToolDone { .. } => {
                    Some(Stage::Tool)
                }
                _ => None,
            },
            Event::ChildStarted { .. } | Event::ChildEnded { .. } => Some(Stage::Tool),
            Event::CompactionStarted { session, .. } | Event::CompactionEnded { session, .. }
                if session == self.session =>
            {
                Some(Stage::Model)
            }
            _ => None,
        };
        if let Some(stage) = stage {
            self.tracker.lock().unwrap_or_else(|e| e.into_inner()).observe_phase(stage);
        }
    }
}

pub(crate) struct Observation {
    tracker: Mutex<Tracker>,
    children: Mutex<std::collections::BTreeMap<u128, (Tracker, bool)>>,
    store: Arc<rook_store::Store>,
    pub(crate) session: u128,
    running: std::sync::atomic::AtomicBool,
}

impl Observation {
    pub(crate) fn new(rook: &rook_core::Rook, session: u128) -> Arc<Self> {
        Arc::new(Self {
            tracker: Mutex::new(Tracker::new(session, rook.config.telemetry.stall_after_secs)),
            children: Mutex::new(std::collections::BTreeMap::new()),
            store: rook.store.clone(),
            session,
            running: std::sync::atomic::AtomicBool::new(true),
        })
    }

    pub(super) fn observe(&self, event: &ChatEvent) {
        self.tracker.lock().unwrap_or_else(|e| e.into_inner()).observe(event);
    }

    pub(crate) fn snapshot(&self, running: bool) -> Snapshot {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot(running && self.running.load(std::sync::atomic::Ordering::Acquire))
    }

    pub(crate) fn for_session(&self, session: u128, running: bool) -> Option<Snapshot> {
        if session == self.session {
            return Some(self.snapshot(running));
        }
        let mut children = self.children.lock().unwrap_or_else(|e| e.into_inner());
        children.get_mut(&session).map(|(tracker, live)| {
            tracker.check(false, false);
            tracker.snapshot(*live && running && self.running.load(std::sync::atomic::Ordering::Acquire))
        })
    }

    fn save(&self, snapshot: &Snapshot) {
        if let Err(error) = activity::save(&self.store, self.session, snapshot) {
            tracing::warn!("could not save activity observation: {error}");
        }
    }

    pub(super) fn finish(&self) {
        self.running.store(false, std::sync::atomic::Ordering::Release);
        self.save(&self.snapshot(false));
    }

    pub(super) fn start(
        self: &Arc<Self>,
        approver: Arc<ChannelApprover>,
        asker: Arc<ChannelAsker>,
        outbound: mpsc::UnboundedSender<ChatEvent>,
    ) -> tokio::task::JoinHandle<()> {
        self.watch(Some((approver, asker)), Some(outbound), std::time::Duration::from_secs(10))
    }

    pub(crate) fn unattended(self: &Arc<Self>) -> Watch {
        Watch { task: self.watch(None, None, std::time::Duration::from_secs(10)), observation: self.clone() }
    }

    fn watch(
        self: &Arc<Self>,
        input: Option<(Arc<ChannelApprover>, Arc<ChannelAsker>)>,
        outbound: Option<mpsc::UnboundedSender<ChatEvent>>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        // Constructed before spawn: cancellation before its first poll still
        // records that this owner no longer runs the observed work.
        if let Err(error) = activity::begin(&self.store, self.session, &self.snapshot(true)) {
            tracing::warn!("could not begin activity observation: {error}");
        }
        let finishing = Finish(self.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let (notify, snapshot) = {
                    let mut tracker = finishing.0.tracker.lock().unwrap_or_else(|e| e.into_inner());
                    let notify = tracker.check(
                        input.as_ref().is_some_and(|(approver, _)| approver.is_waiting()),
                        input.as_ref().is_some_and(|(_, asker)| asker.is_waiting()),
                    );
                    (notify, tracker.snapshot(true))
                };
                finishing.0.save(&snapshot);
                if notify {
                    tracing::warn!(session = %snapshot.session, stage = ?snapshot.stage,
                        idle_secs = snapshot.idle_secs, "no recent progress observed");
                    if let Some(outbound) = &outbound
                        && outbound.send(ChatEvent::Activity { state: snapshot }).is_err()
                    {
                        break;
                    }
                }
            }
        })
    }
}

struct Finish(Arc<Observation>);
impl Drop for Finish {
    fn drop(&mut self) {
        self.0.finish();
    }
}

pub(crate) struct Watch {
    task: tokio::task::JoinHandle<()>,
    observation: Arc<Observation>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.task.abort();
        self.observation.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Live, Settings, approver, asker, everything_it_says};
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn typed_queue_and_pipe_progress_are_independent_of_elapsed_time_display_hints() {
        use rook_core::agent::{
            Progress,
            observe::{Event, Observer},
        };
        use rook_proto::activity::Stage;
        let root = tempfile::tempdir().unwrap();
        let mut config = rook_core::Config::default();
        config.telemetry.stall_after_secs = 30;
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            root.path().into(),
        );
        let session = rook.start_session("phases").unwrap();
        let observation = Observation::new(&rook, session);
        Observer::observe(&*observation, Event::ModelQueued { session });
        tokio::time::advance(Duration::from_secs(31)).await;
        let hint = Progress::Waiting { secs: 31, patience: 300 };
        Observer::observe(
            &*observation,
            Event::Progress { session, workspace: root.path(), progress: &hint },
        );
        assert_eq!(observation.snapshot(true).stage, Stage::Queued);
        assert!(observation.tracker.lock().unwrap().check(false, false));
        Observer::observe(&*observation, Event::ModelStarted { session });
        assert_eq!(observation.snapshot(true).stage, Stage::Model);
        assert!(!observation.snapshot(true).stalled);
        observation.observe(&ChatEvent::Tool { name: "run_command".into(), doing: "PRIVATE COMMAND".into() });
        tokio::time::advance(Duration::from_secs(31)).await;
        let working = Progress::Working { call: "run_command", said: "running for 31 seconds" };
        Observer::observe(
            &*observation,
            Event::Progress { session, workspace: root.path(), progress: &working },
        );
        assert!(observation.tracker.lock().unwrap().check(false, false));
        Observer::observe(&*observation, Event::ToolOutput { session });
        assert_eq!(observation.snapshot(true).stage, Stage::Tool);
        assert_eq!(observation.snapshot(true).operation.as_deref(), Some("run_command"));
        assert_eq!(observation.snapshot(true).idle_secs, 0);
        assert!(!observation.snapshot(true).stalled);
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(observation.tracker.lock().unwrap().check(false, false));
        assert_eq!(observation.snapshot(true).notifications, 3);
        assert!(!serde_json::to_string(&observation.snapshot(true)).unwrap().contains("PRIVATE"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_turn_notifies_without_any_client_and_a_later_join_sees_it() {
        let root = tempfile::tempdir().unwrap();
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            root.path().into(),
        );
        let session = rook.start_session("quiet turn").unwrap();
        let observation = Observation::new(&rook, session);
        *observation.tracker.lock().unwrap() = Tracker::new(session, 30);
        let (outbound, events) = mpsc::unbounded_channel();
        let (approver, relay) = approver(outbound.clone(), Duration::from_secs(3600), Default::default());
        let (asker, ask_relay) = asker(outbound.clone(), Duration::from_secs(3600), Default::default());
        let watchdog = observation.watch(
            Some((approver.clone(), asker.clone())),
            Some(outbound.clone()),
            Duration::from_secs(1),
        );
        let (said, detached) = tokio::sync::broadcast::channel(16);
        drop(detached);
        assert_eq!(said.receiver_count(), 0, "there is no attached client to drive the watcher");
        let backlog: Arc<Mutex<super::super::replay::Replay>> = Default::default();
        let helpers = vec![relay.abort_handle(), ask_relay.abort_handle(), watchdog.abort_handle()];
        let completed = Default::default();
        let task = tokio::spawn(everything_it_says(
            std::future::pending(),
            events,
            said.clone(),
            backlog.clone(),
            helpers.clone(),
            Some(observation.clone()),
            Arc::clone(&completed),
        ));
        let live = Live {
            epoch: rook_store::format_session_id(rook_store::new_session_id()),
            task,
            helpers,
            said,
            backlog,
            approver,
            asker,
            settings: Arc::new(Settings::for_test()),
            activity: Some(observation.clone()),
            completed,
        };
        observation.observe(&ChatEvent::ModelRequest {
            model: "local".into(),
            requested_effort: "high".into(),
            effort: "high".into(),
        });
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(observation.snapshot(true).idle_secs > 30);
        // Subscription synchronizes with the watchdog notice, not a sleep long
        // enough to hope the background task has published it.
        let mut signal = live.said.subscribe();
        tokio::time::timeout(Duration::from_secs(3), signal.recv()).await.unwrap().unwrap();
        let (attached, missed, _) = live.join();
        assert!(missed.iter().any(|event| matches!(event, ChatEvent::Activity { state } if state.stalled && state.stage == rook_proto::activity::Stage::Model)));
        drop(attached);
        let report = rook.diagnostics(session, false).unwrap();
        assert_eq!(report.activity.unwrap().notifications, 1);
        observation.observe(&ChatEvent::Text { text: "progress".into() });
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::timeout(Duration::from_secs(3), signal.recv()).await.unwrap().unwrap();
        assert_eq!(live.activity().unwrap().notifications, 2);
        live.shutdown().await;
        watchdog.await.unwrap_err();
        assert_eq!(rook.diagnostics(session, false).unwrap().activity.unwrap().owner_live, None);
        assert!(!live.activity().unwrap().owner_live.unwrap());
    }
}

#[cfg(test)]
mod child_observation_tests {
    use super::*;
    use rook_core::agent::observe::{Event, Observer};
    #[test]
    fn child_liveness_requires_owned_admission_and_ends_with_that_owner() {
        let dir = tempfile::tempdir().unwrap();
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(dir.path().join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            dir.path().into(),
        );
        let root = rook.start_session("parent").unwrap();
        let child = rook.fork_for_subtask(root, "PRIVATE_TASK").unwrap();
        let observation = Observation::new(&rook, root);
        assert!(observation.for_session(child, true).is_none());
        Observer::observe(&*observation, Event::ChildStarted { parent: root, child, task: "PRIVATE_TASK" });
        Observer::observe(&*observation, Event::ModelStarted { session: child });
        let snapshot = observation.for_session(child, true).unwrap();
        assert_eq!(snapshot.owner_live, Some(true));
        assert_eq!(snapshot.stage, rook_proto::activity::Stage::Model);
        assert!(!serde_json::to_string(&snapshot).unwrap().contains("PRIVATE_TASK"));
        Observer::observe(&*observation, Event::ChildEnded { child, result: None });
        assert_eq!(observation.for_session(child, true).unwrap().owner_live, Some(false));
        Observer::observe(&*observation, Event::ChildStarted { parent: root, child, task: "task" });
        observation.finish();
        assert_eq!(observation.for_session(child, true).unwrap().owner_live, Some(false));
    }
    #[test]
    fn child_projection_reaches_its_bound_and_prunes_finished_observations_first() {
        let dir = tempfile::tempdir().unwrap();
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(dir.path().join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            dir.path().into(),
        );
        let root = rook.start_session("parent").unwrap();
        let observation = Observation::new(&rook, root);
        for child in 1..=129 {
            Observer::observe(
                &*observation,
                Event::ChildStarted { parent: root, child, task: "not retained" },
            );
        }
        assert_eq!(
            observation.children.lock().unwrap().len(),
            128,
            "129 admissions exceed the actual projection cap"
        );
        assert!(observation.for_session(129, true).is_none(), "a missing observation cannot attest an owner");
        assert_eq!(observation.for_session(1, true).unwrap().owner_live, Some(true));
        Observer::observe(&*observation, Event::ChildEnded { child: 1, result: None });
        Observer::observe(
            &*observation,
            Event::ChildStarted { parent: root, child: 130, task: "not retained" },
        );
        assert!(observation.for_session(1, true).is_none(), "finished entries can be pruned");
        assert_eq!(observation.for_session(2, true).unwrap().owner_live, Some(true));
        assert_eq!(observation.for_session(130, true).unwrap().owner_live, Some(true));
    }
}
