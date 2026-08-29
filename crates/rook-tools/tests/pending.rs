//! Waiting for a person to answer.
//!
//! Two ways this goes wrong and neither is visible from the code: an answer that
//! arrives while the map is busy and is dropped on the floor, and an answer that
//! never arrives at all, holding the turn and the store's write lock with it.

use std::sync::Arc;
use std::time::Duration;

use rook_tools::policy::{Approval, ApprovalRequest, Approver, ChannelApprover, Risk};

fn approver(
    patience: Duration,
) -> (Arc<ChannelApprover>, tokio::sync::mpsc::UnboundedReceiver<ApprovalRequest>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (Arc::new(ChannelApprover::new(tx, patience)), rx)
}

/// Answering while others are still being asked, so an insert and a removal
/// overlap. `try_lock` in `answer` dropped the answer whenever they did, and the
/// person who had already clicked waited out the whole timeout for nothing —
/// rare enough that no test forces it reliably, which is why the fix removes the
/// possibility rather than the symptom.
#[tokio::test]
async fn every_answer_reaches_its_question_while_others_are_still_arriving() {
    const IN_FLIGHT: usize = 64;
    let (approver, mut requests) = approver(Duration::from_secs(30));

    let answering = {
        let approver = approver.clone();
        tokio::spawn(async move {
            for _ in 0..IN_FLIGHT {
                let request = requests.recv().await.expect("the request reaches the front end");
                approver.answer(&request.id, Approval::Once);
            }
        })
    };

    let asking: Vec<_> = (0..IN_FLIGHT)
        .map(|i| {
            let approver = approver.clone();
            tokio::spawn(async move {
                approver.ask(&format!("tool{i}"), &Risk::Execute(format!("do {i}")), None).await
            })
        })
        .collect();

    for one in asking {
        assert!(matches!(one.await.unwrap(), Approval::Once), "an answer was lost between the two");
    }
    answering.await.unwrap();
}

#[tokio::test]
async fn a_question_nobody_answers_gives_up_and_says_how_long_it_waited() {
    let (approver, _requests) = approver(Duration::from_millis(50));

    let decided = approver.ask("run_command", &Risk::Execute("rm -rf /tmp/x".into()), None).await;
    let Approval::Deny(why) = decided else { panic!("silence is not consent") };
    assert!(why.contains("no answer within"), "{why}");
}

#[tokio::test]
async fn a_front_end_that_is_not_there_is_refused_at_once_rather_than_waited_out() {
    let (approver, requests) = approver(Duration::from_secs(3600));
    drop(requests);

    let started = std::time::Instant::now();
    let decided = approver.ask("run_command", &Risk::Execute("ls".into()), None).await;
    assert!(matches!(decided, Approval::Deny(_)), "nothing can answer, so nothing will");
    assert!(started.elapsed() < Duration::from_secs(1), "and it did not wait the hour out");
}
