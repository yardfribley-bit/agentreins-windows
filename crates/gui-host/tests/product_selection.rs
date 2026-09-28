#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gui_host::product::ProductService;
use product_config::ProductConfiguration;
use serde_json::json;

#[test]
fn product_service_can_start_before_any_observation() -> Result<(), String> {
    let root = create_test_root()?;
    let evidence = root.join("evidence");
    fs::create_dir_all(&evidence).map_err(|error| error.to_string())?;
    let manifest = root.join("mcp-unconfigured.json");
    write_json(&manifest, &json!({ "status": "unconfigured" }))?;
    let configuration = ProductConfiguration::new(evidence, manifest, root.join("exports"), 7)?;
    let service = ProductService::new(configuration)?;
    assert!(service.current_selection().is_err(), "must not fabricate a native session");
    fs::remove_dir_all(root).map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn product_service_reselects_new_native_observer_session() -> Result<(), String> {
    let root = create_test_root()?;
    let evidence_root = root.join("evidence");
    let export_root = root.join("exports");
    fs::create_dir_all(&evidence_root)
        .map_err(|error| format!("无法创建测试证据目录 error={error}"))?;
    let mcp_manifest = root.join("mcp-protocol-manifest.json");
    write_json(&mcp_manifest, &json!({ "schema_version": "1.0.0" }))?;
    create_observer_pair(&evidence_root, "native-session-01", 1_000, true)?;

    let configuration =
        ProductConfiguration::new(evidence_root.clone(), mcp_manifest, export_root, 30)?;
    let service = ProductService::new(configuration)?;
    assert_eq!(service.current_selection()?.session_id, "native-session-01");

    create_observer_pair(&evidence_root, "native-session-02", 2_000, true)?;
    assert_eq!(service.current_selection()?.session_id, "native-session-02");
    create_observer_pair(&evidence_root, "native-session-03", 3_000, false)?;
    let selection = service.current_selection()?;
    assert_eq!(selection.session_id, "native-session-03");
    assert_eq!(selection.mcp_manifest, None);

    fs::remove_dir_all(&root).map_err(|error| {
        format!(
            "无法清理动态 Observer 选择测试目录 path={} error={error}",
            root.display()
        )
    })?;
    Ok(())
}

fn create_observer_pair(
    evidence_root: &Path,
    session_id: &str,
    captured_at_unix_ms: u64,
    publish_mcp: bool,
) -> Result<(), String> {
    let os_root = evidence_root.join(format!("os-{session_id}"));
    let semantic_root = evidence_root.join(format!("semantic-{session_id}"));
    fs::create_dir(&os_root)
        .map_err(|error| format!("无法创建测试 OS Observer 目录 error={error}"))?;
    fs::create_dir(&semantic_root)
        .map_err(|error| format!("无法创建测试语义 Observer 目录 error={error}"))?;
    let os_health = os_root.join("health.ndjson");
    let semantic_health = semantic_root.join("health.ndjson");
    write_json_line(
        &os_health,
        &json!({
            "captured_at_unix_ms": captured_at_unix_ms,
            "coverage_status": "healthy",
            "session_id": session_id
        }),
    )?;
    write_json_line(
        &semantic_health,
        &json!({
            "captured_at_unix_ms": captured_at_unix_ms,
            "coverage_status": "healthy"
        }),
    )?;
    write_json(
        &os_root.join("observer-process.json"),
        &json!({ "health_output_path": os_health }),
    )?;
    write_json(
        &semantic_root.join("semantic-observer-process.json"),
        &json!({
            "session_id": session_id,
            "health_output_path": semantic_health
        }),
    )?;
    if publish_mcp {
        write_json(
            &semantic_root.join("mcp-protocol-manifest.json"),
            &json!({
                "schema_version": "1.1.0",
                "session_id": session_id
            }),
        )?;
    }
    Ok(())
}

fn create_test_root() -> Result<PathBuf, String> {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("测试时间早于 Unix 元年 error={error}"))?
        .as_millis();
    let root = std::env::temp_dir().join(format!(
        "agentreins-product-selection-{}-{unix_ms}-{sequence}",
        std::process::id()
    ));
    fs::create_dir(&root).map_err(|error| {
        format!(
            "无法创建动态 Observer 选择测试目录 path={} error={error}",
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
