use std::{
    env,
    io::{Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};

fn notice(phase: &str, name: &str) {
    let address = env::var("ROOK_OWNED_PROBE_ADDR").expect("owned event receiver");
    let mut stream = TcpStream::connect(address).expect("connect owned event receiver");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("owned read deadline");
    stream.set_write_timeout(Some(Duration::from_secs(5))).expect("owned write deadline");
    writeln!(stream, "{phase} {name} {}", std::process::id()).expect("write owned event");
    let mut acknowledgment = [0; 3];
    stream.read_exact(&mut acknowledgment).expect("read owned acknowledgment");
    assert_eq!(&acknowledgment, b"ok\n", "owned receiver rejected event");
}

fn main() {
    let executable = env::current_exe().expect("owned executable path");
    let name = executable.file_stem().and_then(|name| name.to_str()).expect("owned tool name");
    notice("start", name);
    thread::sleep(Duration::from_secs(2));
    notice("end", name);
    println!("owned-tool 99.0.1");
}
