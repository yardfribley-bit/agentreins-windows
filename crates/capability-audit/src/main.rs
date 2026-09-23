use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use capability_audit::{AuditContext, audit_evidence};
use native_contracts::{ObservationEvent, SemanticEvent};
use serde::de::DeserializeOwned;

struct Arguments {
    semantic_input: PathBuf,
    os_input: PathBuf,
    workspace_root: Option<String>,
    output: PathBuf,
}

fn main() {
    if let Err(error) = run(std::env::args().collect()) {
        eprintln!(
            "{}",
            serde_json::json!({"level": "error", "message": error})
        );
        std::process::exit(1);
    }
}

fn run(raw_arguments: Vec<String>) -> Result<(), String> {
    let arguments = parse_arguments(&raw_arguments)?;
    let semantic_events = read_ndjson::<SemanticEvent>(&arguments.semantic_input)?;
    let os_events = read_ndjson::<ObservationEvent>(&arguments.os_input)?;
    let report = audit_evidence(
        &semantic_events,
        &os_events,
        &AuditContext {
            workspace_root: arguments.workspace_root,
        },
    )?;
    write_report(&arguments.output, &report)?;
    println!(
        "{}",
        serde_json::json!({
            "semantic_events": report.summary.semantic_events,
            "input_os_events": os_events.len(),
            "os_events": report.summary.os_events_considered,
            "excluded_os_events": report.summary.os_events_excluded,
            "capability_requests": report.summary.requests,
            "would_allow": report.summary.would_allow,
            "would_warn": report.summary.would_warn,
            "would_block": report.summary.would_block,
            "enforcement_actions_applied": report.summary.enforcement_actions_applied,
            "output": arguments.output,
        })
    );
    Ok(())
}

fn parse_arguments(raw_arguments: &[String]) -> Result<Arguments, String> {
    let valid_base = raw_arguments.len() == 7
        && raw_arguments[1] == "--semantic-input"
        && raw_arguments[3] == "--os-input"
        && raw_arguments[5] == "--output";
    let valid_workspace = raw_arguments.len() == 9
        && raw_arguments[1] == "--semantic-input"
        && raw_arguments[3] == "--os-input"
        && raw_arguments[5] == "--workspace-root"
        && raw_arguments[7] == "--output";
    if !valid_base && !valid_workspace {
        return Err(String::from(
            "参数错误，用法: capability-audit --semantic-input <semantic.ndjson> --os-input <observation.ndjson> [--workspace-root <path>] --output <audit.json>",
        ));
    }
    let (workspace_root, output_index) = if valid_workspace {
        (Some(raw_arguments[6].clone()), 8)
    } else {
        (None, 6)
    };
    Ok(Arguments {
        semantic_input: PathBuf::from(&raw_arguments[2]),
        os_input: PathBuf::from(&raw_arguments[4]),
        workspace_root,
        output: PathBuf::from(&raw_arguments[output_index]),
    })
}

fn read_ndjson<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let file = File::open(path)
        .map_err(|error| format!("无法打开 Audit 输入 path={} error={error}", path.display()))?;
    let mut values = Vec::new();
    for (index, line_result) in BufReader::new(file).lines().enumerate() {
        let line = line_result.map_err(|error| {
            format!(
                "无法读取 Audit 输入 path={} line={} error={error}",
                path.display(),
                index + 1
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        values.push(serde_json::from_str(&line).map_err(|error| {
            format!(
                "无法解析 Audit 输入 path={} line={} error={error}",
                path.display(),
                index + 1
            )
        })?);
    }
    Ok(values)
}

fn write_report(
    path: &Path,
    report: &capability_audit::CapabilityAuditReport,
) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("无法创建 Audit 报告 path={} error={error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, report).map_err(|error| {
        format!(
            "无法序列化 Audit 报告 path={} error={error}",
            path.display()
        )
    })?;
    writer.write_all(b"\n").map_err(|error| {
        format!(
            "无法写入 Audit 报告换行 path={} error={error}",
            path.display()
        )
    })?;
    writer
        .flush()
        .map_err(|error| format!("无法刷新 Audit 报告 path={} error={error}", path.display()))
}
