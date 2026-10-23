//! Owned compiler-shaped output; no Cargo invocation or external dependency.
use std::io::Write;

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.iter().any(|a| a == "--version") {
        println!("cargo 0.0.0-owned");
        return;
    }
    assert_eq!(arguments.first().map(String::as_str), Some("check"));
    let mut invoked = std::fs::OpenOptions::new().create(true).append(true).open("invoked.txt").unwrap();
    writeln!(invoked, "{}", arguments.join(" ")).unwrap();
    let small = arguments.iter().any(|a| a == "--small");
    let count = if small { 4 } else { 4096 };
    let mut stderr = std::io::stderr().lock();
    for n in 0..count {
        writeln!(stderr, "   Compiling owned_{n} v1.0.0").unwrap();
    }
    writeln!(stderr, "error[E0502]: OWNED_MIDDLE_DIAGNOSTIC").unwrap();
    writeln!(stderr, " --> src/main.rs:7:11\n  |\n7 | exact_source();\n  | ^^^^^^^^^^^^ OWNED_SOURCE_SPAN")
        .unwrap();
    writeln!(stderr, "warning: OWNED_WARNING_REMAINS\n --> src/lib.rs:3:1").unwrap();
    for n in 0..count {
        writeln!(stderr, "test owned_test_{n} ... ok").unwrap();
    }
    writeln!(stderr, "test result: FAILED. 0 passed; 1 failed; 0 ignored").unwrap();
    writeln!(stderr, "error: could not compile owned-fixture due to previous error").unwrap();
    if arguments.iter().any(|a| a == "--wait") {
        write!(stderr, "error: OWNED_PARTIAL_DIAGNOSTIC").unwrap();
        stderr.flush().unwrap();
        drop(stderr);
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }
    stderr.flush().unwrap();
    std::process::exit(101);
}
