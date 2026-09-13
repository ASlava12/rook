//! Containing a command on Windows.
//!
//! Windows has no fork, so nothing runs between the parent and the command
//! the way Landlock's ruleset does. What it has is integrity levels: a process
//! at low integrity may read what any process may read and write only what is
//! labelled low, and a process may lower its own level and never raise it. So
//! the containment is a launcher — this same binary, started with
//! [`ENV`] set — that lowers itself and then runs the command, which inherits
//! the level and the launcher's pipes. The directories the command may write
//! are labelled low first, by the parent, which is still allowed to.
//!
//! The network is not restrained by this: an integrity level is about objects
//! on the machine. The result of every command says so.

/// The environment variable that makes a rook binary a launcher instead of
/// whatever it was started as. Its value is the command; the others carry
/// where to run it and where its temporary files go.
pub const ENV: &str = "ROOK_CONTAIN";
pub const ENV_CWD: &str = "ROOK_CONTAIN_CWD";
pub const ENV_SCRATCH: &str = "ROOK_CONTAIN_SCRATCH";
/// The binary that answers to [`ENV`], set by that binary about itself at
/// start. A process that never called [`launcher_entry`] — a test binary —
/// must not be started as one: it would run whatever it was instead.
pub const LAUNCHER: &str = "ROOK_LAUNCHER";

/// Everything one command started, held as one thing that can be asked about
/// and ended.
///
/// Unix has a process group: put the command in its own, and one signal reaches
/// whatever it left behind. Windows has nothing of the kind, so `kill_group`
/// there did nothing at all and said so — a command that ran past its timeout
/// was reported as killed-or-not and went on running, and a build left behind
/// by a turn outlived the agent that started it.
///
/// A job object is the nearest thing and is exact: a process is assigned to
/// one, everything it starts after that inherits it, and the job can be asked
/// how many are still in it and told to end them all.
///
/// The assignment happens just after the command starts rather than before,
/// because the command is spawned by the standard library and there is no hook
/// between creation and the first instruction. A grandchild started in that
/// gap is outside the job. The gap is one process creation wide, and closing it
/// means spawning suspended and resuming by hand — which is a rewrite of the
/// spawn path for a case nobody has reported.
#[cfg(windows)]
pub struct Started(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Started {
    /// Put a running process, and everything it starts from now on, in a job of
    /// its own. `None` when the process is already gone or the job cannot be
    /// made, which is not the same as an empty one.
    pub fn holding(pid: u32) -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

        // Safety: documented Win32 calls. Every handle opened here is closed on
        // the path that does not return it, and the job's own handle is closed
        // by `Drop`.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() || job == INVALID_HANDLE_VALUE {
                return None;
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                CloseHandle(job);
                return None;
            }
            let assigned = AssignProcessToJobObject(job, process) != 0;
            CloseHandle(process);
            match assigned {
                true => Some(Self(job)),
                false => {
                    CloseHandle(job);
                    None
                }
            }
        }
    }

    /// Whether anything is still running in it.
    pub fn alive(&self) -> Option<bool> {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };

        let mut counted: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32;
        // Safety: the buffer is the size the call is told it is, and the type is
        // the one the class names.
        let asked = unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicAccountingInformation,
                (&raw mut counted).cast(),
                size,
                std::ptr::null_mut(),
            )
        };
        (asked != 0).then_some(counted.ActiveProcesses > 0)
    }

    /// End everything in it. `true` when the call was made, which is as much as
    /// the unix side claims for a signal.
    pub fn end(&self) -> bool {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // Safety: a job handle this type owns, and an exit code.
        unsafe { TerminateJobObject(self.0, 1) != 0 }
    }
}

#[cfg(windows)]
impl Drop for Started {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        // Safety: a handle this type owns and has not closed. Closing it does
        // not end what is in the job: the limit that would is not set, on
        // purpose — a command left running on purpose outlives the handle.
        unsafe { CloseHandle(self.0) };
    }
}

// Safety: a job handle is not bound to the thread that made it, and every use
// of it here is a call that takes it by value.
#[cfg(windows)]
unsafe impl Send for Started {}
#[cfg(windows)]
unsafe impl Sync for Started {}

/// Run as a launcher if started as one, and never return; otherwise say that
/// this binary can be one, and return at once. The first thing a rook
/// binary's `main` does, before anything that could write or start a thread.
pub fn launcher_entry() {
    #[cfg(windows)]
    if let Ok(command) = std::env::var(ENV) {
        std::process::exit(windows::launch(&command));
    }
    if std::env::var_os(LAUNCHER).is_none()
        && let Ok(me) = std::env::current_exe()
    {
        // Safety: the first line of main, before any other thread exists.
        unsafe { std::env::set_var(LAUNCHER, me) };
    }
}

/// Label `dir`, and everything under it, as low integrity, so that a process
/// at low integrity may write there. A persistent change to the directory's
/// security label, and only that: no permission is added or taken away.
pub fn label_low(dir: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::label_low(dir)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Err("labelling is a Windows thing".into())
    }
}

/// Stop this process's standard handles from reaching the children it starts.
///
/// Windows hands a child every inheritable handle the parent holds, not only
/// the ones it was given as its own stdio — so a daemon started by a command
/// whose output is a pipe keeps that pipe open after the command exits, and
/// whoever is reading it waits for an end that never comes. `rook daemon
/// restart` did exactly that, and the CI job it ran in sat at the six-hour
/// ceiling twelve times.
///
/// Called before starting something that outlives the command. Nothing else
/// needs it: every other child here is given its own pipes or `NUL`, and a
/// handle that is not inheritable is not passed on.
pub fn keep_the_console_to_ourselves() {
    #[cfg(windows)]
    {
        windows::keep_std_handles();
    }
}

#[cfg(windows)]
mod windows {
    use std::path::Path;
    use std::ptr::null_mut;

    /// `SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0)` for the three
    /// standard handles: after this, a child started from here is handed
    /// whatever stdio it is given and none of ours.
    pub fn keep_std_handles() {
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // Safety: `GetStdHandle` returns a handle this process owns or an
            // invalid one, and clearing a flag on either is defined.
            unsafe {
                let handle = GetStdHandle(which);
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, SDDL_REVISION_1,
        SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        GetLengthSid, GetSecurityDescriptorSacl, LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
        TokenIntegrityLevel,
    };

    /// `SE_GROUP_INTEGRITY`, which the bindings leave out.
    const SE_GROUP_INTEGRITY: u32 = 0x20;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The low integrity level, as a SID.
    const LOW: &str = "S-1-16-4096";
    /// A mandatory label ACE: low integrity, inherited by what is created
    /// beneath, forbidding writes from below it — which for a low-labelled
    /// object is nothing.
    const LABEL: &str = "S:(ML;OICI;NW;;;LW)";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error() -> String {
        std::io::Error::last_os_error().to_string()
    }

    /// Lower this process's own integrity level to low. Allowed without any
    /// privilege — a process may always lower itself — and irreversible for
    /// the life of the process, which is the point.
    fn lower_self() -> Result<(), String> {
        // Safety: every call here is a documented Win32 call with the handle
        // and pointers it asks for, each checked before the next uses it.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_DEFAULT | TOKEN_QUERY, &mut token) == 0 {
                return Err(format!("opening the process token: {}", last_error()));
            }
            let mut sid: PSID = null_mut();
            if ConvertStringSidToSidW(wide(LOW).as_ptr(), &mut sid) == 0 {
                CloseHandle(token);
                return Err(format!("the low integrity SID: {}", last_error()));
            }
            let label = TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: SE_GROUP_INTEGRITY },
            };
            let size = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid);
            let set = SetTokenInformation(
                token,
                TokenIntegrityLevel,
                &label as *const TOKEN_MANDATORY_LABEL as *const std::ffi::c_void,
                size,
            );
            let failed = last_error();
            LocalFree(sid as HLOCAL);
            CloseHandle(token);
            match set {
                0 => Err(format!("lowering the integrity level: {failed}")),
                _ => Ok(()),
            }
        }
    }

    pub(crate) fn label_low(dir: &Path) -> Result<(), String> {
        // Safety: as in `lower_self` — documented calls, each result checked.
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(LABEL).as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            ) == 0
            {
                return Err(format!("the low label: {}", last_error()));
            }
            let (mut present, mut defaulted) = (0, 0);
            let mut sacl = null_mut();
            if GetSecurityDescriptorSacl(sd, &mut present, &mut sacl, &mut defaulted) == 0 || present == 0 {
                let why = last_error();
                LocalFree(sd as HLOCAL);
                return Err(format!("reading the label back: {why}"));
            }
            let path = wide(&dir.display().to_string());
            let set = SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                sacl,
            );
            LocalFree(sd as HLOCAL);
            match set {
                0 => Ok(()),
                code => Err(format!(
                    "labelling {} low: {}",
                    dir.display(),
                    std::io::Error::from_raw_os_error(code as i32)
                )),
            }
        }
    }

    /// What the launcher does: lower itself, then run the command through
    /// `cmd /C` in the directory asked for, with its temporary files pointed
    /// at the scratch directory the parent labelled. The command inherits the
    /// level and the launcher's pipes; its exit code is the launcher's.
    pub(crate) fn launch(command: &str) -> i32 {
        if let Err(why) = lower_self() {
            eprintln!("rook: could not contain the command: {why}");
            return 125;
        }
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("cmd");
        cmd.raw_arg(format!("/C {command}"));
        if let Ok(cwd) = std::env::var(super::ENV_CWD) {
            cmd.current_dir(cwd);
        }
        if let Ok(scratch) = std::env::var(super::ENV_SCRATCH) {
            cmd.env("TEMP", &scratch).env("TMP", &scratch);
        }
        cmd.env_remove(super::ENV).env_remove(super::ENV_CWD).env_remove(super::ENV_SCRATCH);
        match cmd.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("rook: could not start the command: {e}");
                126
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::Started;

    /// A job holds a running command, counts what is in it, and ends all of it.
    ///
    /// This is the whole of what Windows had missing. `kill_group` there
    /// returned `false` without trying, so a command that ran past its timeout
    /// was reported as unkillable and went on running — and the check for
    /// whatever it had left behind returned "cannot say", which reads the same
    /// as "nothing".
    ///
    /// Written on a machine that is not Windows, so this is the test that
    /// checks it rather than the author.
    #[test]
    fn a_job_holds_a_running_command_counts_it_and_ends_it() {
        // `ping` to the loopback is the portable way to make a Windows process
        // that lives for a while without a shell builtin.
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .spawn()
            .expect("a command to hold");

        let held = Started::holding(child.id()).expect("a running process can be put in a job");
        assert_eq!(held.alive(), Some(true), "it is in the job and running");

        assert!(held.end(), "the job ends what is in it");
        // Reaped, so the count has somewhere to settle: a terminated process is
        // still in the job until somebody waits for it.
        let _ = child.wait();
        assert_eq!(held.alive(), Some(false), "and nothing is left in it");
    }
}
