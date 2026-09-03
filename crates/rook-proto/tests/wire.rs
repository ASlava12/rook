//! The browser speaks these shapes by hand, in `web/dist/index.html`. A field
//! renamed on one side and not the other is a silent failure at runtime, so the
//! literal JSON is what these tests assert.

use rook_proto::{AskQuestion, ChatEvent, ClientMessage};

#[test]
fn a_question_reaches_the_browser_in_the_shape_it_reads() {
    let event = ChatEvent::Ask {
        id: "3".into(),
        questions: vec![AskQuestion {
            question: "Which target?".into(),
            choices: vec!["staging".into(), "prod".into()],
            multi: false,
        }],
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        serde_json::json!({
            "type": "ask",
            "id": "3",
            "questions": [{"question": "Which target?", "choices": ["staging", "prod"], "multi": false}]
        })
    );
}

#[test]
fn the_browsers_answers_parse_back() {
    let sent = r#"{"type":"answers","id":"3","answers":[["prod"],[]]}"#;
    let ClientMessage::Answers { id, answers } = serde_json::from_str(sent).unwrap() else {
        panic!("the browser's answer message must parse as Answers");
    };
    assert_eq!(id, "3");
    assert_eq!(answers, vec![vec!["prod".to_string()], vec![]], "an empty answer is a skipped question");
}

#[test]
fn a_question_with_no_choices_is_free_text() {
    let asked: AskQuestion = serde_json::from_str(r#"{"question":"Why?"}"#).unwrap();
    assert!(asked.choices.is_empty() && !asked.multi);
}

#[test]
fn a_finished_tool_call_is_reported_as_its_own_event() {
    let event = ChatEvent::ToolDone { name: "read_file".into(), failed: false };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        serde_json::json!({ "type": "tool_done", "name": "read_file", "failed": false }),
        "the browser branches on this tag by hand"
    );
}

#[test]
fn the_settings_the_browser_sends_and_receives_match_what_it_reads() {
    let sent = r#"{"type":"setting","name":"effort","value":"low"}"#;
    let ClientMessage::Setting { name, value } = serde_json::from_str(sent).unwrap() else {
        panic!("the browser's setting message must parse as Setting");
    };
    assert_eq!((name.as_str(), value.as_str()), ("effort", "low"));

    // The lists are what the page draws its selects from; an old page that
    // does not read them is unaffected by their presence.
    let settings = ChatEvent::Settings {
        mode: "assist".into(),
        effort: "high".into(),
        stances: vec!["readonly".into(), "assist".into()],
        efforts: vec!["low".into(), "high".into()],
    };
    assert_eq!(
        serde_json::to_value(settings).unwrap(),
        serde_json::json!({
            "type": "settings", "mode": "assist", "effort": "high",
            "stances": ["readonly", "assist"], "efforts": ["low", "high"]
        }),
        "the page branches on this tag by hand"
    );
}
