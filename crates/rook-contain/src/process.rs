//! Darwin signals a snapshot of group members; a concurrent fork can miss it.
//! Keep the owned group through bounded repeated sweeps. Native BSD state also
//! distinguishes a retained zombie from a refused signal/inspection.

fn exists(group: i32) -> Option<bool> {
    // Safety: a positive process-group ID, and signal zero delivers nothing.
    if unsafe { libc::kill(-group, 0) } == 0 {
        Some(true)
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Some(false)
    } else {
        None
    }
}

fn info(pid: i32) -> Result<libc::proc_bsdshortinfo, Option<i32>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
    // Safety: the buffer has the size and alignment required by this flavor.
    // A nonzero argument explicitly includes zombies in Darwin's BSD query.
    let read =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 1, info.as_mut_ptr().cast(), size) };
    if read == size {
        // Safety: the kernel reported writing the complete structure.
        Ok(unsafe { info.assume_init() })
    } else if read == 0 {
        Err(std::io::Error::last_os_error().raw_os_error())
    } else {
        Err(None)
    }
}

pub(super) fn alive(pid: u32) -> Option<bool> {
    let group = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
    if exists(group) == Some(false) {
        return Some(false);
    }
    // A bounded snapshot: a full buffer cannot prove the group is empty.
    let mut pids = [0_i32; 1024];
    // Safety: the array is an aligned, writable PID buffer of the given size.
    let count = unsafe {
        libc::proc_listpgrppids(group, pids.as_mut_ptr().cast(), std::mem::size_of_val(&pids) as i32)
    };
    if count <= 0 {
        return if exists(group) == Some(false) { Some(false) } else { None };
    }
    if count as usize >= pids.len() {
        return None;
    }
    let mut uncertain = false;
    for pid in pids.into_iter().take(count as usize) {
        if pid <= 0 {
            uncertain = true;
            continue;
        }
        match info(pid) {
            Ok(member) if member.pbsi_pgid == group as u32 && member.pbsi_status != libc::SZOMB => {
                return Some(true);
            }
            Ok(_) | Err(Some(libc::ESRCH)) => {}
            Err(_) => uncertain = true,
        }
    }
    if uncertain { None } else { Some(false) }
}

pub(super) fn end(pid: u32) -> bool {
    let Some(group) = i32::try_from(pid).ok().filter(|pid| *pid > 0) else {
        return false;
    };
    // Safety: a positive, owned process-group ID; one signal reaches a kernel
    // snapshot, not necessarily a child joining while that snapshot is used.
    if unsafe { libc::kill(-group, libc::SIGKILL) } != 0 {
        return false;
    }
    // Cancellation can drop the async owner, so finish this bounded cleanup
    // here rather than detaching another future that shutdown can discard.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while alive(pid) == Some(true) && std::time::Instant::now() < until {
        // Safety: the same owned group, still observed with live members.
        unsafe { libc::kill(-group, libc::SIGKILL) };
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // Like other targets, this means a signal was sent. Callers still require
    // alive() == Some(false) before certifying terminal execution effects.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    static TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Owned {
        child: Child,
        group: crate::Group,
    }
    impl Owned {
        fn start(script: &str, workspace: &std::path::Path) -> Self {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]).current_dir(workspace).process_group(0);
            let child = crate::quietly(&mut command).spawn().unwrap();
            let group = crate::Group::holding(Some(child.id()));
            Self { child, group }
        }

        fn wait_for_zombie(&self) {
            let until = Instant::now() + Duration::from_secs(30);
            loop {
                if info(self.child.id() as i32).is_ok_and(|state| state.pbsi_status == libc::SZOMB) {
                    return;
                }
                assert!(Instant::now() < until, "the owned child must reach a retained zombie state");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            self.group.end();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn an_unreaped_zombie_does_not_keep_its_owned_group_running() {
        let _serial = TESTS.lock().unwrap_or_else(|error| error.into_inner());
        let workspace = tempfile::tempdir().unwrap();
        let owned = Owned::start("exit 0", workspace.path());
        owned.wait_for_zombie();
        // Darwin's group signal probe can return EPERM for an all-zombie
        // group. Native state proves termination despite that ambiguity.
        assert_eq!(exists(owned.child.id() as i32), None);
        assert_eq!(owned.group.alive(), Some(false), "a zombie cannot perform any more writes");
    }

    #[test]
    fn a_live_descendant_counts_even_when_the_group_leader_is_a_zombie() {
        let _serial = TESTS.lock().unwrap_or_else(|error| error.into_inner());
        let workspace = tempfile::tempdir().unwrap();
        let owned = Owned::start("sleep 60 & printf '%s' \"$!\" > child-pid; exit 0", workspace.path());
        owned.wait_for_zombie();
        let child: i32 =
            std::fs::read_to_string(workspace.path().join("child-pid")).unwrap().parse().unwrap();
        let member = info(child).expect("the actual descendant must still be alive");
        assert_eq!(member.pbsi_pgid, owned.child.id(), "the descendant belongs to the owned group");
        assert_ne!(member.pbsi_status, libc::SZOMB);
        assert_eq!(owned.group.alive(), Some(true), "a dead leader does not prove an empty group");
        assert!(owned.group.end());
        let until = Instant::now() + Duration::from_secs(30);
        while owned.group.alive() != Some(false) {
            assert!(Instant::now() < until, "all actual group members must become terminal");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn cancellation_at_the_shell_fork_boundary_leaves_no_running_descendants() {
        let _serial = TESTS.lock().unwrap_or_else(|error| error.into_inner());
        for round in 0..32 {
            let workspace = tempfile::tempdir().unwrap();
            let mut owned =
                Owned::start("printf 'ready' > started; sleep 60; printf 'late' > late", workspace.path());
            let until = Instant::now() + Duration::from_secs(30);
            while !workspace.path().join("started").exists() {
                assert!(Instant::now() < until, "round {round}: the actual shell must start");
                std::thread::yield_now();
            }
            // Keep the cue before sleep's fork: moving it after child setup
            // would remove the cancellation window this regression exercises.
            assert!(owned.group.end(), "round {round}: signal the actual command group");
            owned.child.wait().unwrap();
            assert_eq!(owned.group.alive(), Some(false), "round {round}: no straggling descendant");
            assert!(!workspace.path().join("late").exists(), "round {round}: no late effect");
        }
    }
}
