use rook_llm::stream::ToolCallBuffer;
use rook_llm::{Assembler, Delta, ToolCall};
#[test]
fn untrusted_tool_indices_and_arguments_are_bounded() {
    let mut buffer = ToolCallBuffer::default();
    assert!(buffer.push(usize::MAX, None, Some("read_file"), "{}").is_err());
    let mut buffer = ToolCallBuffer::default();
    assert!(buffer.push(0, None, Some("read_file"), &"x".repeat(33 << 20)).is_err());
}
#[test]
fn missing_names_are_errors_and_missing_ids_are_unique() {
    let mut buffer = ToolCallBuffer::default();
    buffer.push(0, Some("a"), None, "{}").unwrap();
    assert!(buffer.drain().is_err());
    buffer.push(0, None, Some("a"), "{}").unwrap();
    buffer.push(1, None, Some("b"), "{}").unwrap();
    let calls = buffer.drain().unwrap();
    assert!(!calls[0].id.is_empty());
    assert_ne!(calls[0].id, calls[1].id);
}
#[test]
fn reasoning_blocks_and_tool_arguments_share_the_reply_budget() {
    let big = serde_json::json!("x".repeat(33 << 20));
    assert!(Assembler::default().push(Delta::ReasoningDone(big.clone())).is_err());
    assert!(
        Assembler::default()
            .push(Delta::ToolCall(ToolCall { id: "a".into(), name: "write".into(), arguments: big }))
            .is_err()
    );
}
