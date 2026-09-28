#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod observer_lifecycle;

use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use gui_host::product::ProductService;
use gui_host::server::ServerConfiguration;
use product_config::{ProductConfiguration, load_encrypted, save_encrypted};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use uuid::Uuid;

struct ExitState {
    requested: AtomicBool,
}

struct StartupConfiguration {
    configuration_path: PathBuf,
    product: ProductConfiguration,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        report_startup_error(&error);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let startup = load_startup_configuration(env::args().collect())?;
    let product_service = ProductService::new(startup.product.clone())?;
    let initial_session_id = product_service.current_selection()
        .map(|selection| selection.session_id)
        .unwrap_or_else(|_| String::from("awaiting-observers"));
    let access_token = Uuid::new_v4().simple().to_string();
    let application_context = tauri::generate_context!();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main_window(app);
        }))
        .manage(ExitState {
            requested: AtomicBool::new(false),
        })
        .setup(move |app| {
            let ui_directory = app
                .path()
                .resource_dir()
                .map_err(|error| io_error(format!("无法解析 GUI 资源目录 error={error}")))?
                .join("ui");
            #[cfg(windows)]
            observer_lifecycle::start(
                startup.product.clone(),
                app.path()
                    .resource_dir()
                    .map_err(|error| io_error(format!("无法解析 Observer 资源目录 error={error}")))?,
            )
            .map_err(io_error)?;
            let listener = TcpListener::bind(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
            ))
            .map_err(|error| io_error(format!("无法为原生 GUI 绑定回环端口 error={error}")))?;
            let bind_address = listener
                .local_addr()
                .map_err(|error| io_error(format!("无法读取原生 GUI 回环地址 error={error}")))?;
            let server_configuration = ServerConfiguration {
                bind_address,
                ui_directory,
                access_token: access_token.clone(),
                product_service: product_service.clone(),
            };
            start_query_server(server_configuration, listener);
            let bootstrap_url = format!(
                "http://{bind_address}/api/v1/session/{access_token}"
            )
            .parse()
            .map_err(|error| io_error(format!("无法生成原生 GUI 启动地址 error={error}")))?;
            let window = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(bootstrap_url),
            )
            .title("AgentReins Windows Observer")
            .inner_size(1440.0, 960.0)
            .min_inner_size(1024.0, 720.0)
            .build()
            .map_err(|error| io_error(format!("无法创建 AgentReins 原生窗口 error={error}")))?;
            let window_for_event = window.clone();
            window.on_window_event(move |window_event| {
                if let WindowEvent::CloseRequested { api, .. } = window_event {
                    let exit_state = window_for_event.state::<ExitState>();
                    if !exit_state.requested.load(Ordering::SeqCst) {
                        api.prevent_close();
                        if let Err(error) = window_for_event.hide() {
                            eprintln!("无法隐藏 AgentReins 原生窗口 error={error}");
                        }
                    }
                }
            });
            create_tray(app)?;
            println!(
                "{{\"status\":\"running\",\"session_id\":\"{}\",\"configuration_path\":\"{}\",\"address\":\"{}\"}}",
                initial_session_id,
                startup.configuration_path.display(),
                bind_address
            );
            Ok(())
        })
        .run(application_context)
        .map_err(|error| format!("AgentReins 原生壳运行失败 error={error}"))
}

fn create_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "打开 AgentReins", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit", "退出界面", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &exit])?;
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app_handle, event| match event.id().as_ref() {
            "show" => show_main_window(app_handle),
            "exit" => {
                app_handle
                    .state::<ExitState>()
                    .requested
                    .store(true, Ordering::SeqCst);
                app_handle.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn show_main_window(app_handle: &tauri::AppHandle) {
    let Some(window) = app_handle.get_webview_window("main") else {
        eprintln!("AgentReins 原生主窗口不存在 label=main");
        return;
    };
    if let Err(error) = window.show() {
        eprintln!("无法显示 AgentReins 原生窗口 error={error}");
        return;
    }
    if let Err(error) = window.set_focus() {
        eprintln!("无法聚焦 AgentReins 原生窗口 error={error}");
    }
}

fn start_query_server(configuration: ServerConfiguration, listener: TcpListener) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("AgentReins GUI 查询运行时创建失败 error={error}");
                std::process::exit(1);
            }
        };
        if let Err(error) =
            runtime.block_on(gui_host::server::run_with_listener(configuration, listener))
        {
            eprintln!("AgentReins GUI 查询主机退出 error={error}");
            std::process::exit(1);
        }
    });
}

fn load_startup_configuration(raw_arguments: Vec<String>) -> Result<StartupConfiguration, String> {
    let local_app_data = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| String::from("Windows LOCALAPPDATA 未定义"))?;
    let configuration_path = local_app_data.join("AgentReins").join("product-config.bin");
    if raw_arguments.len() == 1 {
        if !configuration_path.exists() {
            initialize_default_configuration(&configuration_path)?;
        }
        return Ok(StartupConfiguration {
            product: load_encrypted(&configuration_path)?,
            configuration_path,
        });
    }
    if raw_arguments.len() != 10
        || raw_arguments[1] != "--initialize"
        || raw_arguments[2] != "--evidence-root"
        || raw_arguments[4] != "--mcp-manifest"
        || raw_arguments[6] != "--retention-days"
        || raw_arguments[8] != "--diagnostic-export-root"
    {
        return Err(String::from(
            "参数错误，用法: agentreins-desktop --initialize --evidence-root <path> --mcp-manifest <path> --retention-days <days> --diagnostic-export-root <path>",
        ));
    }
    if configuration_path.exists() {
        return Err(format!(
            "产品配置已存在，拒绝覆盖 path={}",
            configuration_path.display()
        ));
    }
    let retention_days = raw_arguments[7]
        .parse::<u16>()
        .map_err(|error| format!("无效证据保留天数 value={} error={error}", raw_arguments[7]))?;
    let product = ProductConfiguration::new(
        PathBuf::from(&raw_arguments[3]),
        PathBuf::from(&raw_arguments[5]),
        PathBuf::from(&raw_arguments[9]),
        retention_days,
    )?;
    save_encrypted(&configuration_path, &product)?;
    Ok(StartupConfiguration {
        configuration_path,
        product,
    })
}

fn io_error(message: String) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::other(message))
}

fn initialize_default_configuration(configuration_path: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    let root = configuration_path.parent().ok_or("产品配置缺少父目录")?;
    let evidence = root.join("evidence");
    let exports = root.join("exports");
    for directory in [&evidence, &exports] {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("无法创建首次启动目录 path={} error={error}", directory.display()))?;
    }
    // Reserved configuration path, not an observation or fabricated evidence.
    // Actual MCP evidence is selected from each observer run's native manifest.
    let manifest = root.join("mcp-unconfigured.json");
    if !manifest.exists() {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&manifest)
            .map_err(|error| format!("无法创建 MCP 配置占位文件 error={error}"))?;
        file.write_all(b"{\"status\":\"unconfigured\"}")
            .map_err(|error| format!("无法写入 MCP 配置占位文件 error={error}"))?;
    }
    let product = ProductConfiguration::new(evidence, manifest, exports, 7)?;
    save_encrypted(configuration_path, &product)
}

fn report_startup_error(error: &str) {
    if let Some(root) = env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(root).join("AgentReins");
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::write(root.join("startup-error.log"), error);
    }
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(window: *mut std::ffi::c_void, text: *const u16, caption: *const u16, flags: u32) -> i32;
        }
        let text: Vec<u16> = format!("AgentReins 启动失败：\n{error}\n\n详情保存在 %LOCALAPPDATA%\\AgentReins\\startup-error.log")
            .encode_utf16().chain(Some(0)).collect();
        let caption: Vec<u16> = "AgentReins".encode_utf16().chain(Some(0)).collect();
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10); }
    }
}
