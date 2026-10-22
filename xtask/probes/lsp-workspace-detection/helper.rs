#![cfg_attr(windows, windows_subsystem = "windows")]
use std::io::{Read,Write};
fn main() {
    let exe=std::env::current_exe().unwrap();
    let name=exe.file_stem().unwrap().to_str().unwrap();
    if !["rust-analyzer","typescript-language-server","pyright-langserver","gopls","clangd"].contains(&name) { std::process::exit(1); }
    let args:Vec<_>=std::env::args().skip(1).collect();
    let eof=std::io::stdin().read(&mut [0]).unwrap()==0;
    let version=args==["--version"];
    let log=std::env::var("ROOK_OWNED_LSP_WORKSPACE_LOG").unwrap();
    let mut file=std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
    writeln!(file,"{name} {eof} {version}").unwrap();
    if std::env::var("ROOK_OWNED_LSP_WORKSPACE_REFUSE").as_deref()==Ok(name) { std::process::exit(7); }
    println!("owned language server 99.0.1");
}
