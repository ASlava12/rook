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

#[cfg(windows)]
mod windows {
    use std::path::Path;
    use std::ptr::null_mut;

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
