#![cfg_attr(windows, windows_subsystem = "windows")]
use std::{env, io::{Read, Write}, net::TcpStream, time::Duration};

fn main() {
    let exe = env::current_exe().unwrap();
    let name = exe.file_stem().unwrap().to_str().unwrap();
    if !["rust-analyzer", "typescript-language-server", "pyright-langserver", "gopls", "clangd"].contains(&name) {
        std::process::exit(1);
    }
    let eof = std::io::stdin().read(&mut [0]).unwrap() == 0;
    let version = env::args().skip(1).collect::<Vec<_>>() == ["--version"];
    let mut socket = TcpStream::connect(env::var("ROOK_OWNED_PARALLEL_LSP_ADDR").unwrap()).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(65))).unwrap();
    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(socket, "{name} {} {eof} {version}", std::process::id()).unwrap();
    let mut ack = [0];
    socket.read_exact(&mut ack).unwrap();
    assert_eq!(ack, *b"k");
    // Remain serving after --version. The actual caller's probe patience and
    // owned cleanup must retire the connection, before fixture cleanup.
    let _ = socket.read(&mut [0]);
}
