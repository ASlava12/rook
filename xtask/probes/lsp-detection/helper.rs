#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{
    io::{Read, Write},
    net::TcpStream,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn connect(phase: &str, mode: &str) -> TcpStream {
    let mut socket = TcpStream::connect(std::env::var("ROOK_OWNED_LSP_ENDPOINT").unwrap()).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut byte = [0];
    let stdin_eof = std::io::stdin().read(&mut byte).unwrap() == 0;
    writeln!(socket, "{phase} {mode} {} {stdin_eof}", std::process::id()).unwrap();
    socket.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"k");
    socket
}

fn main() {
    if std::env::current_exe().unwrap().file_stem().unwrap() != "rust-analyzer" {
        std::process::exit(1);
    }
    let mode = std::env::var("ROOK_OWNED_LSP_MODE").unwrap();
    let marker = std::env::var("ROOK_OWNED_LSP_MARKER").unwrap();
    if std::env::args().any(|arg| arg == "--owned-holder") {
        let mut socket = connect("holder", &mode);
        std::fs::write(marker, b"ready").unwrap();
        let _ = socket.read(&mut [0]);
        return;
    }
    assert!(std::env::args().any(|arg| arg == "--version"));
    let mut socket = connect("start", &mode);
    if mode == "pending" {
        let _ = socket.read(&mut [0]);
        return;
    }
    if mode.starts_with("fork-") {
        // The actual discovery probe owns the intentionally orphaned fixture.
        #[allow(clippy::zombie_processes)]
        let _holder = Command::new(std::env::current_exe().unwrap())
            .arg("--owned-holder")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !std::path::Path::new(&marker).is_file() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if mode.ends_with("refusal") { std::process::exit(7); }
    println!("owned rust-analyzer 99.0.1");
}
