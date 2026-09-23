use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::CompressionMethod;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::{ObserverSelection, ProductConfiguration};

const MILLISECONDS_PER_DAY: u64 = 86_400_000;

#[derive(Clone, Debug, Serialize)]
pub struct RetentionCandidate {
    pub run_root: PathBuf,
    pub last_modified_unix_ms: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RetentionPlan {
    pub retention_days: u16,
    pub cutoff_unix_ms: u64,
    pub candidates: Vec<RetentionCandidate>,
    pub confirmation_token: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RetentionApplication {
    pub deleted_directories: Vec<PathBuf>,
    pub deleted_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticExportSummary {
    pub archive_path: PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Deserialize)]
struct OsObserverState {
    output_directory: PathBuf,
}

#[derive(Deserialize)]
struct SemanticObserverState {
    semantic_manifest_path: PathBuf,
}

#[derive(Deserialize)]
struct RollingManifest {
    schema_version: String,
    segments: Vec<RollingSegment>,
}

#[derive(Deserialize)]
struct RollingSegment {
    full_file: String,
    filtered_file: String,
    full_bytes: u64,
    filtered_bytes: u64,
}

#[derive(Deserialize)]
struct SemanticManifest {
    schema_version: String,
    semantic_file: PathBuf,
    published_bytes: u64,
}

#[derive(Deserialize)]
struct McpManifest {
    schema_version: String,
    session_id: String,
    mcp_file: PathBuf,
    published_bytes: u64,
}

#[derive(Serialize)]
struct DiagnosticArchiveManifest {
    schema_version: &'static str,
    entries: Vec<DiagnosticArchiveEntry>,
}

#[derive(Serialize)]
struct DiagnosticArchiveEntry {
    archive_path: String,
    source_path: PathBuf,
    published_bytes: u64,
    sha256: String,
}

pub fn create_retention_plan(
    configuration: &ProductConfiguration,
    selection: &ObserverSelection,
    now_unix_ms: u64,
) -> Result<RetentionPlan, String> {
    configuration.validate()?;
    let retention_ms = u64::from(configuration.retention_days)
        .checked_mul(MILLISECONDS_PER_DAY)
        .ok_or_else(|| String::from("证据保留周期溢出"))?;
    let cutoff_unix_ms = now_unix_ms
        .checked_sub(retention_ms)
        .ok_or_else(|| String::from("当前时间小于证据保留周期"))?;
    let protected_roots = [
        selection.os_run_root.as_path(),
        selection.semantic_run_root.as_path(),
    ];
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&configuration.evidence_root).map_err(|error| {
        format!(
            "无法枚举证据根目录 path={} error={error}",
            configuration.evidence_root.display()
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "无法读取证据根目录项 path={} error={error}",
                configuration.evidence_root.display()
            )
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!("无法读取证据目录属性 path={} error={error}", path.display())
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        if protected_roots.contains(&path.as_path()) || !is_observer_run(&path) {
            continue;
        }
        let modified_unix_ms = directory_latest_modified_unix_ms(&path)?;
        if modified_unix_ms >= cutoff_unix_ms {
            continue;
        }
        candidates.push(RetentionCandidate {
            bytes: directory_bytes(&path)?,
            run_root: path,
            last_modified_unix_ms: modified_unix_ms,
        });
    }
    candidates.sort_by(|left, right| left.run_root.cmp(&right.run_root));
    let confirmation_token = retention_token(configuration.retention_days, &candidates);
    Ok(RetentionPlan {
        retention_days: configuration.retention_days,
        cutoff_unix_ms,
        candidates,
        confirmation_token,
    })
}

pub fn apply_retention_plan(
    configuration: &ProductConfiguration,
    selection: &ObserverSelection,
    confirmation_token: &str,
    now_unix_ms: u64,
) -> Result<RetentionApplication, String> {
    let plan = create_retention_plan(configuration, selection, now_unix_ms)?;
    if plan.confirmation_token != confirmation_token {
        return Err(String::from("证据保留确认令牌不匹配，必须重新预览"));
    }
    let mut deleted_directories = Vec::new();
    let mut deleted_bytes = 0_u64;
    for candidate in plan.candidates {
        fs::remove_dir_all(&candidate.run_root).map_err(|error| {
            format!(
                "无法删除过期 Observer 运行目录 path={} error={error}",
                candidate.run_root.display()
            )
        })?;
        deleted_bytes = deleted_bytes
            .checked_add(candidate.bytes)
            .ok_or_else(|| String::from("删除证据字节数溢出"))?;
        deleted_directories.push(candidate.run_root);
    }
    Ok(RetentionApplication {
        deleted_directories,
        deleted_bytes,
    })
}

pub fn create_diagnostic_export(
    configuration: &ProductConfiguration,
    selection: &ObserverSelection,
    archive_name: &str,
) -> Result<DiagnosticExportSummary, String> {
    configuration.validate()?;
    validate_archive_name(archive_name)?;
    fs::create_dir_all(&configuration.diagnostic_export_root).map_err(|error| {
        format!(
            "无法创建诊断导出目录 path={} error={error}",
            configuration.diagnostic_export_root.display()
        )
    })?;
    let archive_path = configuration.diagnostic_export_root.join(archive_name);
    if archive_path.exists() {
        return Err(format!(
            "诊断导出已存在，拒绝覆盖 path={}",
            archive_path.display()
        ));
    }
    let temporary_path = configuration
        .diagnostic_export_root
        .join(format!("{archive_name}.partial"));
    if temporary_path.exists() {
        return Err(format!(
            "诊断导出临时文件已存在，拒绝覆盖 path={}",
            temporary_path.display()
        ));
    }
    let export_result = write_diagnostic_archive(&temporary_path, selection);
    let (files, bytes) = match export_result {
        Ok(summary) => summary,
        Err(export_error) => {
            return Err(remove_failed_export(&temporary_path, export_error));
        }
    };
    if let Err(rename_error) = fs::rename(&temporary_path, &archive_path) {
        let export_error = format!(
            "无法原子发布诊断 ZIP source={} target={} error={rename_error}",
            temporary_path.display(),
            archive_path.display()
        );
        return Err(remove_failed_export(&temporary_path, export_error));
    }
    let sha256 = sha256_file(&archive_path)?;
    Ok(DiagnosticExportSummary {
        archive_path,
        files,
        bytes,
        sha256,
    })
}

fn write_diagnostic_archive(
    temporary_path: &Path,
    selection: &ObserverSelection,
) -> Result<(u64, u64), String> {
    let archive_file = File::options()
        .create_new(true)
        .write(true)
        .open(temporary_path)
        .map_err(|error| {
            format!(
                "无法创建诊断导出临时文件 path={} error={error}",
                temporary_path.display()
            )
        })?;
    let mut zip = ZipWriter::new(archive_file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut entries = Vec::new();
    append_os_evidence(&mut zip, &options, selection, &mut entries)?;
    append_semantic_evidence(&mut zip, &options, selection, &mut entries)?;
    if selection.mcp_manifest.is_some() {
        append_mcp_evidence(&mut zip, &options, selection, &mut entries)?;
    }
    let source_files = u64::try_from(entries.len())
        .map_err(|error| format!("诊断导出文件数超出范围 error={error}"))?;
    let source_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.published_bytes)
            .ok_or_else(|| String::from("诊断导出字节数溢出"))
    })?;
    let diagnostic_manifest = serde_json::to_vec_pretty(&DiagnosticArchiveManifest {
        schema_version: "0.1.0",
        entries,
    })
    .map_err(|error| format!("无法序列化诊断导出 manifest error={error}"))?;
    append_bytes(
        &mut zip,
        &options,
        "diagnostic-manifest.json",
        &diagnostic_manifest,
    )?;
    zip.finish().map_err(|error| {
        format!(
            "无法完成诊断 ZIP path={} error={error}",
            temporary_path.display()
        )
    })?;
    let manifest_bytes = u64::try_from(diagnostic_manifest.len())
        .map_err(|error| format!("诊断导出 manifest 长度超出范围 error={error}"))?;
    Ok((source_files + 1, source_bytes + manifest_bytes))
}

fn remove_failed_export(temporary_path: &Path, export_error: String) -> String {
    match fs::remove_file(temporary_path) {
        Ok(()) => export_error,
        Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => export_error,
        Err(cleanup_error) => format!(
            "{export_error}; 无法清理诊断导出临时文件 path={} error={cleanup_error}",
            temporary_path.display()
        ),
    }
}

fn append_os_evidence(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    selection: &ObserverSelection,
    entries: &mut Vec<DiagnosticArchiveEntry>,
) -> Result<(), String> {
    let state_path = selection.os_run_root.join("observer-process.json");
    let state: OsObserverState = read_json_file(&state_path, "OS Observer 状态")?;
    let manifest_path = latest_rolling_manifest(&state.output_directory)?;
    let (manifest, manifest_bytes) =
        read_manifest::<RollingManifest>(&manifest_path, "OS 证据 manifest")?;
    if manifest.schema_version != "0.1.0" {
        return Err(format!(
            "不支持的 OS 证据 manifest 版本 path={} schema_version={}",
            manifest_path.display(),
            manifest.schema_version
        ));
    }
    entries.push(append_bytes_from_source(
        zip,
        options,
        "os/rolling-manifest.json",
        &manifest_path,
        &manifest_bytes,
    )?);
    for segment in manifest.segments {
        let full_path = manifest_child(&state.output_directory, &segment.full_file)?;
        let filtered_path = manifest_child(&state.output_directory, &segment.filtered_file)?;
        entries.push(append_file_prefix(
            zip,
            options,
            &format!("os/segments/{}", segment.full_file),
            &full_path,
            segment.full_bytes,
        )?);
        entries.push(append_file_prefix(
            zip,
            options,
            &format!("os/segments/{}", segment.filtered_file),
            &filtered_path,
            segment.filtered_bytes,
        )?);
    }
    Ok(())
}

fn append_semantic_evidence(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    selection: &ObserverSelection,
    entries: &mut Vec<DiagnosticArchiveEntry>,
) -> Result<(), String> {
    let state_path = selection
        .semantic_run_root
        .join("semantic-observer-process.json");
    let state: SemanticObserverState = read_json_file(&state_path, "语义 Observer 状态")?;
    let (manifest, manifest_bytes) =
        read_manifest::<SemanticManifest>(&state.semantic_manifest_path, "语义证据 manifest")?;
    if manifest.schema_version != "0.1.0" {
        return Err(format!(
            "不支持的语义证据 manifest 版本 path={} schema_version={}",
            state.semantic_manifest_path.display(),
            manifest.schema_version
        ));
    }
    entries.push(append_bytes_from_source(
        zip,
        options,
        "semantic/semantic-evidence-manifest.json",
        &state.semantic_manifest_path,
        &manifest_bytes,
    )?);
    entries.push(append_file_prefix(
        zip,
        options,
        "semantic/workbuddy-semantic.full.ndjson",
        &manifest.semantic_file,
        manifest.published_bytes,
    )?);
    Ok(())
}

fn append_mcp_evidence(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    selection: &ObserverSelection,
    entries: &mut Vec<DiagnosticArchiveEntry>,
) -> Result<(), String> {
    let manifest_path = selection
        .mcp_manifest
        .as_ref()
        .ok_or_else(|| String::from("当前会话没有 MCP 原生发布物"))?;
    let (manifest, manifest_bytes) =
        read_manifest::<McpManifest>(manifest_path, "MCP 证据 manifest")?;
    if manifest.schema_version != "1.1.0" {
        return Err(format!(
            "不支持的 MCP 证据 manifest 版本 path={} schema_version={}",
            manifest_path.display(),
            manifest.schema_version
        ));
    }
    if manifest.session_id != selection.session_id {
        return Err(format!(
            "MCP 证据 manifest 与选中会话不匹配 path={} expected={} actual={}",
            manifest_path.display(),
            selection.session_id,
            manifest.session_id
        ));
    }
    entries.push(append_bytes_from_source(
        zip,
        options,
        "mcp/mcp-protocol-manifest.json",
        manifest_path,
        &manifest_bytes,
    )?);
    entries.push(append_file_prefix(
        zip,
        options,
        "mcp/mcp-protocol.ndjson",
        &manifest.mcp_file,
        manifest.published_bytes,
    )?);
    Ok(())
}

fn append_file_prefix(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    entry_name: &str,
    source: &Path,
    published_bytes: u64,
) -> Result<DiagnosticArchiveEntry, String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("无法读取诊断源属性 path={} error={error}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("诊断源必须是普通文件 path={}", source.display()));
    }
    if metadata.len() < published_bytes {
        return Err(format!(
            "诊断源长度小于已发布边界 path={} published_bytes={} actual_bytes={}",
            source.display(),
            published_bytes,
            metadata.len()
        ));
    }
    zip.start_file(entry_name, *options)
        .map_err(|error| format!("无法创建 ZIP 条目 entry={entry_name} error={error}"))?;
    let input = File::open(source)
        .map_err(|error| format!("无法读取诊断源文件 path={} error={error}", source.display()))?;
    let mut limited = input.take(published_bytes);
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 65_536];
    loop {
        let read = limited.read(&mut buffer).map_err(|error| {
            format!("无法读取诊断源前缀 path={} error={error}", source.display())
        })?;
        if read == 0 {
            break;
        }
        zip.write_all(&buffer[..read])
            .map_err(|error| format!("无法写入 ZIP 条目 entry={entry_name} error={error}"))?;
        hasher.update(&buffer[..read]);
        copied = copied
            .checked_add(u64::try_from(read).map_err(|error| {
                format!(
                    "诊断源读取长度超出范围 path={} error={error}",
                    source.display()
                )
            })?)
            .ok_or_else(|| String::from("诊断导出字节数溢出"))?;
    }
    if copied != published_bytes {
        return Err(format!(
            "诊断源读取未达到已发布边界 path={} expected={} actual={}",
            source.display(),
            published_bytes,
            copied
        ));
    }
    Ok(DiagnosticArchiveEntry {
        archive_path: String::from(entry_name),
        source_path: source.to_path_buf(),
        published_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn append_bytes_from_source(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    entry_name: &str,
    source: &Path,
    contents: &[u8],
) -> Result<DiagnosticArchiveEntry, String> {
    append_bytes(zip, options, entry_name, contents)?;
    Ok(DiagnosticArchiveEntry {
        archive_path: String::from(entry_name),
        source_path: source.to_path_buf(),
        published_bytes: u64::try_from(contents.len()).map_err(|error| {
            format!("诊断源长度超出范围 path={} error={error}", source.display())
        })?,
        sha256: format!("{:x}", Sha256::digest(contents)),
    })
}

fn append_bytes(
    zip: &mut ZipWriter<File>,
    options: &SimpleFileOptions,
    entry_name: &str,
    contents: &[u8],
) -> Result<(), String> {
    zip.start_file(entry_name, *options)
        .map_err(|error| format!("无法创建 ZIP 条目 entry={entry_name} error={error}"))?;
    zip.write_all(contents)
        .map_err(|error| format!("无法写入 ZIP 条目 entry={entry_name} error={error}"))
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T, String> {
    let contents = fs::read(path)
        .map_err(|error| format!("无法读取{label} path={} error={error}", path.display()))?;
    serde_json::from_slice(&contents)
        .map_err(|error| format!("无法解析{label} path={} error={error}", path.display()))
}

fn read_manifest<T: for<'de> Deserialize<'de>>(
    path: &Path,
    label: &str,
) -> Result<(T, Vec<u8>), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取{label}属性 path={} error={error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label}必须是普通文件 path={}", path.display()));
    }
    let contents = fs::read(path)
        .map_err(|error| format!("无法读取{label} path={} error={error}", path.display()))?;
    let manifest = serde_json::from_slice(&contents)
        .map_err(|error| format!("无法解析{label} path={} error={error}", path.display()))?;
    Ok((manifest, contents))
}

fn latest_rolling_manifest(directory: &Path) -> Result<PathBuf, String> {
    let mut candidates = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "无法枚举 OS 证据目录 path={} error={error}",
                directory.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "无法读取 OS 证据目录项 path={} error={error}",
                directory.display()
            )
        })?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("-manifest-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.pop().ok_or_else(|| {
        format!(
            "OS Observer 尚无已发布 manifest path={}",
            directory.display()
        )
    })
}

fn manifest_child(directory: &Path, file_name: &str) -> Result<PathBuf, String> {
    let path = Path::new(file_name);
    let mut components = path.components();
    let valid =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !valid {
        return Err(format!("证据 manifest 文件名非法 name={file_name}"));
    }
    Ok(directory.join(path))
}

fn directory_bytes(path: &Path) -> Result<u64, String> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)
        .map_err(|error| format!("无法枚举证据目录 path={} error={error}", path.display()))?
    {
        let entry = entry
            .map_err(|error| format!("无法读取证据目录项 path={} error={error}", path.display()))?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            format!(
                "无法读取证据属性 path={} error={error}",
                entry_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "证据目录包含符号链接 path={}",
                entry_path.display()
            ));
        }
        let entry_bytes = if metadata.is_dir() {
            directory_bytes(&entry_path)?
        } else {
            metadata.len()
        };
        total = total
            .checked_add(entry_bytes)
            .ok_or_else(|| String::from("证据目录字节数溢出"))?;
    }
    Ok(total)
}

fn directory_latest_modified_unix_ms(path: &Path) -> Result<u64, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取证据属性 path={} error={error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("证据目录包含符号链接 path={}", path.display()));
    }
    let mut latest =
        system_time_to_unix_ms(metadata.modified().map_err(|error| {
            format!("无法读取证据修改时间 path={} error={error}", path.display())
        })?)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|error| format!("无法枚举证据目录 path={} error={error}", path.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("无法读取证据目录项 path={} error={error}", path.display())
            })?;
            latest = latest.max(directory_latest_modified_unix_ms(&entry.path())?);
        }
    }
    Ok(latest)
}

fn retention_token(retention_days: u16, candidates: &[RetentionCandidate]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(retention_days.to_le_bytes());
    for candidate in candidates {
        hasher.update(candidate.run_root.as_os_str().to_string_lossy().as_bytes());
        hasher.update(candidate.last_modified_unix_ms.to_le_bytes());
        hasher.update(candidate.bytes.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut input = File::open(path)
        .map_err(|error| format!("无法读取导出哈希源 path={} error={error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        let read = input.read(&mut buffer).map_err(|error| {
            format!("无法读取导出哈希数据 path={} error={error}", path.display())
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_archive_name(name: &str) -> Result<(), String> {
    if !name.ends_with(".zip") || name.len() > 120 {
        return Err(format!(
            "诊断导出文件名必须以 .zip 结尾且不超过 120 个字符 name={name}"
        ));
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(format!("诊断导出文件名包含非法字符 name={name}"));
    }
    Ok(())
}

fn is_observer_run(path: &Path) -> bool {
    path.join("observer-process.json").is_file()
        || path.join("semantic-observer-process.json").is_file()
}

fn system_time_to_unix_ms(value: SystemTime) -> Result<u64, String> {
    let duration = value
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("文件时间早于 Unix 元年 error={error}"))?;
    u64::try_from(duration.as_millis())
        .map_err(|error| format!("文件 Unix 毫秒超出 u64 error={error}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{RetentionCandidate, retention_token, validate_archive_name};

    #[test]
    fn rejects_diagnostic_export_path_traversal() {
        assert!(validate_archive_name("agentreins-diagnostic-1.zip").is_ok());
        assert!(validate_archive_name("../diagnostic.zip").is_err());
        assert!(validate_archive_name("diagnostic.txt").is_err());
    }

    #[test]
    fn retention_token_changes_with_candidate_facts() {
        let first = RetentionCandidate {
            run_root: PathBuf::from(r"C:\evidence\run-01"),
            last_modified_unix_ms: 100,
            bytes: 200,
        };
        let second = RetentionCandidate {
            run_root: PathBuf::from(r"C:\evidence\run-01"),
            last_modified_unix_ms: 101,
            bytes: 200,
        };
        assert_ne!(
            retention_token(30, &[first]),
            retention_token(30, &[second])
        );
    }
}
