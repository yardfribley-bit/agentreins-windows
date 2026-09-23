#[cfg(windows)]
mod retention;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run(std::env::args().collect()) {
        eprintln!(
            "{{\"level\":\"error\",\"message\":{}}}",
            json_string(&error)
        );
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("\"无法序列化错误信息\""))
}

#[cfg(not(windows))]
fn main() {
    eprintln!("etw-collector 只能在 Windows 上运行");
    std::process::exit(1);
}
