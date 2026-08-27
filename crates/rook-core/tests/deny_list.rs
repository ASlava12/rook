//! The default deny list, in both directions.
//!
//! Nothing overrides a denial, so a rule that fires on a harmless command takes
//! that command away for good — and a deny list that cries wolf is one people
//! turn off. Both halves are therefore asserted: what it must refuse, and what
//! it must not.

use rook_core::Config;
use rook_tools::policy::{Decision, Policy, Risk};

fn refuses(command: &str) -> bool {
    let sandbox = Config::default().sandbox;
    let (policy, errors) = Policy::compile(sandbox.mode, &sandbox.allow, &sandbox.ask, &sandbox.deny);
    assert!(errors.is_empty(), "{errors:?}");
    matches!(policy.decide(&Risk::Execute(command.to_string())), Decision::Deny(_))
}

#[test]
fn the_shapes_that_cannot_be_undone_are_refused() {
    for command in [
        "rm -rf /",
        "rm -rf /*",
        "sudo rm -rf /",
        "mkfs.ext4 /dev/sda1",
        "sudo mkfs /dev/nvme0n1",
        "dd if=/dev/zero of=/dev/sda",
        "cat /dev/urandom > /dev/sda",
        ":(){ :|:& };:",
        "chmod -R 777 /",
        "make build && rm -rf /",
    ] {
        assert!(refuses(command), "{command:?} should be refused");
    }
}

#[test]
fn naming_a_dangerous_command_is_not_running_one() {
    for command in [
        "echo 'never run mkfs on a live disk'",
        "grep -r mkfs docs/",
        "rg 'rm -rf /' --files-with-matches",
        "git commit -m 'guard against rm -rf /'",
        "cat notes.md",
    ] {
        assert!(!refuses(command), "{command:?} only mentions it");
    }
}

#[test]
fn a_bounded_version_of_the_same_command_is_not_refused() {
    for command in [
        "rm -rf /tmp/scratch",
        "rm -rf ./target",
        "chmod -R 777 /tmp/scratch",
        "dd if=disk.img of=copy.img",
        "./scripts/mkfs-helper.sh --dry-run",
    ] {
        assert!(!refuses(command), "{command:?} is bounded and should be allowed through");
    }
}
