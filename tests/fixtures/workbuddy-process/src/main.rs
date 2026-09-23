use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;
use std::time::Duration;

fn main() {
    exercise_file_operations();
    exercise_loopback_network();

    let child_status = Command::new("cmd.exe")
        .args(["/d", "/c", "exit", "0"])
        .status()
        .unwrap_or_else(|error| panic!("无法启动测试子进程 command=cmd.exe error={error}"));
    assert!(
        child_status.success(),
        "测试子进程返回失败 status={child_status}"
    );
    thread::sleep(Duration::from_secs(2));
}

fn exercise_file_operations() {
    let source_path = std::env::temp_dir().join(format!(
        "agentreins-gate0-{}-source.txt",
        std::process::id()
    ));
    let renamed_path = std::env::temp_dir().join(format!(
        "agentreins-gate0-{}-renamed.txt",
        std::process::id()
    ));
    fs::write(&source_path, b"AgentReins Gate 0 fixture").unwrap_or_else(|error| {
        panic!(
            "无法写入测试文件 path={} error={error}",
            source_path.display()
        )
    });
    let content = fs::read(&source_path).unwrap_or_else(|error| {
        panic!(
            "无法读取测试文件 path={} error={error}",
            source_path.display()
        )
    });
    assert_eq!(content, b"AgentReins Gate 0 fixture");
    thread::sleep(Duration::from_millis(1_200));
    let repeated_content = fs::read(&source_path).unwrap_or_else(|error| {
        panic!(
            "无法重复读取测试文件 path={} error={error}",
            source_path.display()
        )
    });
    assert_eq!(repeated_content, b"AgentReins Gate 0 fixture");
    fs::rename(&source_path, &renamed_path).unwrap_or_else(|error| {
        panic!(
            "无法重命名测试文件 source={} destination={} error={error}",
            source_path.display(),
            renamed_path.display()
        )
    });
    fs::remove_file(&renamed_path).unwrap_or_else(|error| {
        panic!(
            "无法删除测试文件 path={} error={error}",
            renamed_path.display()
        )
    });
}

fn exercise_loopback_network() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("无法绑定测试 TCP 监听器 error={error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("无法读取测试 TCP 地址 error={error}"));
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .unwrap_or_else(|error| panic!("无法接受测试 TCP 连接 error={error}"));
        let mut request = [0u8; 4];
        stream
            .read_exact(&mut request)
            .unwrap_or_else(|error| panic!("无法读取测试 TCP 数据 error={error}"));
        assert_eq!(&request, b"ping");
        stream
            .write_all(b"pong")
            .unwrap_or_else(|error| panic!("无法写入测试 TCP 响应 error={error}"));
    });
    let mut client = TcpStream::connect(address)
        .unwrap_or_else(|error| panic!("无法连接测试 TCP 监听器 address={address} error={error}"));
    client
        .write_all(b"ping")
        .unwrap_or_else(|error| panic!("无法写入测试 TCP 请求 error={error}"));
    let mut response = [0u8; 4];
    client
        .read_exact(&mut response)
        .unwrap_or_else(|error| panic!("无法读取测试 TCP 响应 error={error}"));
    assert_eq!(&response, b"pong");
    server
        .join()
        .unwrap_or_else(|_| panic!("测试 TCP 服务线程异常退出"));
}
