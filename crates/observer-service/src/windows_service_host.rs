use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

const SERVICE_NAME: &str = "AgentReinsObserver";
const HEALTH_INTERVAL_SECONDS: u64 = 5;
const ROLL_SIZE_BYTES: u64 = 67_108_864;
const ROLL_INTERVAL_SECONDS: u64 = 300;

define_windows_service!(ffi_service_main, service_main);

#[derive(Serialize)]
struct ObserverProcessState {
    schema_version: &'static str,
    collector_process_id: u32,
    collector_started_at_unix_ms: u64,
    collector_path: PathBuf,
    run_root: PathBuf,
    identity_path: PathBuf,
    stop_signal_path: PathBuf,
    health_output_path: PathBuf,
    output_directory: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    health_interval_seconds: u64,
    roll_size_bytes: u64,
    roll_interval_seconds: u64,
}

#[derive(Serialize)]
struct CurrentRunPointer {
    schema_version: &'static str,
    updated_at_unix_ms: u64,
    lifecycle_state: &'static str,
    session_id: String,
    run_root: PathBuf,
    process_state_path: PathBuf,
}

#[derive(Deserialize)]
struct ObserverHealthRecord {
    session_id: String,
    coverage_status: String,
}

#[derive(Deserialize)]
struct ServiceConfiguration {
    schema_version: String,
    evidence_root: PathBuf,
    identity_path: PathBuf,
}

pub fn run() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|error| {
        format!("无法连接 Windows 服务控制管理器 service={SERVICE_NAME} error={error}")
    })
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        write_service_error(&error);
    }
}

fn run_service() -> Result<(), String> {
    let stop_requested = Arc::new(AtomicBool::new(false));
    let stop_requested_for_handler = Arc::clone(&stop_requested);
    let status_handle = service_control_handler::register(SERVICE_NAME, move |event| match event {
        ServiceControl::Stop => {
            stop_requested_for_handler.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(|error| {
        format!("无法注册 Windows 服务控制处理器 service={SERVICE_NAME} error={error}")
    })?;

    status_handle
        .set_service_status(service_status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            1,
            Duration::from_secs(20),
        ))
        .map_err(|error| format!("无法报告服务启动状态 service={SERVICE_NAME} error={error}"))?;

    let layout = ServiceLayout::load()?;
    let mut observer = start_observer(&layout)?;
    let session_id = match wait_for_initial_health(&mut observer) {
        Ok(session_id) => session_id,
        Err(error) => {
            if let Err(stop_error) = observer.child.kill() {
                return Err(format!(
                    "{error}; 无法终止启动失败的 ETW Collector process_id={} error={stop_error}",
                    observer.child.id()
                ));
            }
            return Err(error);
        }
    };
    write_current_run_pointer(&layout, &observer, "running", session_id)?;

    status_handle
        .set_service_status(service_status(
            ServiceState::Running,
            ServiceControlAccept::STOP,
            0,
            Duration::default(),
        ))
        .map_err(|error| format!("无法报告服务运行状态 service={SERVICE_NAME} error={error}"))?;

    let observer_result = supervise_observer(&mut observer, &stop_requested);
    status_handle
        .set_service_status(service_status(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
            1,
            Duration::from_secs(20),
        ))
        .map_err(|error| format!("无法报告服务停止中状态 service={SERVICE_NAME} error={error}"))?;
    observer_result?;
    status_handle
        .set_service_status(service_status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            0,
            Duration::default(),
        ))
        .map_err(|error| format!("无法报告服务停止状态 service={SERVICE_NAME} error={error}"))
}

struct ServiceLayout {
    install_root: PathBuf,
    evidence_root: PathBuf,
    identity_path: PathBuf,
    runtime_root: PathBuf,
}

impl ServiceLayout {
    fn load() -> Result<Self, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("无法读取 Observer 服务程序路径 error={error}"))?;
        let install_root = executable
            .parent()
            .ok_or_else(|| {
                format!(
                    "Observer 服务程序路径缺少父目录 path={}",
                    executable.display()
                )
            })?
            .to_path_buf();
        let program_data = std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .ok_or_else(|| String::from("Windows PROGRAMDATA 未定义"))?;
        let product_root = program_data.join("AgentReins");
        let configuration_path = product_root.join("config").join("observer-service.json");
        let runtime_root = product_root.join("runtime");
        let configuration =
            read_json::<ServiceConfiguration>(&configuration_path, "Observer 服务配置")?;
        if configuration.schema_version != "0.1.0" {
            return Err(format!(
                "不支持的 Observer 服务配置版本 expected=0.1.0 actual={} path={}",
                configuration.schema_version,
                configuration_path.display()
            ));
        }
        let evidence_root = configuration.evidence_root;
        let identity_path = configuration.identity_path;
        if !evidence_root.is_absolute() || !identity_path.is_absolute() {
            return Err(format!(
                "Observer 服务配置必须使用绝对路径 evidence_root={} identity_path={}",
                evidence_root.display(),
                identity_path.display()
            ));
        }
        if !identity_path.is_file() {
            return Err(format!(
                "Agent 身份文件不存在 path={}",
                identity_path.display()
            ));
        }
        let collector_path = install_root.join("etw-collector.exe");
        if !collector_path.is_file() {
            return Err(format!(
                "ETW Collector 不存在 path={}",
                collector_path.display()
            ));
        }
        fs::create_dir_all(&evidence_root).map_err(|error| {
            format!(
                "无法创建 Observer 证据目录 path={} error={error}",
                evidence_root.display()
            )
        })?;
        fs::create_dir_all(&runtime_root).map_err(|error| {
            format!(
                "无法创建 Observer 运行状态目录 path={} error={error}",
                runtime_root.display()
            )
        })?;
        Ok(Self {
            install_root,
            evidence_root,
            identity_path,
            runtime_root,
        })
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("无法读取{label} path={} error={error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("无法解析{label} path={} error={error}", path.display()))
}

struct RunningObserver {
    child: Child,
    run_root: PathBuf,
    process_state_path: PathBuf,
    stop_signal_path: PathBuf,
    health_output_path: PathBuf,
}

fn start_observer(layout: &ServiceLayout) -> Result<RunningObserver, String> {
    let run_root = layout
        .evidence_root
        .join(format!("os-auto-{}", Uuid::new_v4().simple()));
    let segments = run_root.join("segments");
    fs::create_dir_all(&segments).map_err(|error| {
        format!(
            "无法创建 OS Observer 运行目录 path={} error={error}",
            run_root.display()
        )
    })?;
    let stop_signal_path = run_root.join("observer-stop.requested");
    let health_output_path = run_root.join("observer-health.ndjson");
    let stdout_path = run_root.join("observer-stdout.json");
    let stderr_path = run_root.join("observer-stderr.ndjson");
    let collector_path = layout.install_root.join("etw-collector.exe");
    let stdout = create_output(&stdout_path)?;
    let stderr = create_output(&stderr_path)?;
    let child = Command::new(&collector_path)
        .arg("--observe-until-stopped")
        .arg("--stop-signal")
        .arg(&stop_signal_path)
        .arg("--health-output")
        .arg(&health_output_path)
        .arg("--health-interval-seconds")
        .arg(HEALTH_INTERVAL_SECONDS.to_string())
        .arg("--output-directory")
        .arg(&segments)
        .arg("--roll-size-bytes")
        .arg(ROLL_SIZE_BYTES.to_string())
        .arg("--roll-interval-seconds")
        .arg(ROLL_INTERVAL_SECONDS.to_string())
        .arg("--identity")
        .arg(&layout.identity_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| {
            format!(
                "无法启动 ETW Collector path={} error={error}",
                collector_path.display()
            )
        })?;
    let process_state_path = run_root.join("observer-process.json");
    write_json_atomic(
        &process_state_path,
        &ObserverProcessState {
            schema_version: "0.2.0",
            collector_process_id: child.id(),
            collector_started_at_unix_ms: unix_time_ms()?,
            collector_path,
            run_root: run_root.clone(),
            identity_path: layout.identity_path.clone(),
            stop_signal_path: stop_signal_path.clone(),
            health_output_path: health_output_path.clone(),
            output_directory: segments,
            stdout_path,
            stderr_path,
            health_interval_seconds: HEALTH_INTERVAL_SECONDS,
            roll_size_bytes: ROLL_SIZE_BYTES,
            roll_interval_seconds: ROLL_INTERVAL_SECONDS,
        },
    )?;
    write_json_atomic(
        &layout.runtime_root.join("current-os-run.json"),
        &CurrentRunPointer {
            schema_version: "0.1.0",
            updated_at_unix_ms: unix_time_ms()?,
            lifecycle_state: "starting",
            session_id: String::new(),
            run_root,
            process_state_path: process_state_path.clone(),
        },
    )?;
    Ok(RunningObserver {
        child,
        run_root: process_state_path
            .parent()
            .ok_or_else(|| String::from("Observer 状态路径缺少父目录"))?
            .to_path_buf(),
        process_state_path,
        stop_signal_path,
        health_output_path,
    })
}

fn wait_for_initial_health(observer: &mut RunningObserver) -> Result<String, String> {
    let deadline = SystemTime::now()
        .checked_add(Duration::from_secs(20))
        .ok_or_else(|| String::from("Observer 健康等待期限溢出"))?;
    loop {
        if let Some(exit) = observer.child.try_wait().map_err(|error| {
            format!(
                "无法读取 ETW Collector 启动状态 process_id={} error={error}",
                observer.child.id()
            )
        })? {
            return Err(format!(
                "ETW Collector 在首个健康记录前退出 process_id={} exit_status={exit}",
                observer.child.id()
            ));
        }
        if observer.health_output_path.is_file() {
            let contents = fs::read_to_string(&observer.health_output_path).map_err(|error| {
                format!(
                    "无法读取 ETW Collector 健康记录 path={} error={error}",
                    observer.health_output_path.display()
                )
            })?;
            if let Some(line) = contents.lines().rev().find(|line| !line.trim().is_empty()) {
                let health =
                    serde_json::from_str::<ObserverHealthRecord>(line).map_err(|error| {
                        format!(
                            "无法解析 ETW Collector 健康记录 path={} error={error}",
                            observer.health_output_path.display()
                        )
                    })?;
                if health.session_id.is_empty() {
                    return Err(format!(
                        "ETW Collector 健康记录缺少 session_id path={}",
                        observer.health_output_path.display()
                    ));
                }
                if health.coverage_status != "healthy" {
                    return Err(format!(
                        "ETW Collector 首个健康记录未达到 healthy status={} path={}",
                        health.coverage_status,
                        observer.health_output_path.display()
                    ));
                }
                return Ok(health.session_id);
            }
        }
        if SystemTime::now() >= deadline {
            return Err(format!(
                "ETW Collector 未在期限内写入首个健康记录 process_id={} path={}",
                observer.child.id(),
                observer.health_output_path.display()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn write_current_run_pointer(
    layout: &ServiceLayout,
    observer: &RunningObserver,
    lifecycle_state: &'static str,
    session_id: String,
) -> Result<(), String> {
    write_json_atomic(
        &layout.runtime_root.join("current-os-run.json"),
        &CurrentRunPointer {
            schema_version: "0.1.0",
            updated_at_unix_ms: unix_time_ms()?,
            lifecycle_state,
            session_id,
            run_root: observer.run_root.clone(),
            process_state_path: observer.process_state_path.clone(),
        },
    )
}

fn supervise_observer(
    observer: &mut RunningObserver,
    stop_requested: &AtomicBool,
) -> Result<(), String> {
    loop {
        if stop_requested.load(Ordering::SeqCst) {
            fs::write(&observer.stop_signal_path, b"stop").map_err(|error| {
                format!(
                    "无法写入 Observer 停止信号 path={} error={error}",
                    observer.stop_signal_path.display()
                )
            })?;
            let exit = observer.child.wait().map_err(|error| {
                format!(
                    "无法等待 ETW Collector 停止 process_id={} error={error}",
                    observer.child.id()
                )
            })?;
            if !exit.success() {
                return Err(format!(
                    "ETW Collector 停止时返回失败 process_id={} exit_status={exit}",
                    observer.child.id()
                ));
            }
            return Ok(());
        }
        if let Some(exit) = observer.child.try_wait().map_err(|error| {
            format!(
                "无法读取 ETW Collector 状态 process_id={} error={error}",
                observer.child.id()
            )
        })? {
            return Err(format!(
                "ETW Collector 意外退出 process_id={} exit_status={exit}",
                observer.child.id()
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn service_status(
    current_state: ServiceState,
    controls_accepted: ServiceControlAccept,
    checkpoint: u32,
    wait_hint: Duration,
) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint,
        wait_hint,
        process_id: None,
    }
}

fn create_output(path: &Path) -> Result<File, String> {
    File::create(path).map_err(|error| {
        format!(
            "无法创建 Observer 输出文件 path={} error={error}",
            path.display()
        )
    })
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let encoded = serde_json::to_vec_pretty(value).map_err(|error| {
        format!(
            "无法序列化 Observer 状态 path={} error={error}",
            path.display()
        )
    })?;
    let temporary_path = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    fs::write(&temporary_path, encoded).map_err(|error| {
        format!(
            "无法写入 Observer 临时状态 path={} error={error}",
            temporary_path.display()
        )
    })?;
    fs::rename(&temporary_path, path).map_err(|error| {
        format!(
            "无法原子发布 Observer 状态 source={} target={} error={error}",
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

fn write_service_error(error: &str) {
    let Some(program_data) = std::env::var_os("PROGRAMDATA").map(PathBuf::from) else {
        eprintln!("{error}");
        return;
    };
    let log_root = program_data.join("AgentReins").join("logs");
    if let Err(create_error) = fs::create_dir_all(&log_root) {
        eprintln!(
            "无法创建服务日志目录 path={} error={create_error}; original_error={error}",
            log_root.display()
        );
        return;
    }
    let record = serde_json::json!({
        "captured_at_unix_ms": unix_time_ms().unwrap_or(0),
        "level": "error",
        "component": "agentreins-observer-service",
        "message": error,
    });
    if let Err(write_error) = fs::write(
        log_root.join("observer-service-last-error.json"),
        record.to_string(),
    ) {
        eprintln!("无法写入服务错误日志 error={write_error}; original_error={error}");
    }
}
