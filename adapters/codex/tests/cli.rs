use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("agentreins-codex-{}-{stamp}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("rollout.jsonl"),
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"own\"}}\n",
        )
        .unwrap();
        Self(directory)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codex-live-reader"))
            .arg("--input")
            .arg(self.0.join("rollout.jsonl"))
            .args(["--session-id", "own"])
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn executable_baselines_then_reads_only_appended_records() {
    let fixture = Fixture::new();
    let initial = fixture.run(&[]);
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let initial: Value = serde_json::from_slice(&initial.stdout).unwrap();
    assert_eq!(initial["events"].as_array().unwrap().len(), 0);
    let cursor_path = fixture.0.join("cursor.json");
    fs::write(
        &cursor_path,
        serde_json::to_vec(&initial["next_cursor"]).unwrap(),
    )
    .unwrap();
    OpenOptions::new().append(true).open(fixture.0.join("rollout.jsonl")).unwrap()
        .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"new request\",\"turn_id\":\"t1\"}}\n").unwrap();
    let batch = fixture.run(&["--cursor", cursor_path.to_str().unwrap()]);
    assert!(
        batch.status.success(),
        "{}",
        String::from_utf8_lossy(&batch.stderr)
    );
    let batch: Value = serde_json::from_slice(&batch.stdout).unwrap();
    assert_eq!(batch["events"].as_array().unwrap().len(), 1);
    assert_eq!(batch["events"][0]["kind"], "user_prompt");
    assert_eq!(batch["events"][0]["turn_id"], "t1");
}

#[test]
fn invalid_cursor_and_mutually_exclusive_options_fail_without_batch_output() {
    let fixture = Fixture::new();
    let cursor_path = fixture.0.join("cursor.json");
    fs::write(&cursor_path, b"{}").unwrap();
    let failed = fixture.run(&["--cursor", cursor_path.to_str().unwrap()]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let failed = fixture.run(&["--cursor", cursor_path.to_str().unwrap(), "--from-start"]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
}
