use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ProductConfiguration;

const MCP_MANIFEST_FILE_NAME: &str = "mcp-protocol-manifest.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObserverSelection {
    pub session_id: String,
    pub os_run_root: PathBuf,
    pub semantic_run_root: PathBuf,
    pub mcp_manifest: Option<PathBuf>,
}

#[derive(Deserialize)]
struct OsObserverState {
    health_output_path: PathBuf,
}

#[derive(Deserialize)]
struct SemanticObserverState {
    session_id: String,
    health_output_path: PathBuf,
}

#[derive(Deserialize)]
struct HealthRecord {
    captured_at_unix_ms: u64,
    coverage_status: String,
    #[serde(default)]
    session_id: String,
}

struct OsCandidate {
    run_root: PathBuf,
    session_id: String,
    captured_at_unix_ms: u64,
}

struct SemanticCandidate {
    run_root: PathBuf,
    session_id: String,
    captured_at_unix_ms: u64,
    mcp_manifest: Option<PathBuf>,
}

#[derive(Deserialize)]
struct McpEvidenceManifest {
    schema_version: String,
    session_id: String,
}

pub fn select_observer_runs(
    configuration: &ProductConfiguration,
) -> Result<ObserverSelection, String> {
    configuration.validate()?;
    let os_candidates = load_os_candidates(&configuration.evidence_root)?;
    let semantic_candidates = load_semantic_candidates(&configuration.evidence_root)?;
    let mut joined = Vec::new();
    for os_candidate in os_candidates {
        for semantic_candidate in &semantic_candidates {
            if os_candidate.session_id == semantic_candidate.session_id {
                joined.push((
                    os_candidate
                        .captured_at_unix_ms
                        .min(semantic_candidate.captured_at_unix_ms),
                    os_candidate.run_root.clone(),
                    semantic_candidate.run_root.clone(),
                    os_candidate.session_id.clone(),
                    semantic_candidate.mcp_manifest.clone(),
                ));
            }
        }
    }
    joined.sort_by_key(|candidate| candidate.0);
    let (_, os_run_root, semantic_run_root, session_id, mcp_manifest) =
        joined.pop().ok_or_else(|| {
            format!(
                "证据根目录中没有共享原生 session_id 的 OS/语义 Observer path={}",
                configuration.evidence_root.display()
            )
        })?;
    Ok(ObserverSelection {
        session_id,
        os_run_root,
        semantic_run_root,
        mcp_manifest,
    })
}

fn load_os_candidates(evidence_root: &Path) -> Result<Vec<OsCandidate>, String> {
    let mut candidates = Vec::new();
    for run_root in child_directories(evidence_root)? {
        let state_path = run_root.join("observer-process.json");
        if !state_path.is_file() {
            continue;
        }
        let state = read_json::<OsObserverState>(&state_path, "OS Observer 状态")?;
        let health = read_last_health(&state.health_output_path, "OS Observer 健康记录")?;
        if health.session_id.is_empty() {
            return Err(format!(
                "OS Observer 健康记录缺少 session_id path={}",
                state.health_output_path.display()
            ));
        }
        if health.coverage_status != "healthy" && health.coverage_status != "idle" {
            continue;
        }
        candidates.push(OsCandidate {
            run_root,
            session_id: health.session_id,
            captured_at_unix_ms: health.captured_at_unix_ms,
        });
    }
    Ok(candidates)
}

fn load_semantic_candidates(evidence_root: &Path) -> Result<Vec<SemanticCandidate>, String> {
    let mut candidates = Vec::new();
    for run_root in child_directories(evidence_root)? {
        let state_path = run_root.join("semantic-observer-process.json");
        if !state_path.is_file() {
            continue;
        }
        let state = read_json::<SemanticObserverState>(&state_path, "语义 Observer 状态")?;
        let health = read_last_health(&state.health_output_path, "语义 Observer 健康记录")?;
        if health.coverage_status != "healthy" {
            continue;
        }
        let mcp_manifest_path = run_root.join(MCP_MANIFEST_FILE_NAME);
        let mcp_manifest = load_mcp_manifest(&mcp_manifest_path, &state.session_id)?;
        candidates.push(SemanticCandidate {
            run_root,
            session_id: state.session_id,
            captured_at_unix_ms: health.captured_at_unix_ms,
            mcp_manifest,
        });
    }
    Ok(candidates)
}

fn load_mcp_manifest(path: &Path, session_id: &str) -> Result<Option<PathBuf>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let manifest = read_json::<McpEvidenceManifest>(path, "MCP 会话发布 manifest")?;
    if manifest.schema_version != "1.1.0" {
        return Err(format!(
            "不支持的 MCP 会话发布契约 expected=1.1.0 actual={} path={}",
            manifest.schema_version,
            path.display()
        ));
    }
    if manifest.session_id != session_id {
        return Err(format!(
            "MCP 发布物与语义 Observer 会话不匹配 expected={} actual={} path={}",
            session_id,
            manifest.session_id,
            path.display()
        ));
    }
    Ok(Some(path.to_path_buf()))
}

fn child_directories(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut directories = Vec::new();
    let entries = fs::read_dir(root)
        .map_err(|error| format!("无法枚举证据根目录 path={} error={error}", root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!("无法读取证据根目录项 path={} error={error}", root.display())
        })?;
        let path = entry.path();
        if path.is_dir() {
            directories.push(path);
        }
    }
    Ok(directories)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("无法读取{label} path={} error={error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("无法解析{label} path={} error={error}", path.display()))
}

fn read_last_health(path: &Path, label: &str) -> Result<HealthRecord, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("无法读取{label} path={} error={error}", path.display()))?;
    let mut position = file
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("无法定位{label}末尾 path={} error={error}", path.display()))?;
    let mut suffix = Vec::new();
    while position > 0 {
        let read_size = position.min(8_192) as usize;
        position -= read_size as u64;
        file.seek(SeekFrom::Start(position)).map_err(|error| {
            format!(
                "无法定位{label} path={} offset={position} error={error}",
                path.display()
            )
        })?;
        let mut chunk = vec![0_u8; read_size];
        file.read_exact(&mut chunk).map_err(|error| {
            format!(
                "无法读取{label} path={} offset={position} error={error}",
                path.display()
            )
        })?;
        chunk.extend_from_slice(&suffix);
        suffix = chunk;
        let Some(line_end) = suffix
            .iter()
            .rposition(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t'))
            .map(|index| index + 1)
        else {
            continue;
        };
        if let Some(line_start) = suffix[..line_end].iter().rposition(|byte| *byte == b'\n') {
            return parse_health_line(&suffix[line_start + 1..line_end], path, label);
        }
        if position == 0 {
            return parse_health_line(&suffix[..line_end], path, label);
        }
    }
    Err(format!("{label}为空 path={}", path.display()))
}

fn parse_health_line(contents: &[u8], path: &Path, label: &str) -> Result<HealthRecord, String> {
    let line = std::str::from_utf8(contents).map_err(|error| {
        format!(
            "{label}末行不是 UTF-8 path={} error={error}",
            path.display()
        )
    })?;
    serde_json::from_str(line)
        .map_err(|error| format!("无法解析{label}末行 path={} error={error}", path.display()))
}
