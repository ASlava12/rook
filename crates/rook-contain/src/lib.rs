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

/// `CREATE_NO_WINDOW`: start a child without giving it a console.
///
/// Windows hands every process it starts a console unless told not to, and a
/// console is a window on somebody's desktop. Nothing here wants one — every
/// child is handed pipes — so each is a window that opens and shuts for no
/// reason. Reported from a real desktop: `rook tui` throws up a swarm of them
/// before it draws a frame, because starting up probes sixteen toolchains, and
/// every turn throws up more, because a turn runs a shell, a language server
/// and whatever else it needs.
///
/// The number rather than a `windows-sys` import: this is the whole of what is
/// wanted, and it is what the flag has been since it was documented. Exported
/// as well as applied, because a `tokio::process::Command` takes it by its own
/// method and does not want the standard library's extension trait.
#[cfg(windows)]
pub const NO_WINDOW: u32 = 0x0800_0000;

/// Start this child without a console of its own.
///
/// Does nothing off Windows, where a process has no window to begin with.
pub fn quietly(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(NO_WINDOW);
    }
    command
}

/// What this Windows calls a refusal, in the language it is speaking.
///
/// A contained command that tries to write outside its workspace fails with the
/// operating system's own sentence, and the note that explains it was reached
/// by looking for `access is denied` and a handful of neighbours — English,
/// every one. On a Russian install the twenty-one bytes `cmd` writes to stderr
/// are `Отказано в доступе.`, so the note had never fired for anything, and the
/// model was left with an exit code and a sentence it could do nothing with.
///
/// Asked of the system rather than listed here, because a list of translations
/// is a list to keep in step and there are a hundred of them. `FormatMessage`
/// returns the same text the command printed, because it is where the command
/// got it.
///
/// Empty off Windows, where the refusals are already in the list that is there.
#[cfg(windows)]
pub fn refusals() -> Vec<String> {
    /// The codes a sandbox refusal actually arrives as: denied outright, a
    /// volume that cannot be written, and a file somebody else is holding.
    const CODES: [u32; 3] = [5, 19, 32];
    CODES.iter().filter_map(|code| message_for(*code)).collect()
}

#[cfg(not(windows))]
pub fn refusals() -> Vec<String> {
    Vec::new()
}

/// One system error code as this machine words it.
#[cfg(windows)]
fn message_for(code: u32) -> Option<String> {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
    };

    let mut buffer = [0u16; 512];
    // Safety: the call is told the buffer's length, and writes no more than
    // that many `u16`s into it. `IGNORE_INSERTS` is what makes the message
    // safe to ask for without argument list.
    let written = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            std::ptr::null(),
            code,
            0,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            std::ptr::null(),
        )
    };
    if written == 0 {
        return None;
    }
    let said = String::from_utf16_lossy(&buffer[..written as usize]);
    // `FormatMessage` ends its sentences with a full stop and a line break, and
    // what is wanted is something to look for inside a command's output.
    let said = said.trim().trim_end_matches('.').trim().to_string();
    (!said.is_empty()).then_some(said)
}

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
/// between creation and the first instruction. A grandchild started in that gap
/// is outside the job.
///
/// Measured here rather than left as "rare", because a decision taken on how
/// wide it feels is not a decision. From `spawn` returning to the assignment
/// being done is 23µs on average and 77µs at its worst over two hundred runs;
/// the soonest a `cmd` handed `start /b` got a grandchild running at all was
/// 9.2ms, and over forty runs none was ever outside the job. The window is a
/// hundred and twenty times narrower than the fastest a child can act, because
/// a child has to be loaded and started before it can start anything.
///
/// So it stays open. Closing it means spawning suspended and resuming by hand,
/// which is `CreateProcess` in place of the standard library — losing the pipe
/// plumbing and `kill_on_drop` that come with it — to cover a hundred and
/// twentieth of a millisecond. To re-measure: hold a command in a job and time
/// the call, against how long until the job counts two processes.
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

/// End one process, for the case where it could never be put in a job.
///
/// `Group::end` on Windows had nothing to fall back on: where the job could not
/// be made it answered `false` and killed nothing at all — not even the shell —
/// while unix's `kill(-pid, SIGKILL)` at least always reaches the command
/// itself. A job that cannot be made is rare, and "rare" is what the message
/// "could not be killed" was covering for while the command went on running.
#[cfg(windows)]
pub fn end_process(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    // Safety: documented calls, and the handle opened here is closed on every
    // path that does not return early without one.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return false;
        }
        let ended = TerminateProcess(process, 1) != 0;
        CloseHandle(process);
        ended
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

/// What a command printed, as text.
///
/// Everywhere else a command's bytes are UTF-8 and this is `from_utf8_lossy`.
/// On Windows they are usually not: a console program writing to a pipe emits
/// the machine's OEM code page, which on a Russian install is 866. So `ping`
/// reached the model as eight lines of mojibake, and the model spent a
/// reasoning step calling its own tool output garbled rather than reading it.
/// Every localized message from `git`, `net`, `sc`, `tasklist` and the MSVC
/// linker arrived the same way — which is to say the agent could act on none of
/// them, and neither could the person watching.
///
/// UTF-8 is tried first because much of what a turn runs — cargo, rustc, node —
/// emits it whatever the code page says, and the two agree on ASCII anyway.
/// Everything one command started, whatever the platform calls it.
///
/// A process group on unix, where the command is put in its own and one signal
/// reaches whatever it left behind. A job object on Windows, which is the
/// nearest thing and is exact.
///
/// Here rather than beside the one tool that first needed it, because it is the
/// answer to a platform question and this is where those live — and because a
/// second caller turned up: a check run by `rook eval` kills its shell and
/// leaves `sleep 60` holding the pipe, which is the same bug the exec tool
/// already had and had already fixed.
pub struct Group {
    /// The number to point a question at, and what a person is shown.
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<Started>,
}

impl Group {
    /// Takes hold of a command that has just started.
    pub fn holding(pid: Option<u32>) -> Self {
        #[cfg(windows)]
        return Self { pid, job: pid.and_then(Started::holding) };
        #[cfg(not(windows))]
        Self { pid }
    }

    /// Whoever is asking about this command, by number.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Whether anything is still running in it.
    ///
    /// Signal 0 is the question rather than an answer: it performs the
    /// permission and existence checks and delivers nothing. The command's own
    /// shell is in this group and has been reaped by the time this is asked, so
    /// a member left is one the command started and did not wait for. The job
    /// object answers the same question by counting.
    ///
    /// `None` where there is nothing to ask, which is not the same as an empty
    /// one — `false` there would claim nothing was left behind, and `true` that
    /// every command leaves something.
    pub fn alive(&self) -> Option<bool> {
        #[cfg(unix)]
        // Safety: a signal number of 0 delivers nothing and only asks.
        return self.pid.map(|pid| unsafe { libc::kill(-(pid as i32), 0) == 0 });
        #[cfg(windows)]
        return self.job.as_ref().and_then(Started::alive);
        #[cfg(not(any(unix, windows)))]
        None
    }

    /// Ends all of it. `true` when the call was made.
    pub fn end(&self) -> bool {
        #[cfg(unix)]
        // Safety: a documented call. The negative pid is the group, which is
        // the whole point — killing the shell alone leaves what it started.
        return self.pid.is_some_and(|pid| unsafe { libc::kill(-(pid as i32), libc::SIGKILL) == 0 });
        #[cfg(windows)]
        return match &self.job {
            Some(job) => job.end(),
            // Not even the shell, otherwise: a job that could not be made left
            // this answering `false` and killing nothing, where unix's signal
            // always at least reaches the command itself.
            None => self.pid.is_some_and(end_process),
        };
        #[cfg(not(any(unix, windows)))]
        false
    }
}

/// Put a command in a group of its own, so a deadline can take the whole tree.
///
/// On Windows the job object is taken after the spawn instead, by
/// [`Group::holding`]; what this does there is keep the console to itself,
/// which is the other half of not disturbing whoever is watching.
pub fn on_its_own(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    quietly(command)
}

pub fn printed(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    #[cfg(windows)]
    if !is_utf8(bytes) {
        // Safety: a call that reads a machine setting and takes no arguments.
        let page = unsafe { windows_sys::Win32::Globalization::GetOEMCP() };
        return std::borrow::Cow::Owned(from_code_page(bytes, page));
    }
    String::from_utf8_lossy(bytes)
}

/// Whether these bytes are UTF-8, forgiving a sequence that is merely cut off
/// at the end.
///
/// The distinction is the whole safety of guessing. Output is decoded a read at
/// a time and kept up to a byte cap, so a multi-byte character lands across the
/// boundary routinely — and calling that "not UTF-8" would push a whole chunk
/// of perfectly good UTF-8 through the OEM table and produce the very mojibake
/// this exists to remove. `error_len` tells the two apart: `None` is a sequence
/// that was valid until the bytes ran out, `Some` is a byte that could not
/// appear there at all.
#[cfg(windows)]
fn is_utf8(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// The same bytes read through a named code page.
///
/// The page is `GetOEMCP` and not `GetConsoleOutputCP`, which was the first
/// answer and was wrong twice over. A console output page belongs to a console,
/// and the command whose bytes these are has no console — it was handed pipes,
/// and a program writing to a pipe encodes in the machine's OEM page. Worse, a
/// console page is shared mutable state: every process on the same console can
/// set it, and under `cargo test --workspace` something else on that console
/// did, so this decoded the same bytes correctly alone and into mojibake beside
/// the rest of the suite. `GetOEMCP` is a machine setting that nothing else is
/// racing to change.
///
/// Taken as an argument rather than read here, so the test can name 866 instead
/// of assuming the machine it runs on has it — on a US runner the OEM page is
/// 437 and the same bytes are box drawing.
#[cfg(windows)]
fn from_code_page(bytes: &[u8], page: u32) -> String {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;

    // It reports a zero length as an error rather than as an empty string, and
    // takes the length as an `i32`.
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if len == 0 {
        return String::new();
    }
    // Safety: the documented two-step — ask for the length, then write exactly
    // that many `u16`s into a buffer of exactly that size. Every failure falls
    // back to the lossy read rather than asserting: unreadable text is worth
    // more than no text, and this runs on whatever a command happened to print.
    unsafe {
        let wide_len = MultiByteToWideChar(page, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        if wide_len <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; wide_len as usize];
        let written = MultiByteToWideChar(page, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), wide_len);
        if written <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        wide.truncate(written as usize);
        String::from_utf16_lossy(&wide)
    }
}

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
    use super::{Started, from_code_page, printed};

    /// The bytes a console program actually writes to a pipe on a Russian
    /// Windows. Read as UTF-8 they were eight lines of mojibake in the model's
    /// context, and the model said so instead of reading them.
    ///
    /// 866 is named rather than read from the machine, twice over. A runner
    /// whose OEM page is 437 would decode the same bytes to box drawing and
    /// fail a test about neither. And the page this first asked the machine for
    /// was `GetConsoleOutputCP`, which any process sharing the console can set —
    /// so this passed alone and failed under `cargo test --workspace`, where
    /// something else on that console had changed it.
    #[test]
    fn output_in_the_machines_code_page_arrives_as_text_and_not_as_mojibake() {
        // `Ошибка` in code page 866, which is what `ping` and `net` emit here.
        let said = [0x8Eu8, 0xE8, 0xA8, 0xA1, 0xAA, 0xA0];
        // The precondition and the failure being fixed, in one line: these are
        // the bytes the old read turned into replacement characters. Asserting
        // only that they are not UTF-8 says less and is folded away anyway,
        // because the compiler can see the literal.
        let was = String::from_utf8_lossy(&said);
        assert!(was.contains(char::REPLACEMENT_CHARACTER), "read as UTF-8 this is mojibake: {was:?}");

        let text = from_code_page(&said, 866);
        assert_eq!(text, "Ошибка", "read through the code page rather than replaced: {text:?}");
    }

    /// The risk the other way, and the worse one: output that was UTF-8 all
    /// along, cut by a read boundary or a byte cap in the middle of a character.
    /// Calling that "not UTF-8" would push a whole chunk of good text through
    /// the OEM table — mojibake made by the thing that exists to remove it.
    #[test]
    fn utf8_cut_short_by_a_read_boundary_is_still_read_as_utf8() {
        let whole = "привет".as_bytes();
        // One byte short of the last character, which is where a 16 KiB read
        // lands on a long stream often enough to matter.
        let cut = &whole[..whole.len() - 1];
        assert!(std::str::from_utf8(cut).is_err(), "the precondition: it is cut mid-character");

        let text = printed(cut);
        assert!(text.starts_with("приве"), "the valid part survived as itself: {text:?}");
    }

    /// And the wiring above it: bytes that are not UTF-8 go through the code
    /// page rather than being replaced, and the page comes from the machine.
    ///
    /// Which page is the whole of what this pins. The two calls agree on a
    /// quiet machine and diverge exactly when something has changed the
    /// console — which is when it mattered, and is not a state a test can ask
    /// for. So the claim is made against the source rather than against a
    /// value: whatever `GetOEMCP` says, that is what `printed` used.
    #[test]
    fn printed_reads_what_is_not_utf8_through_the_machines_page() {
        let said = [0x8Eu8, 0xE8, 0xA8, 0xA1, 0xAA, 0xA0];
        // Safety: a call that reads a machine setting and takes no arguments.
        let page = unsafe { windows_sys::Win32::Globalization::GetOEMCP() };

        let text = printed(&said);
        assert_eq!(text, from_code_page(&said, page), "the machine's page, not the console's: {text:?}");
        assert_ne!(text, String::from_utf8_lossy(&said), "and not the lossy read it used to be");
    }

    /// ASCII is the same in both tables, and is most of what a turn runs.
    #[test]
    fn ascii_is_untouched_whichever_table_is_in_force() {
        assert_eq!(
            printed(b"error: could not compile `rook-tools`"),
            "error: could not compile `rook-tools`"
        );
        assert_eq!(printed(b""), "");
    }

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

    /// The fallback, for a command no job would take. It is the difference
    /// between killing the shell and killing nothing at all, which is what a
    /// timeout did on that path.
    #[test]
    fn a_command_no_job_would_take_is_still_ended() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 30 127.0.0.1 > nul"])
            .spawn()
            .expect("a command to end");

        // It has to be running, or "it is gone afterwards" is about nothing.
        assert!(child.try_wait().expect("a child answers").is_none(), "it was over before this began");

        assert!(super::end_process(child.id()), "the call was not made");
        let status = child.wait().expect("a killed command still reports");
        assert!(!status.success(), "it finished on its own, so it was not ended: {status:?}");
    }
}
