use std::io::Cursor as Reader;

use codex_adapter::{Cursor, EventKind, read_batch};
use serde_json::{Value, json};

fn line(kind: &str, payload: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&json!({
        "timestamp": "2026-10-11T08:00:00Z", "type": kind, "payload": payload
    }))
    .unwrap();
    bytes.push(b'\n');
    bytes
}

fn replay(bytes: Vec<u8>) -> codex_adapter::Batch {
    read_batch(
        &mut Reader::new(bytes),
        &Cursor::replay("stream".into(), "own".into()),
        65536,
    )
    .unwrap()
}

#[test]
fn baseline_rejects_incomplete_or_mismatched_session_metadata() {
    let bytes = line("session_meta", json!({"id":"parent"}));
    assert!(Cursor::baseline(&mut Reader::new(bytes), "stream".into(), "own".into()).is_err());
    assert!(
        Cursor::baseline(
            &mut Reader::new(b"{\"type\":".to_vec()),
            "stream".into(),
            "own".into()
        )
        .is_err()
    );
}

#[test]
fn partial_rows_retry_and_committed_rows_are_not_replayed() {
    let bytes = line(
        "event_msg",
        json!({"type":"user_message", "message":"需求", "turn_id":"t1"}),
    );
    let cursor = Cursor::replay("stream".into(), "own".into());
    let partial = read_batch(
        &mut Reader::new(bytes[..bytes.len() - 1].to_vec()),
        &cursor,
        65536,
    )
    .unwrap();
    assert!(partial.events.is_empty());
    assert_eq!(partial.next_cursor, cursor);
    let complete = read_batch(&mut Reader::new(bytes.clone()), &cursor, 65536).unwrap();
    assert_eq!(complete.events[0].kind, EventKind::UserPrompt);
    assert_eq!(complete.events[0].raw_line, bytes);
    let idle = read_batch(&mut Reader::new(bytes), &complete.next_cursor, 65536).unwrap();
    assert_eq!(idle.bytes_read, 0);
    assert!(idle.events.is_empty());
}

#[test]
fn live_baseline_does_not_emit_history_or_preexisting_partial_row() {
    let mut bytes = line("session_meta", json!({"id":"own", "cwd":"C:\\repo"}));
    bytes.extend(line(
        "event_msg",
        json!({"type":"user_message", "message":"old"}),
    ));
    let partial = line(
        "event_msg",
        json!({"type":"user_message", "message":"also old"}),
    );
    bytes.extend_from_slice(&partial[..10]);
    let cursor = Cursor::baseline(
        &mut Reader::new(bytes.clone()),
        "stream".into(),
        "own".into(),
    )
    .unwrap();
    assert_eq!(cursor.state.workspace.as_deref(), Some("C:\\repo"));
    bytes.extend_from_slice(&partial[10..]);
    bytes.extend(line(
        "event_msg",
        json!({"type":"user_message", "message":"new", "turn_id":"t"}),
    ));
    let batch = read_batch(&mut Reader::new(bytes), &cursor, 65536).unwrap();
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].raw["payload"]["message"], "new");
}

#[test]
fn failed_persistence_can_retry_with_identical_event_ids() {
    let bytes = line(
        "response_item",
        json!({"type":"function_call", "call_id":"call1", "arguments":"{}"}),
    );
    let first = replay(bytes.clone());
    let retry = replay(bytes);
    assert_eq!(first.events, retry.events);
    assert_eq!(first.events[0].call_id.as_deref(), Some("call1"));
}

#[test]
fn tool_calls_and_outputs_keep_native_call_identity() {
    let mut bytes = line("turn_context", json!({"turn_id":"t1", "cwd":"C:\\repo"}));
    bytes.extend(line(
        "response_item",
        json!({"type":"custom_tool_call", "call_id":"c1", "input":"patch"}),
    ));
    bytes.extend(line(
        "response_item",
        json!({"type":"custom_tool_call_output", "call_id":"c1", "output":"done"}),
    ));
    let events = replay(bytes).events;
    assert_eq!(events[1].kind, EventKind::ToolCall);
    assert_eq!(events[2].kind, EventKind::ToolResult);
    assert_eq!(events[1].call_id, events[2].call_id);
    assert_eq!(events[2].turn_id.as_deref(), Some("t1"));
}

#[test]
fn late_tool_result_uses_its_call_turn_instead_of_the_latest_turn() {
    let mut bytes = line("turn_context", json!({"turn_id":"t1"}));
    bytes.extend(line(
        "response_item",
        json!({"type":"function_call", "call_id":"c1"}),
    ));
    bytes.extend(line("turn_context", json!({"turn_id":"t2"})));
    bytes.extend(line(
        "response_item",
        json!({"type":"function_call_output", "call_id":"c1", "output":"late"}),
    ));
    bytes.extend(line(
        "response_item",
        json!({"type":"function_call_output", "call_id":"unknown", "output":"unattributed"}),
    ));
    let events = replay(bytes).events;
    assert_eq!(events[3].turn_id.as_deref(), Some("t1"));
    assert_eq!(events[4].turn_id, None);
}

#[test]
fn native_stop_never_claims_goal_completion() {
    let events = replay(line(
        "event_msg",
        json!({"type":"task_complete", "turn_id":"t1"}),
    ))
    .events;
    assert_eq!(events[0].kind, EventKind::TaskStopped);
}

#[test]
fn copied_parent_history_never_replaces_child_identity() {
    let mut bytes = line(
        "session_meta",
        json!({"id":"own", "source":{"subagent":{"thread_spawn":{
            "parent_thread_id":"parent", "agent_path":"/root/child"
        }}}}),
    );
    bytes.extend(line(
        "session_meta",
        json!({"id":"parent", "cwd":"C:\\parent"}),
    ));
    bytes.extend(line(
        "event_msg",
        json!({"type":"user_message", "message":"parent request", "turn_id":"parent-turn"}),
    ));
    bytes.extend(line("event_msg", json!({"type":"thread_settings_applied"})));
    bytes.extend(line(
        "event_msg",
        json!({"type":"task_started", "turn_id":"child-turn"}),
    ));
    bytes.extend(line(
        "event_msg",
        json!({"type":"user_message", "message":"delegated instruction"}),
    ));
    let events = replay(bytes).events;
    assert!(events.iter().all(|event| event.session_id == "own"));
    assert_eq!(events[2].kind, EventKind::RuntimeContext);
    assert_eq!(events[2].turn_id, None);
    assert_eq!(events[4].turn_id.as_deref(), Some("child-turn"));
    assert_eq!(events[5].kind, EventKind::RuntimeContext);
    assert_eq!(events[5].parent_session_id.as_deref(), Some("parent"));
}

#[test]
fn runtime_envelopes_are_context_but_human_quotes_are_preserved() {
    let mut bytes = line(
        "event_msg",
        json!({"type":"user_message", "message":"<environment_context>x</environment_context>"}),
    );
    bytes.extend(line("event_msg", json!({"type":"user_message", "message":"<environment_context>x</environment_context>\n请修复"})));
    bytes.extend(line("event_msg", json!({"type":"user_message", "message":"<environment_context>x</environment_context>", "content_item_kinds":["user.text"]})));
    let events = replay(bytes).events;
    assert_eq!(events[0].kind, EventKind::RuntimeContext);
    assert_eq!(events[1].kind, EventKind::UserPrompt);
    assert_eq!(events[2].kind, EventKind::UserPrompt);
}

#[test]
fn truncation_and_cursor_tail_replacement_require_explicit_rebaseline() {
    let bytes = line(
        "event_msg",
        json!({"type":"user_message", "message":"original"}),
    );
    let first = replay(bytes.clone());
    assert!(
        read_batch(
            &mut Reader::new(Vec::<u8>::new()),
            &first.next_cursor,
            65536
        )
        .is_err()
    );
    // The cursor authenticates only its trailing 64 bytes, not old history.
    // Alter a timestamp digit inside that anchor while preserving file length.
    let mut changed = bytes.clone();
    let index = changed.iter().rposition(|byte| *byte == b'0').unwrap();
    assert!(index >= changed.len().saturating_sub(64));
    changed[index] = b'1';
    assert!(read_batch(&mut Reader::new(changed), &first.next_cursor, 65536).is_err());
}

#[test]
fn malformed_rows_and_oversized_rows_do_not_acknowledge_data() {
    let cursor = Cursor::replay("stream".into(), "own".into());
    assert!(read_batch(&mut Reader::new(b"not-json\n".to_vec()), &cursor, 65536).is_err());
    let bytes = line(
        "event_msg",
        json!({"type":"user_message", "message":"large"}),
    );
    assert!(read_batch(&mut Reader::new(bytes), &cursor, 8).is_err());
    assert_eq!(cursor.offset, 0);
}

#[test]
fn cursor_round_trip_preserves_native_identity_and_unknown_payload() {
    let bytes = line(
        "turn_context",
        json!({"turn_id":"t1", "future_field":{"opaque":[1,2,3]}}),
    );
    let batch = replay(bytes);
    let encoded = serde_json::to_vec(&batch.next_cursor).unwrap();
    assert_eq!(
        serde_json::from_slice::<Cursor>(&encoded).unwrap(),
        batch.next_cursor
    );
    assert_eq!(
        batch.events[0].raw["payload"]["future_field"]["opaque"][2],
        3
    );
}
