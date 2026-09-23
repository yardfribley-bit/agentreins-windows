#![cfg(windows)]

use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use product_config::{
    ProductConfiguration, apply_retention_plan, create_diagnostic_export, create_retention_plan,
    load_encrypted, save_encrypted, select_observer_runs,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

const DAY_MILLISECONDS: u64 = 86_400_000;
const UNPUBLISHED_TAIL: &[u8] = b"{\"unpublished\":true}\n";

#[derive(Deserialize)]
struct DiagnosticManifest {
    schema_version: String,
    entries: Vec<DiagnosticEntry>,
}

#[derive(Deserialize)]
struct DiagnosticEntry {
    archive_path: String,
    published_bytes: u64,
    sha256: String,
}

#[test]
fn product_data_lifecycle_uses_dpapi_native_selection_and_explicit_actions() -> Result<(), String> {
    let root = create_test_root()?;
    let evidence_root = root.join("evidence");
    let os_root = evidence_root.join("os-current");
    let semantic_root = evidence_root.join("semantic-current");
    let expired_root = evidence_root.join("os-expired");
    let export_root = root.join("exports");
    fs::create_dir_all(&os_root).map_err(|error| format!("无法创建测试 OS 目录 error={error}"))?;
    fs::create_dir_all(&semantic_root)
        .map_err(|error| format!("无法创建测试语义目录 error={error}"))?;
    fs::create_dir_all(&expired_root)
        .map_err(|error| format!("无法创建测试过期目录 error={error}"))?;

    let captured_at_unix_ms = current_unix_ms()?;
    let os_health = os_root.join("health.ndjson");
    let semantic_health = semantic_root.join("health.ndjson");
    let expired_health = expired_root.join("health.ndjson");
    write_json_line(
        &os_health,
        &json!({
            "captured_at_unix_ms": captured_at_unix_ms,
            "coverage_status": "healthy",
            "session_id": "native-session-01"
        }),
    )?;
    write_json_line(
        &semantic_health,
        &json!({
            "captured_at_unix_ms": captured_at_unix_ms,
            "coverage_status": "healthy"
        }),
    )?;
    write_json_line(
        &expired_health,
        &json!({
            "captured_at_unix_ms": captured_at_unix_ms,
            "coverage_status": "degraded",
            "session_id": "expired-session"
        }),
    )?;
    write_json(
        &os_root.join("observer-process.json"),
        &json!({
            "health_output_path": os_health,
            "output_directory": os_root
        }),
    )?;
    let semantic_manifest = semantic_root.join("semantic-evidence-manifest.json");
    write_json(
        &semantic_root.join("semantic-observer-process.json"),
        &json!({
            "session_id": "native-session-01",
            "health_output_path": semantic_health,
            "semantic_manifest_path": semantic_manifest
        }),
    )?;
    write_json(
        &expired_root.join("observer-process.json"),
        &json!({ "health_output_path": expired_health }),
    )?;
    let os_full_published = b"{\"kind\":\"process\"}\n";
    let os_filtered_published = b"{\"kind\":\"filtered\"}\n";
    let semantic_published = b"{\"kind\":\"tool\"}\n";
    let mcp_published = b"{\"jsonrpc\":\"2.0\",\"id\":1}\n";
    let os_full = os_root.join("native-session-01-000001.full.ndjson");
    let os_filtered = os_root.join("native-session-01-000001.filtered.ndjson");
    let semantic_file = semantic_root.join("workbuddy-semantic.full.ndjson");
    write_with_unpublished_tail(&os_full, os_full_published)?;
    write_with_unpublished_tail(&os_filtered, os_filtered_published)?;
    write_with_unpublished_tail(&semantic_file, semantic_published)?;
    write_json(
        &os_root.join("native-session-01-manifest-000001.json"),
        &json!({
            "schema_version": "0.1.0",
            "session_id": "native-session-01",
            "complete": false,
            "segments": [{
                "full_file": "native-session-01-000001.full.ndjson",
                "filtered_file": "native-session-01-000001.filtered.ndjson",
                "full_bytes": os_full_published.len(),
                "filtered_bytes": os_filtered_published.len()
            }]
        }),
    )?;
    write_json(
        &semantic_manifest,
        &json!({
            "schema_version": "0.1.0",
            "session_id": "native-session-01",
            "semantic_file": semantic_file,
            "published_bytes": semantic_published.len(),
            "semantic_events": 1
        }),
    )?;

    let mcp_file = root.join("mcp-protocol.ndjson");
    let mcp_manifest = root.join("mcp-protocol-manifest.json");
    write_with_unpublished_tail(&mcp_file, mcp_published)?;
    write_json(
        &mcp_manifest,
        &json!({
            "schema_version": "1.0.0",
            "mcp_file": mcp_file,
            "published_bytes": mcp_published.len(),
            "protocol_records": 1,
            "complete": true
        }),
    )?;
    let session_mcp_manifest = semantic_root.join("mcp-protocol-manifest.json");
    write_json(
        &session_mcp_manifest,
        &json!({
            "schema_version": "1.1.0",
            "session_id": "native-session-01",
            "mcp_file": mcp_file,
            "published_bytes": mcp_published.len(),
            "protocol_records": 1,
            "complete": true,
            "source_manifest": mcp_manifest
        }),
    )?;

    let configuration =
        ProductConfiguration::new(evidence_root, mcp_manifest.clone(), export_root, 30)?;
    let configuration_path = root.join("product-config.bin");
    save_encrypted(&configuration_path, &configuration)?;
    assert_eq!(load_encrypted(&configuration_path)?, configuration);

    let selection = select_observer_runs(&configuration)?;
    assert_eq!(selection.session_id, "native-session-01");
    assert_eq!(selection.os_run_root, os_root);
    assert_eq!(selection.semantic_run_root, semantic_root);
    assert_eq!(selection.mcp_manifest, Some(session_mcp_manifest));

    let future_unix_ms = captured_at_unix_ms
        .checked_add(31 * DAY_MILLISECONDS)
        .ok_or_else(|| String::from("测试时间溢出"))?;
    let plan = create_retention_plan(&configuration, &selection, future_unix_ms)?;
    assert_eq!(plan.candidates.len(), 1);
    assert_eq!(plan.candidates[0].run_root, expired_root);
    let application = apply_retention_plan(
        &configuration,
        &selection,
        &plan.confirmation_token,
        future_unix_ms,
    )?;
    assert_eq!(application.deleted_directories, vec![expired_root.clone()]);
    assert!(!expired_root.exists());

    let export =
        create_diagnostic_export(&configuration, &selection, "agentreins-diagnostic-test.zip")?;
    assert!(export.archive_path.is_file());
    assert_eq!(export.files, 8);
    assert_eq!(export.sha256.len(), 64);
    assert!(!export.archive_path.with_extension("zip.partial").exists());
    let archive_file = File::open(&export.archive_path)
        .map_err(|error| format!("无法打开诊断测试 ZIP error={error}"))?;
    let mut archive = ZipArchive::new(archive_file)
        .map_err(|error| format!("无法解析诊断测试 ZIP error={error}"))?;
    assert_eq!(
        read_zip_entry(
            &mut archive,
            "os/segments/native-session-01-000001.full.ndjson"
        )?,
        os_full_published
    );
    assert_eq!(
        read_zip_entry(
            &mut archive,
            "os/segments/native-session-01-000001.filtered.ndjson"
        )?,
        os_filtered_published
    );
    assert_eq!(
        read_zip_entry(&mut archive, "semantic/workbuddy-semantic.full.ndjson")?,
        semantic_published
    );
    assert_eq!(
        read_zip_entry(&mut archive, "mcp/mcp-protocol.ndjson")?,
        mcp_published
    );
    let diagnostic_manifest = serde_json::from_slice::<DiagnosticManifest>(&read_zip_entry(
        &mut archive,
        "diagnostic-manifest.json",
    )?)
    .map_err(|error| format!("无法解析诊断测试 manifest error={error}"))?;
    assert_eq!(diagnostic_manifest.schema_version, "0.1.0");
    assert_eq!(diagnostic_manifest.entries.len(), 7);
    let mcp_entry = diagnostic_manifest
        .entries
        .iter()
        .find(|entry| entry.archive_path == "mcp/mcp-protocol.ndjson")
        .ok_or_else(|| String::from("诊断测试 manifest 缺少 MCP 条目"))?;
    assert_eq!(mcp_entry.published_bytes, mcp_published.len() as u64);
    assert_eq!(
        mcp_entry.sha256,
        format!("{:x}", Sha256::digest(mcp_published))
    );

    fs::remove_dir_all(&root).map_err(|error| {
        format!(
            "无法清理产品生命周期测试目录 path={} error={error}",
            root.display()
        )
    })?;
    Ok(())
}

fn write_with_unpublished_tail(path: &Path, published: &[u8]) -> Result<(), String> {
    let mut contents = Vec::with_capacity(published.len() + UNPUBLISHED_TAIL.len());
    contents.extend_from_slice(published);
    contents.extend_from_slice(UNPUBLISHED_TAIL);
    fs::write(path, contents).map_err(|error| {
        format!(
            "无法写入带未发布尾部的测试证据 path={} error={error}",
            path.display()
        )
    })
}

fn read_zip_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_name(name)
        .map_err(|error| format!("诊断测试 ZIP 缺少条目 name={name} error={error}"))?;
    let mut contents = Vec::new();
    entry
        .read_to_end(&mut contents)
        .map_err(|error| format!("无法读取诊断测试 ZIP 条目 name={name} error={error}"))?;
    Ok(contents)
}

fn create_test_root() -> Result<PathBuf, String> {
    let root = std::env::temp_dir().join(format!(
        "agentreins-product-lifecycle-{}-{}",
        std::process::id(),
        current_unix_ms()?
    ));
    fs::create_dir(&root).map_err(|error| {
        format!(
            "无法创建产品生命周期测试根目录 path={} error={error}",
            root.display()
        )
    })?;
    Ok(root)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let contents = serde_json::to_vec(value)
        .map_err(|error| format!("无法序列化测试 JSON path={} error={error}", path.display()))?;
    fs::write(path, contents)
        .map_err(|error| format!("无法写入测试 JSON path={} error={error}", path.display()))
}

fn write_json_line(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let mut contents = serde_json::to_vec(value).map_err(|error| {
        format!(
            "无法序列化测试 NDJSON path={} error={error}",
            path.display()
        )
    })?;
    contents.push(b'\n');
    fs::write(path, contents)
        .map_err(|error| format!("无法写入测试 NDJSON path={} error={error}", path.display()))
}

fn current_unix_ms() -> Result<u64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("测试时间早于 Unix 元年 error={error}"))?;
    u64::try_from(duration.as_millis())
        .map_err(|error| format!("测试 Unix 毫秒超出 u64 error={error}"))
}
