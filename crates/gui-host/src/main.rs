use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use gui_host::product::ProductService;
use gui_host::server::ServerConfiguration;
use product_config::load_encrypted;

struct Arguments {
    bind_address: SocketAddr,
    ui_directory: PathBuf,
    product_config: PathBuf,
    access_token: String,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let arguments = parse_arguments(env::args().collect())?;
    let product_configuration = load_encrypted(&arguments.product_config)?;
    let product_service = ProductService::new(product_configuration)?;
    let configuration = ServerConfiguration {
        bind_address: arguments.bind_address,
        ui_directory: arguments.ui_directory,
        access_token: arguments.access_token,
        product_service,
    };
    gui_host::server::run(configuration).await
}

fn parse_arguments(raw_arguments: Vec<String>) -> Result<Arguments, String> {
    if raw_arguments.len() != 9
        || raw_arguments[1] != "--bind"
        || raw_arguments[3] != "--ui-directory"
        || raw_arguments[5] != "--product-config"
        || raw_arguments[7] != "--access-token"
    {
        return Err(String::from(
            "参数错误，用法: gui-host --bind <127.0.0.1:port> --ui-directory <dist> --product-config <encrypted-config> --access-token <token>",
        ));
    }

    let bind_address = raw_arguments[2]
        .parse::<SocketAddr>()
        .map_err(|error| format!("无效监听地址 address={} error={error}", raw_arguments[2]))?;
    if !bind_address.ip().is_loopback() {
        return Err(format!("GUI 主机只允许监听回环地址 address={bind_address}"));
    }
    if raw_arguments[8].len() < 32 {
        return Err(String::from("GUI 访问令牌长度不足，至少需要 32 个字符"));
    }

    Ok(Arguments {
        bind_address,
        ui_directory: PathBuf::from(&raw_arguments[4]),
        product_config: PathBuf::from(&raw_arguments[6]),
        access_token: raw_arguments[8].clone(),
    })
}
