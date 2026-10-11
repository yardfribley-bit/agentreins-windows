//! Incremental Codex rollout reader. The caller persists evidence and the next
//! cursor in one transaction; reading a batch never advances the supplied cursor.
//! No process ownership or goal completion is inferred from a rollout record.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: &str = "codex-live-v1";
pub const DEFAULT_BATCH_BYTES: usize = 8 * 1024 * 1024;
const ANCHOR_BYTES: u64 = 64;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StreamState {
    /// Stable native session ID from the rollout filename, never a parent ID.
    pub session_id: String,
    pub turn_id: Option<String>,
    pub workspace: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent_path: Option<String>,
    pub inherited_history: bool,
    #[serde(default)]
    pub open_calls: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Cursor {
    pub schema_version: String,
    /// Stable identity for this source file, not a PID or display name.
    pub stream_id: String,
    pub offset: u64,
    pub state: StreamState,
    pub anchor: Vec<u8>,
    /// A live baseline may fall inside a row that started before observation.
    pub skip_partial_baseline: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    UserPrompt,
    RuntimeContext,
    ToolCall,
    ToolResult,
    AssistantMessage,
    TaskStarted,
    TaskStopped,
    TaskFailed,
    TaskCancelled,
    SourceRecord,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct LiveEvent {
    pub schema_version: String,
    pub event_id: String,
    pub stream_id: String,
    pub offset_start: u64,
    pub offset_end: u64,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub call_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub workspace: Option<String>,
    pub kind: EventKind,
    /// Preserve the source timestamp, payload, and unknown fields losslessly.
    pub raw: Value,
    /// Exact JSONL bytes, including the newline, for forensic export.
    pub raw_line: Vec<u8>,
}

#[derive(Serialize)]
pub struct Batch {
    pub events: Vec<LiveEvent>,
    pub next_cursor: Cursor,
    pub bytes_read: usize,
}

impl Cursor {
    /// Explicit replay entry point for fixtures/imports. Live observation should
    /// use `baseline` so historical requests are not presented as new tasks.
    pub fn replay(stream_id: String, session_id: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.into(),
            stream_id,
            offset: 0,
            state: StreamState {
                session_id,
                turn_id: None,
                workspace: None,
                parent_session_id: None,
                agent_path: None,
                inherited_history: false,
                open_calls: BTreeMap::new(),
            },
            anchor: Vec::new(),
            skip_partial_baseline: false,
        }
    }

    /// Read only the first bounded metadata row and baseline at the current EOF.
    /// Historical message bodies are neither parsed nor emitted.
    pub fn baseline<R: Read + Seek>(
        reader: &mut R,
        stream_id: String,
        session_id: String,
    ) -> io::Result<Self> {
        let mut cursor = Self::replay(stream_id, session_id);
        reader.seek(SeekFrom::Start(0))?;
        let mut header = Vec::new();
        (&mut *reader).take(64 * 1024).read_to_end(&mut header)?;
        if let Some(end) = header.iter().position(|byte| *byte == b'\n') {
            let row: Value = serde_json::from_slice(&header[..end]).map_err(invalid)?;
            let id = text(&row["payload"], "id").or_else(|| text(&row["payload"], "session_id"));
            if row["type"] != "session_meta"
                || id.as_deref() != Some(cursor.state.session_id.as_str())
            {
                return Err(invalid(
                    "first metadata row does not prove the supplied native session",
                ));
            }
            update_state(&mut cursor.state, &row);
            // EOF is already beyond inherited history at baseline. The next
            // observation still needs an explicit native turn ID.
            cursor.state.inherited_history = false;
        } else if !header.is_empty() {
            return Err(invalid(
                "first metadata row incomplete or exceeds 64 KiB; retry without advancing",
            ));
        }
        cursor.offset = reader.seek(SeekFrom::End(0))?;
        cursor.anchor = anchor(reader, cursor.offset)?;
        cursor.skip_partial_baseline = cursor.anchor.last().is_some_and(|byte| *byte != b'\n');
        Ok(cursor)
    }
}

/// Read at most `maximum_bytes` from an append-only file. Partial final rows are
/// retried from the same offset. Truncation/replacement and malformed rows fail
/// explicitly; the caller retains its previous committed cursor and evidence.
pub fn read_batch<R: Read + Seek>(
    reader: &mut R,
    cursor: &Cursor,
    maximum_bytes: usize,
) -> io::Result<Batch> {
    if cursor.schema_version != SCHEMA_VERSION || maximum_bytes == 0 {
        return Err(invalid("unsupported cursor or empty batch budget"));
    }
    let length = reader.seek(SeekFrom::End(0))?;
    if length < cursor.offset || anchor(reader, cursor.offset)? != cursor.anchor {
        return Err(invalid(
            "source truncated or replaced; explicit rebaseline required",
        ));
    }
    reader.seek(SeekFrom::Start(cursor.offset))?;
    let mut bytes = Vec::new();
    (&mut *reader)
        .take(maximum_bytes as u64)
        .read_to_end(&mut bytes)?;
    let mut next = cursor.clone();
    let mut events = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let end = index + 1;
        if next.skip_partial_baseline {
            next.skip_partial_baseline = false;
        } else if bytes[start..index]
            .iter()
            .any(|byte| !byte.is_ascii_whitespace())
        {
            let row: Value = serde_json::from_slice(&bytes[start..index]).map_err(invalid)?;
            if !row.is_object() || !row["type"].is_string() || !row["payload"].is_object() {
                return Err(invalid("invalid Codex rollout envelope"));
            }
            update_state(&mut next.state, &row);
            let offset_start = cursor.offset + start as u64;
            let offset_end = cursor.offset + end as u64;
            let mut kind = classify(&row);
            // Copied parent history and child user envelopes are context. Never
            // turn delegated/inherited text into a human's original request.
            if next.state.inherited_history
                || (next.state.parent_session_id.is_some() && kind == EventKind::UserPrompt)
            {
                kind = EventKind::RuntimeContext;
            }
            let call_id = text(&row["payload"], "call_id");
            let turn_id = if kind == EventKind::ToolResult {
                let bound = call_id
                    .as_ref()
                    .and_then(|call| next.state.open_calls.remove(call));
                text(&row["payload"], "turn_id").or(bound)
            } else {
                if kind == EventKind::ToolCall
                    && let (Some(call), Some(turn)) = (&call_id, &next.state.turn_id)
                {
                    next.state.open_calls.insert(call.clone(), turn.clone());
                }
                next.state.turn_id.clone()
            };
            events.push(LiveEvent {
                schema_version: SCHEMA_VERSION.into(),
                event_id: format!("codex:{}:{offset_start}", cursor.stream_id),
                stream_id: cursor.stream_id.clone(),
                offset_start,
                offset_end,
                session_id: next.state.session_id.clone(),
                turn_id,
                call_id,
                parent_session_id: next.state.parent_session_id.clone(),
                workspace: next.state.workspace.clone(),
                kind,
                raw: row,
                raw_line: bytes[start..end].to_vec(),
            });
        }
        next.offset = cursor.offset + end as u64;
        start = end;
    }
    if start == 0 && bytes.len() == maximum_bytes {
        return Err(invalid(
            "row exceeds batch budget; enlarge budget without advancing cursor",
        ));
    }
    next.anchor = anchor(reader, next.offset)?;
    Ok(Batch {
        events,
        next_cursor: next,
        bytes_read: bytes.len(),
    })
}

fn anchor<R: Read + Seek>(reader: &mut R, offset: u64) -> io::Result<Vec<u8>> {
    let start = offset.saturating_sub(ANCHOR_BYTES);
    reader.seek(SeekFrom::Start(start))?;
    let mut result = vec![0; (offset - start) as usize];
    reader.read_exact(&mut result)?;
    Ok(result)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(String::from)
}

fn update_state(state: &mut StreamState, row: &Value) {
    let payload = &row["payload"];
    if row["type"] == "session_meta" {
        let session = text(payload, "id").or_else(|| text(payload, "session_id"));
        if session.as_deref() != Some(state.session_id.as_str()) {
            state.inherited_history = true;
            state.turn_id = None;
            return;
        }
        state.workspace = text(payload, "cwd");
        let spawn = &payload["source"]["subagent"]["thread_spawn"];
        state.parent_session_id = text(spawn, "parent_thread_id");
        state.agent_path = text(spawn, "agent_path");
    }
    let addressed = row["type"] == "response_item"
        && payload["type"] == "agent_message"
        && state
            .agent_path
            .as_deref()
            .is_some_and(|path| payload["recipient"] == path);
    if (row["type"] == "event_msg" && payload["type"] == "thread_settings_applied") || addressed {
        state.inherited_history = false;
        state.turn_id = None;
    }
    if state.inherited_history {
        return;
    }
    if let turn = text(payload, "turn_id") {
        state.turn_id = Some(turn);
    }
    if row["type"] == "turn_context"
        && let Some(cwd) = text(payload, "cwd")
    {
        state.workspace = Some(cwd);
    }
}

fn classify(row: &Value) -> EventKind {
    let payload = &row["payload"];
    match (row["type"].as_str(), payload["type"].as_str()) {
        (Some("event_msg"), Some("user_message")) if runtime_user_context(payload) => {
            EventKind::RuntimeContext
        }
        (Some("event_msg"), Some("user_message")) => EventKind::UserPrompt,
        (Some("event_msg"), Some("task_started")) => EventKind::TaskStarted,
        // Runtime stopped is distinct from verified goal completion.
        (Some("event_msg"), Some("task_complete")) => EventKind::TaskStopped,
        (Some("event_msg"), Some("task_failed" | "error")) => EventKind::TaskFailed,
        (Some("event_msg"), Some("turn_aborted" | "task_cancelled" | "task_canceled")) => {
            EventKind::TaskCancelled
        }
        (Some("response_item"), Some("function_call" | "custom_tool_call")) => EventKind::ToolCall,
        (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
            EventKind::ToolResult
        }
        (Some("response_item"), Some("message")) if payload["role"] == "assistant" => {
            EventKind::AssistantMessage
        }
        (Some("response_item"), Some("message")) => EventKind::RuntimeContext,
        _ => EventKind::SourceRecord,
    }
}

fn runtime_user_context(payload: &Value) -> bool {
    if let Some(kinds) = payload["content_item_kinds"].as_array() {
        if kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| kind.starts_with("user."))
        {
            return false;
        }
        if !kinds.is_empty()
            && kinds.iter().all(|kind| {
                kind.as_str().is_some_and(|kind| {
                    kind.starts_with("environment.")
                        || kind.starts_with("environments.")
                        || kind.starts_with("additional_content.")
                })
            })
        {
            return true;
        }
    }
    let original = payload["message"].as_str().unwrap_or("").trim();
    let mut remaining = original;
    loop {
        let tag = [
            "environment_context",
            "in-app-browser-context",
            "external_codex_apps_open_page",
        ]
        .into_iter()
        .find(|tag| {
            remaining.starts_with(&format!("<{tag}>")) || remaining.starts_with(&format!("<{tag} "))
        });
        let Some(tag) = tag else { break };
        let closing = format!("</{tag}>");
        let Some(end) = remaining.find(&closing) else {
            break;
        };
        remaining = remaining[end + closing.len()..].trim();
    }
    original != remaining && remaining.is_empty()
}
