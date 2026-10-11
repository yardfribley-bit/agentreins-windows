//! One bounded read per invocation. Stdout is one JSON batch; callers must
//! commit its events before committing `next_cursor`. No background polling.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

use codex_adapter::{Cursor, DEFAULT_BATCH_BYTES, read_batch};

fn main() {
    if let Err(error) = run() {
        eprintln!("codex-live-reader: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = None;
    let mut session = None;
    let mut cursor_path = None;
    let mut replay = false;
    let mut maximum_bytes = DEFAULT_BATCH_BYTES;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" => input = Some(PathBuf::from(args.next().ok_or("missing --input value")?)),
            "--session-id" => session = Some(args.next().ok_or("missing --session-id value")?),
            "--cursor" => {
                cursor_path = Some(PathBuf::from(args.next().ok_or("missing --cursor value")?))
            }
            "--maximum-bytes" => {
                maximum_bytes = args
                    .next()
                    .ok_or("missing --maximum-bytes value")?
                    .parse()?
            }
            "--from-start" => replay = true,
            "--help" => {
                println!(
                    "codex-live-reader --input ROLLOUT --session-id NATIVE_ID [--cursor JSON_FILE] [--maximum-bytes N] [--from-start]\nDefault: baseline at EOF. Output: JSON batch with events and next_cursor. Commit evidence before cursor. --from-start is explicit historical replay."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    let input = fs::canonicalize(input.ok_or("--input is required")?)?;
    let session: String = session.ok_or("--session-id is required")?;
    if session.trim().is_empty() || (replay && cursor_path.is_some()) {
        return Err(
            "nonempty session required; --cursor and --from-start are mutually exclusive".into(),
        );
    }
    let stream_id = input.to_string_lossy().into_owned();
    let mut file = File::open(&input)?;
    let cursor = if let Some(path) = cursor_path {
        let cursor: Cursor = serde_json::from_slice(&fs::read(path)?)?;
        if cursor.stream_id != stream_id || cursor.state.session_id != session {
            return Err("cursor belongs to another source or native session".into());
        }
        cursor
    } else if replay {
        Cursor::replay(stream_id, session)
    } else {
        Cursor::baseline(&mut file, stream_id, session)?
    };
    let batch = read_batch(&mut file, &cursor, maximum_bytes)?;
    // Serialize before writing so errors cannot be mistaken for a complete batch.
    let bytes = serde_json::to_vec(&batch)?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
