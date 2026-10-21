//! Darwin keeps a process group addressable while it contains only zombies.
//! Those processes cannot write; waiting for launchd to reap them can otherwise
//! hold command cancellation and inspected recovery indefinitely.

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
    if !exists(group)? {
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
            let child = Command::new("/bin/sh")
                .args(["-c", script])
                .current_dir(workspace)
                .process_group(0)
                .spawn()
                .unwrap();
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
        let _serial = TESTS.lock().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let owned = Owned::start("exit 0", workspace.path());
        owned.wait_for_zombie();
        assert_eq!(exists(owned.child.id() as i32), Some(true), "signal zero still sees the zombie");
        assert_eq!(owned.group.alive(), Some(false), "a zombie cannot perform any more writes");
    }

    #[test]
    fn a_live_descendant_counts_even_when_the_group_leader_is_a_zombie() {
        let _serial = TESTS.lock().unwrap();
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
}
