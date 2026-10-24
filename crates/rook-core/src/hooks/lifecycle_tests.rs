#![cfg(test)]

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct InputOwner(Arc<AtomicBool>);
impl Drop for InputOwner {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[tokio::test(start_paused = true)]
async fn exited_hook_retires_stalled_stdin_without_waiting_for_its_watchdog() {
    let dropped = Arc::new(AtomicBool::new(false));
    let owner = InputOwner(dropped.clone());
    let input = async move {
        let _owner = owner;
        std::future::pending::<()>().await;
    };
    let result =
        tokio::time::timeout(Duration::from_millis(50), wait_after_input(input, std::future::ready(7)))
            .await
            .expect("an exited child cannot consume stdin; retire the pending input");
    assert_eq!(result, 7);
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn completed_input_still_waits_for_the_actual_child_exit() {
    let (exit, receive) = tokio::sync::oneshot::channel();
    let mut waiting = Box::pin(wait_after_input(async {}, async { receive.await.unwrap() }));
    tokio::select! {
        _ = &mut waiting => panic!("input completion is not process completion"),
        _ = tokio::task::yield_now() => {},
    }
    exit.send(9).unwrap();
    assert_eq!(waiting.await, 9);
}

#[tokio::test]
async fn early_exiting_hooks_keep_their_actual_exit_and_reply_without_reading_a_large_input() {
    for (command, denied) in [("exit 7", true), ("echo ready", false)] {
        let (hooks, errors) = Hooks::compile(&[HookConfig {
            event: Event::PreTool,
            command: command.into(),
            ..Default::default()
        }]);
        assert!(errors.is_empty());
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            hooks.run(Event::PreTool, "write", &serde_json::json!({"input":"x".repeat(1024*1024)})),
        )
        .await
        .expect("an already exited hook must not wait for its input watchdog");
        if denied {
            let Some(Decision::Deny(why)) = outcome.decision else {
                panic!("exit 7 must still refuse the gate")
            };
            assert!(why.contains("hook failed: exit 7"), "{why}");
        } else {
            assert!(outcome.decision.is_none());
            assert_eq!(outcome.context(), Some("ready".into()));
        }
    }
}
