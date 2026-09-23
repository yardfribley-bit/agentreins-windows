#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod windows_service_host;

#[cfg(windows)]
fn main() {
    if let Err(error) = windows_service_host::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("agentreins-observer-service 只能在 Windows 上运行");
    std::process::exit(1);
}
