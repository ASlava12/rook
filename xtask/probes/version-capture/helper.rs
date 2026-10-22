#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{
    env,
    io::{Read, Write},
    net::TcpStream,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn notice(phase: &str, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(env::var("ROOK_OWNED_CAPTURE_ADDR").unwrap()).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    writeln!(stream, "{phase} {name} {}", std::process::id()).unwrap();
    let mut acknowledgment = [0; 3];
    stream.read_exact(&mut acknowledgment).unwrap();
    assert_eq!(&acknowledgment, b"ok\n");
    stream
}

fn banner(mut stream: impl Write, bytes: usize) {
    stream.write_all(b"owned-tool 99.0.1\n").unwrap();
    stream.write_all(&vec![b'x'; bytes]).unwrap();
    stream.flush().unwrap();
}

fn main() {
    let executable = env::current_exe().unwrap();
    let name = executable.file_stem().unwrap().to_str().unwrap();
    if env::args().any(|value| value == "--owned-pipe-holder") {
        let connection = notice("holder", name);
        thread::sleep(Duration::from_secs(30));
        drop(connection);
        return;
    }
    drop(notice("start", name));
    match name {
        "rustc" => {
            banner(std::io::stdout(), 0);
            // Deliberately outlives this parent while retaining both pipes.
            // The caller's owned group is responsible for early retirement.
            let _child = Command::new(&executable)
                .arg("--owned-pipe-holder")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            drop(notice("parent-exiting", name));
        }
        "python3" => banner(std::io::stdout(), 60 * 1024),
        "java" => {
            banner(std::io::stderr(), 0);
            drop(notice("finished", name));
            std::process::exit(1);
        }
        "git" => {
            banner(std::io::stdout(), 35 * 1024);
            banner(std::io::stderr(), 35 * 1024);
        }
        "cargo" => banner(std::io::stdout(), 128 * 1024),
        "npm" => banner(std::io::stderr(), 128 * 1024),
        // Shadow all other inventory names too: Windows may find an installed
        // system program even after PATH has been reduced to this directory.
        _ => std::process::exit(1),
    }
    drop(notice("finished", name));
}
