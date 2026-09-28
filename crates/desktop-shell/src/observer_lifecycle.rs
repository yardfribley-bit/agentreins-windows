use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use product_config::ProductConfiguration;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use windows_service::service::{Service, ServiceAccess, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE_NAME: &str = "AgentReinsObserver";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SERVICE_DOES_NOT_EXIST: i32 = 1060;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum LifecycleState {
    NotInstalled,
    Starting,
    Running,
    Idle,
    Degraded,
    Stopped,
    IdentityMismatch,
    PermissionRequired,
    Failed,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleSnapshot {
    schema_version: &'static str,
    updated_at_unix_ms: u64,
    state: LifecycleState,
    os_observer_state: LifecycleState,
    semantic_observer_state: LifecycleState,
    session_id: Option<String>,
    detail: String,
    applied: bool,
}

#[derive(Deserialize)]
struct CurrentOsRun {
    updated_at_unix_ms: u64,
    lifecycle_state: String,
    session_id: String,
    run_root: PathBuf,
}

#[derive(Deserialize)]
struct SemanticProcessState {
    project_root: PathBuf,
    identity_path: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct SemanticRunPointer {
    schema_version: String,
    updated_at_unix_ms: u64,
    lifecycle_state: String,
    session_id: String,
    run_root: PathBuf,
}

#[derive(Deserialize)]
struct SemanticStatus {
    running_status: String,
}

struct ObserverProfile {
    project_root: PathBuf,
    identity_path: PathBuf,
}

#[derive(Deserialize)]
struct ServiceConfiguration {
    schema_version: String,
    evidence_root: PathBuf,
    identity_path: PathBuf,
}

pub fn start(configuration: ProductConfiguration, resource_root: PathBuf) -> Result<(), String> {
    write_lifecycle(
        lifecycle_path()?,
        LifecycleSnapshot {
            schema_version: "0.1.0",
            updated_at_unix_ms: unix_time_ms()?,
            state: LifecycleState::Starting,
            os_observer_state: LifecycleState::Starting,
            semantic_observer_state: LifecycleState::Starting,
            session_id: None,
            detail: String::from("正在确认 OS 与语义 Observer"),
            applied: false,
        },
    )?;
    thread::spawn(move || {
        let compatible_resource_root = powershell_compatible_path(&resource_root);
        if let Err(error) = ensure_observers(&configuration, &compatible_resource_root) {
            let lifecycle = match lifecycle_path() {
                Ok(path) => path,
                Err(path_error) => {
                    eprintln!(
                        "无法解析 Observer 生命周期状态路径 error={path_error}; original_error={error}"
                    );
                    return;
                }
            };
            if let Err(write_error) = write_lifecycle(
                lifecycle,
                LifecycleSnapshot {
                    schema_version: "0.1.0",
                    updated_at_unix_ms: unix_time_ms().unwrap_or(0),
                    state: classify_error(&error),
                    os_observer_state: LifecycleState::Degraded,
                    semantic_observer_state: LifecycleState::Stopped,
                    session_id: None,
                    detail: error,
                    applied: false,
                },
            ) {
                eprintln!("无法写入 Observer 生命周期错误状态 error={write_error}");
            }
        } else {
            start_audit_upload_loop(&compatible_resource_root);
        }
    });
    Ok(())
}

fn start_audit_upload_loop(resource_root: &Path) {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return;
    };
    let Some(program_data) = std::env::var_os("PROGRAMDATA").map(PathBuf::from) else {
        return;
    };
    let cloud_audit = local_app_data.join("AgentReins").join("CloudAudit");
    if fs::create_dir_all(&cloud_audit).is_err() {
        eprintln!("无法创建云审计状态目录");
        return;
    }
    let config = cloud_audit.join("device.json");
    let state = cloud_audit.join("state");
    let runtime = local_app_data.join("AgentReins").join("runtime");
    let os_runtime = program_data.join("AgentReins").join("runtime");
    let script = resource_root
        .join("scripts")
        .join("upload-agentreins-audit.ps1");
    if !script.is_file() {
        eprintln!("云审计上传脚本未随应用打包 path={}", script.display());
        return;
    }
    let script = script.to_path_buf();
    thread::spawn(move || loop {
        let status_path = cloud_audit.join("upload-status.json");
        if config.is_file() {
            match power_shell_path() {
                Ok(shell) => {
                    let status = Command::new(shell)
                        .arg("-NoProfile")
                        .arg("-NonInteractive")
                        .arg("-ExecutionPolicy")
                        .arg("Bypass")
                        .arg("-File")
                        .arg(&script)
                        .arg("-EvidenceRoot")
                        .arg(&runtime)
                        .arg("-OsEvidenceRoot")
                        .arg(&os_runtime)
                        .arg("-ConfigurationPath")
                        .arg(&config)
                        .arg("-StateRoot")
                        .arg(&state)
                        .creation_flags(CREATE_NO_WINDOW)
                        .status();
                    let (upload_state, exit_code) = match status {
                        Ok(status) if status.success() => ("completed", status.code()),
                        Ok(status) => ("failed", status.code()),
                        Err(_) => ("launch_failed", None),
                    };
                    // Diagnostic status never contains credentials or task bodies.
                    let _ = write_json_atomic(&status_path, &serde_json::json!({
                        "state": upload_state, "exitCode": exit_code,
                        "updatedAtUnixMs": unix_time_ms().unwrap_or(0)
                    }));
                }
                Err(error) => eprintln!("无法找到 PowerShell，云审计上传暂停 error={error}"),
            }
        } else {
            let _ = write_json_atomic(&status_path, &serde_json::json!({
                "state": "not_enrolled", "updatedAtUnixMs": unix_time_ms().unwrap_or(0)
            }));
        }
        thread::sleep(Duration::from_secs(15));
    });
}

fn ensure_observers(
    configuration: &ProductConfiguration,
    resource_root: &Path,
) -> Result<(), String> {
    let lifecycle = lifecycle_path()?;
    let profile = load_observer_profile(configuration, resource_root)?;
    ensure_service_installed(configuration, resource_root, &profile.identity_path)?;
    validate_service_configuration(configuration, &profile.identity_path)?;
    let minimum_pointer_time = ensure_os_service_started()?;
    let os_run = wait_for_os_run(minimum_pointer_time)?;
    ensure_semantic_observer(resource_root, &os_run, &profile)?;
    write_lifecycle(
        lifecycle,
        LifecycleSnapshot {
            schema_version: "0.1.0",
            updated_at_unix_ms: unix_time_ms()?,
            state: LifecycleState::Running,
            os_observer_state: LifecycleState::Running,
            semantic_observer_state: LifecycleState::Running,
            session_id: Some(os_run.session_id),
            detail: String::from("OS 与语义 Observer 已在同一原生会话中运行"),
            applied: false,
        },
    )
}

fn ensure_service_installed(
    product_configuration: &ProductConfiguration,
    resource_root: &Path,
    identity_path: &Path,
) -> Result<(), String> {
    let configuration_path = service_configuration_path()?;
    if let Some(service) = open_observer_service()?
        && configuration_path.is_file()
        && service_uses_install_root(&service, resource_root)?
    {
        return Ok(());
    }
    let install_script = resource_root
        .join("scripts")
        .join("install-agentreins-observer-service.ps1");
    if !install_script.is_file() {
        return Err(format!(
            "Observer Windows 服务未安装且安装脚本不存在 service={SERVICE_NAME} path={}",
            install_script.display()
        ));
    }
    let output = run_command(
        Command::new(power_shell_path()?)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&install_script)
            .arg("-InstallRoot")
            .arg(resource_root)
            .arg("-EvidenceRoot")
            .arg(&product_configuration.evidence_root)
            .arg("-IdentityPath")
            .arg(identity_path),
    )?;
    if !output.status.success() {
        return Err(format!(
            "Observer Windows 服务自动安装失败 service={SERVICE_NAME} status={} stdout={} stderr={}",
            output.status,
            decode_output(&output.stdout),
            decode_output(&output.stderr)
        ));
    }
    if open_observer_service()?.is_none() {
        return Err(format!(
            "Observer Windows 服务安装命令成功但服务仍不存在 service={SERVICE_NAME}"
        ));
    }
    Ok(())
}

fn service_uses_install_root(service: &Service, resource_root: &Path) -> Result<bool, String> {
    let configuration = service.query_config().map_err(|error| {
        format!("无法读取 Observer Windows 服务配置 service={SERVICE_NAME} error={error}")
    })?;
    let expected_path = resource_root.join("agentreins-observer-service.exe");
    Ok(windows_paths_equal(
        &configuration.executable_path,
        &expected_path,
    ))
}

fn windows_paths_equal(left: &Path, right: &Path) -> bool {
    normalize_windows_path(left).eq_ignore_ascii_case(&normalize_windows_path(right))
}

fn normalize_windows_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim()
        .trim_matches('"')
        .trim_start_matches(r"\\?\")
        .replace('/', r"\")
}

fn validate_service_configuration(
    product_configuration: &ProductConfiguration,
    expected_identity_path: &Path,
) -> Result<(), String> {
    let configuration_path = service_configuration_path()?;
    let configuration =
        read_json::<ServiceConfiguration>(&configuration_path, "Observer 服务配置")?;
    if configuration.schema_version != "0.1.0" {
        return Err(format!(
            "不支持的 Observer 服务配置版本 expected=0.1.0 actual={} path={}",
            configuration.schema_version,
            configuration_path.display()
        ));
    }
    if configuration.evidence_root != product_configuration.evidence_root {
        return Err(format!(
            "Observer 服务证据根目录与产品配置不一致 service={} product={}",
            configuration.evidence_root.display(),
            product_configuration.evidence_root.display()
        ));
    }
    let expected_identity = fs::read(expected_identity_path).map_err(|error| {
        format!(
            "无法读取 WorkBuddy 身份文件 path={} error={error}",
            expected_identity_path.display()
        )
    })?;
    let configured_identity = fs::read(&configuration.identity_path).map_err(|error| {
        format!(
            "无法读取 Observer 服务身份文件 path={} error={error}",
            configuration.identity_path.display()
        )
    })?;
    if expected_identity != configured_identity {
        return Err(format!(
            "Observer 服务身份与当前产品配置身份不匹配 expected={} configured={}",
            expected_identity_path.display(),
            configuration.identity_path.display()
        ));
    }
    Ok(())
}

fn service_configuration_path() -> Result<PathBuf, String> {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .map(|path| {
            path.join("AgentReins")
                .join("config")
                .join("observer-service.json")
        })
        .ok_or_else(|| String::from("Windows PROGRAMDATA 未定义"))
}

fn ensure_os_service_started() -> Result<Option<u64>, String> {
    let service = open_observer_service()?
        .ok_or_else(|| format!("Observer Windows 服务未安装 service={SERVICE_NAME}"))?;
    let status = service.query_status().map_err(|error| {
        format!("无法查询 Observer Windows 服务状态 service={SERVICE_NAME} error={error}")
    })?;
    if status.current_state == ServiceState::Running {
        return Ok(None);
    }
    let started_at_unix_ms = if status.current_state == ServiceState::Stopped {
        let started_at_unix_ms = unix_time_ms()?;
        service.start::<&str>(&[]).map_err(|error| {
            format!("无法启动 Observer Windows 服务 service={SERVICE_NAME} error={error}")
        })?;
        Some(started_at_unix_ms)
    } else {
        None
    };
    for _attempt in 0..300 {
        let status = service.query_status().map_err(|error| {
            format!("无法查询 Observer Windows 服务状态 service={SERVICE_NAME} error={error}")
        })?;
        if status.current_state == ServiceState::Running {
            return Ok(started_at_unix_ms);
        }
        if status.current_state == ServiceState::Stopped {
            return Err(format!(
                "Observer Windows 服务在启动期间停止 service={SERVICE_NAME} exit_code={:?}",
                status.exit_code
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "Observer Windows 服务未在期限内进入 running service={SERVICE_NAME}"
    ))
}

fn open_observer_service() -> Result<Option<Service>, String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| {
            format!("无法连接 Windows 服务管理器 service={SERVICE_NAME} error={error}")
        })?;
    match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_CONFIG | ServiceAccess::QUERY_STATUS | ServiceAccess::START,
    ) {
        Ok(service) => Ok(Some(service)),
        Err(windows_service::Error::Winapi(error))
            if error.raw_os_error() == Some(SERVICE_DOES_NOT_EXIST) =>
        {
            Ok(None)
        }
        Err(error) => Err(format!(
            "无法打开 Observer Windows 服务 service={SERVICE_NAME} error={error}"
        )),
    }
}

fn wait_for_os_run(minimum_pointer_time: Option<u64>) -> Result<CurrentOsRun, String> {
    let program_data = std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .ok_or_else(|| String::from("Windows PROGRAMDATA 未定义"))?;
    let pointer_path = program_data
        .join("AgentReins")
        .join("runtime")
        .join("current-os-run.json");
    for _attempt in 0..300 {
        if pointer_path.is_file() {
            let pointer = read_json::<CurrentOsRun>(&pointer_path, "OS Observer 当前运行指针")?;
            let is_current_run = minimum_pointer_time
                .map(|minimum| pointer.updated_at_unix_ms >= minimum)
                .unwrap_or(true);
            if pointer.lifecycle_state == "running"
                && !pointer.session_id.is_empty()
                && is_current_run
            {
                return Ok(pointer);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "OS Observer 未在期限内进入 running path={}",
        pointer_path.display()
    ))
}

fn load_observer_profile(configuration: &ProductConfiguration, resource_root: &Path) -> Result<ObserverProfile, String> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        .ok_or_else(|| String::from("Windows LOCALAPPDATA 未定义"))?;
    let profile_path = local.join("AgentReins").join("observer-profile.json");
    if profile_path.is_file() {
        return validate_observer_profile(read_json(&profile_path, "WorkBuddy 观测配置")?);
    }
    let mut candidates = Vec::new();
    let entries = fs::read_dir(&configuration.evidence_root).map_err(|error| {
        format!(
            "无法枚举旧语义 Observer 以迁移启动配置 path={} error={error}",
            configuration.evidence_root.display()
        )
    })?;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("无法读取旧 Observer 目录项 error={error}"))?
            .path()
            .join("semantic-observer-process.json");
        if !path.is_file() {
            continue;
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                format!(
                    "无法读取语义 Observer 状态时间 path={} error={error}",
                    path.display()
                )
            })?;
        candidates.push((modified, path));
    }
    candidates.sort_by_key(|candidate| candidate.0);
    if candidates.is_empty() {
        let script = resource_root.join("scripts").join("initialize-workbuddy-observer.ps1");
        if !script.is_file() {
            return Err(format!("首次采集初始化脚本未打包 path={}", script.display()));
        }
        let output = run_command(Command::new(power_shell_path()?)
            .arg("-NoProfile").arg("-NonInteractive").arg("-ExecutionPolicy").arg("Bypass")
            .arg("-File").arg(script))?;
        if !output.status.success() {
            return Err(format!("首次采集初始化失败 stdout={} stderr={}", decode_output(&output.stdout), decode_output(&output.stderr)));
        }
        return validate_observer_profile(read_json(&profile_path, "首次 WorkBuddy 观测配置")?);
    }
    let state_path = candidates
        .pop()
        .map(|candidate| candidate.1)
        .ok_or_else(|| {
            format!(
                "没有可迁移的语义 Observer 启动配置 path={}",
                configuration.evidence_root.display()
            )
        })?;
    let state = read_json::<SemanticProcessState>(&state_path, "语义 Observer 状态")?;
    validate_observer_profile(state)
}

fn validate_observer_profile(state: SemanticProcessState) -> Result<ObserverProfile, String> {
    if !state.project_root.is_dir() {
        return Err(format!(
            "WorkBuddy 项目目录不存在 path={}",
            state.project_root.display()
        ));
    }
    if !state.identity_path.is_file() {
        return Err(format!(
            "WorkBuddy 身份文件不存在 path={}",
            state.identity_path.display()
        ));
    }
    Ok(ObserverProfile {
        project_root: state.project_root,
        identity_path: state.identity_path,
    })
}

fn ensure_semantic_observer(
    resource_root: &Path,
    os_run: &CurrentOsRun,
    profile: &ObserverProfile,
) -> Result<(), String> {
    let runtime_root = local_runtime_root()?;
    fs::create_dir_all(&runtime_root).map_err(|error| {
        format!(
            "无法创建当前用户 Observer 状态目录 path={} error={error}",
            runtime_root.display()
        )
    })?;
    let pointer_path = runtime_root.join("current-semantic-run.json");
    if pointer_path.is_file() {
        let pointer = read_json::<SemanticRunPointer>(&pointer_path, "语义 Observer 当前运行指针")?;
        if semantic_observer_is_running(resource_root, &pointer.run_root)? {
            if pointer.session_id == os_run.session_id {
                return Ok(());
            }
            stop_semantic_observer(resource_root, &pointer.run_root)?;
        }
    }
    start_semantic_observer(resource_root, os_run, profile, &pointer_path)
}

fn semantic_observer_is_running(resource_root: &Path, run_root: &Path) -> Result<bool, String> {
    let status_script = resource_root
        .join("scripts")
        .join("get-workbuddy-semantic-observer-status.ps1");
    if !status_script.is_file() || !run_root.is_dir() {
        return Ok(false);
    }
    let output = run_command(
        Command::new(power_shell_path()?)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&status_script)
            .arg("-RunRoot")
            .arg(run_root),
    )?;
    if !output.status.success() {
        return Ok(false);
    }
    let status = serde_json::from_slice::<SemanticStatus>(&output.stdout).map_err(|error| {
        format!(
            "无法解析语义 Observer 状态 path={} error={error}",
            run_root.display()
        )
    })?;
    Ok(status.running_status == "running")
}

fn stop_semantic_observer(resource_root: &Path, run_root: &Path) -> Result<(), String> {
    let stop_script = resource_root
        .join("scripts")
        .join("stop-workbuddy-semantic-observer.ps1");
    let output = run_command(
        Command::new(power_shell_path()?)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&stop_script)
            .arg("-RunRoot")
            .arg(run_root),
    )?;
    if !output.status.success() {
        return Err(format!(
            "无法停止旧语义 Observer path={} status={} stdout={} stderr={}",
            run_root.display(),
            output.status,
            decode_output(&output.stdout),
            decode_output(&output.stderr)
        ));
    }
    Ok(())
}

fn start_semantic_observer(
    resource_root: &Path,
    os_run: &CurrentOsRun,
    profile: &ObserverProfile,
    pointer_path: &Path,
) -> Result<(), String> {
    let evidence_root = os_run.run_root.parent().ok_or_else(|| {
        format!(
            "OS Observer 运行目录缺少证据根目录 path={}",
            os_run.run_root.display()
        )
    })?;
    let run_root = evidence_root.join(format!("semantic-auto-{}", Uuid::new_v4().simple()));
    let start_script = resource_root
        .join("scripts")
        .join("start-workbuddy-semantic-observer.ps1");
    let collector_path = resource_root.join("workbuddy-semantic-collector.exe");
    let output = run_command(
        Command::new(power_shell_path()?)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&start_script)
            .arg("-SourceRoot")
            .arg(resource_root)
            .arg("-CollectorPath")
            .arg(&collector_path)
            .arg("-ProjectRoot")
            .arg(&profile.project_root)
            .arg("-RunRoot")
            .arg(&run_root)
            .arg("-IdentityPath")
            .arg(&profile.identity_path)
            .arg("-SessionId")
            .arg(&os_run.session_id)
            .arg("-PollIntervalSeconds")
            .arg("5"),
    )?;
    if !output.status.success() {
        return Err(format!(
            "无法启动语义 Observer status={} stdout={} stderr={}",
            output.status,
            decode_output(&output.stdout),
            decode_output(&output.stderr)
        ));
    }
    write_json_atomic(
        pointer_path,
        &SemanticRunPointer {
            schema_version: String::from("0.1.0"),
            updated_at_unix_ms: unix_time_ms()?,
            lifecycle_state: String::from("running"),
            session_id: os_run.session_id.clone(),
            run_root,
        },
    )
}

fn run_command(command: &mut Command) -> Result<Output, String> {
    command
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| {
            format!("无法执行 Observer 生命周期命令 command={command:?} error={error}")
        })
}

fn power_shell_path() -> Result<PathBuf, String> {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|path| {
            path.join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .ok_or_else(|| String::from("Windows SystemRoot 未定义"))
}

fn lifecycle_path() -> Result<PathBuf, String> {
    local_runtime_root().map(|path| path.join("observer-lifecycle.json"))
}

fn local_runtime_root() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("AgentReins").join("runtime"))
        .ok_or_else(|| String::from("Windows LOCALAPPDATA 未定义"))
}

fn classify_error(error: &str) -> LifecycleState {
    if error.contains("未安装") {
        LifecycleState::NotInstalled
    } else if error.contains("Access is denied") || error.contains("拒绝访问") {
        LifecycleState::PermissionRequired
    } else if error.contains("身份") || error.contains("PID 已被") {
        LifecycleState::IdentityMismatch
    } else {
        LifecycleState::Failed
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("无法读取{label} path={} error={error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("无法解析{label} path={} error={error}", path.display()))
}

fn write_lifecycle(path: PathBuf, snapshot: LifecycleSnapshot) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Observer 生命周期路径缺少父目录 path={}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "无法创建 Observer 生命周期目录 path={} error={error}",
            parent.display()
        )
    })?;
    write_json_atomic(&path, &snapshot)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let encoded = serde_json::to_vec_pretty(value).map_err(|error| {
        format!(
            "无法序列化 Observer 生命周期状态 path={} error={error}",
            path.display()
        )
    })?;
    let temporary_path = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    fs::write(&temporary_path, encoded).map_err(|error| {
        format!(
            "无法写入 Observer 生命周期临时状态 path={} error={error}",
            temporary_path.display()
        )
    })?;
    fs::rename(&temporary_path, path).map_err(|error| {
        format!(
            "无法原子发布 Observer 生命周期状态 source={} target={} error={error}",
            temporary_path.display(),
            path.display()
        )
    })
}

fn unix_time_ms() -> Result<u64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("当前时间早于 Unix 元年 error={error}"))?;
    u64::try_from(duration.as_millis())
        .map_err(|error| format!("当前 Unix 毫秒超出 u64 error={error}"))
}

fn decode_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn powershell_compatible_path(path: &Path) -> PathBuf {
    let path_text = path.to_string_lossy();
    if let Some(unc_path) = path_text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc_path}"));
    }
    if let Some(local_path) = path_text.strip_prefix(r"\\?\") {
        return PathBuf::from(local_path);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::{
        LifecycleSnapshot, LifecycleState, powershell_compatible_path, windows_paths_equal,
    };
    use std::path::Path;

    #[test]
    fn powershell_path_removes_windows_verbatim_prefix() {
        let path = Path::new(r"\\?\C:\Program Files\AgentReins");
        assert_eq!(
            powershell_compatible_path(path),
            Path::new(r"C:\Program Files\AgentReins")
        );
    }

    #[test]
    fn service_paths_compare_case_insensitively_without_verbatim_prefix() {
        assert!(windows_paths_equal(
            Path::new(r#""\\?\C:\AgentReins\agentreins-observer-service.exe""#),
            Path::new(r"c:\agentreins\AgentReins-Observer-Service.exe"),
        ));
    }

    #[test]
    fn lifecycle_snapshot_uses_gui_contract_field_names() {
        let snapshot = LifecycleSnapshot {
            schema_version: "0.1.0",
            updated_at_unix_ms: 1,
            state: LifecycleState::Starting,
            os_observer_state: LifecycleState::Starting,
            semantic_observer_state: LifecycleState::Starting,
            session_id: None,
            detail: String::from("starting"),
            applied: false,
        };
        let value = serde_json::to_value(snapshot).expect("生命周期状态应可序列化");

        assert_eq!(value["schemaVersion"], "0.1.0");
        assert_eq!(value["updatedAtUnixMs"], 1);
        assert_eq!(value["osObserverState"], "starting");
        assert_eq!(value["semanticObserverState"], "starting");
        assert!(value.get("schema_version").is_none());
    }
}
