//! Own fixture processes before startup parsing, with bounded readiness and useful diagnostics.
//!
//! # Ownership contract
//!
//! A fixture owns the process it starts and every descendant that stays in that process's
//! ownership domain. Teardown and abrupt test exit must stop all of them within a bound:
//!
//! - On macOS and Linux the domain is the leader's process group, plus, for [`Process::spawn`], its
//!   parent-child tree. Drop then freezes the tree, kills discovered descendants (including ones
//!   that started their own session while their parent was still ours) and then the group. SSH
//!   fixtures need that and add independent lifetime guards that stay runnable while the tree is
//!   frozen. HTTP and CONNECT fixtures use [`Process::spawn_grouped`]: their descendants stay in
//!   the group, so Drop kills the group without freezing it. Their owner watch therefore keeps
//!   running through teardown and kills the group after an abrupt owner exit. A frozen fixture
//!   can't rely on the kernel's SIGHUP/SIGCONT for orphaned stopped groups: a same-session
//!   subreaper keeps the group from becoming orphaned, which Linux CI showed.
//! - On Windows the domain is a kill-on-close Job Object created per fixture. The leader starts
//!   suspended and joins the job before it runs, so every descendant inherits membership. Drop
//!   terminates the job, and an abrupt owner exit closes its only handle, which kills the job. The
//!   test process itself joins one more kill-on-close job before its first fixture starts, so a
//!   leader is contained from creation, even if the owner dies before the fixture's own job exists.
//!
//! On macOS and Linux, a descendant that double-forks into a new session and loses its parent
//! before teardown discovers it leaves the domain. No supported fixture does this; a fixture that
//! needs it must first add a reproduction and extend this contract. A process-tree snapshot can't
//! promise to contain arbitrary daemons.
use std::fs::File;
use std::io::Read;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, kill_process, kill_process_group, waitid,
};

pub struct Process {
    child: Child,
    /// Freeze and discover the process tree at teardown, for descendants outside the group.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    discover: bool,
    #[cfg(windows)]
    job: job::Job,
    output: tempfile::NamedTempFile,
    diagnostics: tempfile::NamedTempFile,
    graceful: bool,
}

impl Process {
    /// Starts a fixture whose descendants may leave its process group, such as sshd sessions.
    // Shared with crates that only start HTTP fixtures.
    #[allow(dead_code)]
    pub fn spawn(command: &mut Command, graceful: bool) -> Self {
        Self::start(command, graceful, true)
    }

    /// Starts a fixture whose descendants all stay in its process group. Teardown never freezes
    /// it, so the fixture's own owner watch keeps running until the group is killed.
    // Shared with crates that only start SSH fixtures.
    #[allow(dead_code)]
    pub fn spawn_grouped(command: &mut Command, graceful: bool) -> Self {
        Self::start(command, graceful, false)
    }

    fn start(command: &mut Command, graceful: bool, discover: bool) -> Self {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = discover;
        let output = tempfile::NamedTempFile::new().unwrap();
        let diagnostics = tempfile::NamedTempFile::new().unwrap();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        // Keep the test's session: stopped fixture groups recover on orphan HUP/CONT.
        // Independent lifetime guards are exempt from tree freezes, including under subreapers.
        command.process_group(0);
        command
            .stdin(Stdio::piped())
            .stdout(output.reopen().unwrap())
            .stderr(diagnostics.reopen().unwrap());
        #[cfg(windows)]
        let child = job::spawn_suspended(command);
        #[cfg(not(windows))]
        let child = command.spawn();
        let child = child.unwrap_or_else(|error| panic!("fixture spawn {command:?}: {error}"));
        #[cfg(windows)]
        let (child, job) = job::Job::own(child);
        Self {
            child,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            discover,
            #[cfg(windows)]
            job,
            output,
            diagnostics,
            graceful,
        }
    }

    pub fn ready<T>(&mut self, parse: impl FnOnce(&str) -> Result<T, String>) -> T {
        let result = self
            .line(Duration::from_secs(10))
            .and_then(|line| parse(&line));
        result.unwrap_or_else(|error| panic!("fixture startup: {error}\n{}", self.diagnostics()))
    }

    // Shared with examples that only need startup readiness.
    #[allow(dead_code)]
    pub fn complete(&mut self, timeout: Duration) {
        let end = Instant::now() + timeout;
        while matches!(self.exited(), Ok(false)) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(self.exited(), Ok(true)),
            "fixture completion deadline expired\n{}",
            self.diagnostics()
        );
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let pid = Pid::from_raw(self.child.id() as i32).unwrap();
            let status = waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
            )
            .expect("fixture completion status")
            .unwrap();
            assert_eq!(
                status.exit_status(),
                Some(0),
                "fixture failed\n{}",
                self.diagnostics()
            );
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert!(
            self.child
                .wait()
                .expect("fixture completion status")
                .success(),
            "fixture failed\n{}",
            self.diagnostics()
        );
    }

    fn line(&mut self, timeout: Duration) -> Result<String, String> {
        let end = Instant::now() + timeout;
        loop {
            let bytes = read_prefix(self.output.path(), 8193).map_err(|e| e.to_string())?;
            if let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
                return String::from_utf8(bytes[..end].to_vec()).map_err(|e| e.to_string());
            }
            if bytes.len() > 8192 {
                return Err("readiness line exceeds 8192 bytes".into());
            }
            if self.exited()? {
                return Err("exited before readiness".into());
            }
            if Instant::now() >= end {
                return Err("readiness deadline expired".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn exited(&mut self) -> Result<bool, String> {
        let pid = Pid::from_raw(self.child.id() as i32).unwrap();
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        waitid(WaitId::Pid(pid), options)
            .map(|status| status.is_some())
            .map_err(|e| e.to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn exited(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|e| e.to_string())
    }

    fn diagnostics(&self) -> String {
        let bytes = read_prefix(self.diagnostics.path(), 16384).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn kill_descendants(&self, deadline: Instant) {
        let leader = self.child.id();
        let pid = Pid::from_raw(leader as i32).unwrap();
        // Freeze the parent before taking a process-tree snapshot. sshd sessions and Git
        // services can start their own groups; killing only our original group misses them.
        let _ = kill_process(pid, Signal::STOP);
        let mut owned = vec![leader];
        while Instant::now() < deadline {
            let Some(listing) = process_listing(deadline) else {
                eprintln!("fixture cleanup discovery timed out or failed");
                break;
            };
            let mut added = false;
            let mut running = false;
            for line in listing.lines() {
                // Guards remain runnable to observe original-owner exit even if a same-session
                // subreaper adopts our stopped supervisor. Never stop or kill them here.
                if line.contains("--lifetime-guard") {
                    continue;
                }
                let mut fields = line.split_whitespace();
                let child = fields.next().and_then(|value| value.parse::<u32>().ok());
                let parent = fields.next().and_then(|value| value.parse::<u32>().ok());
                let state = fields.next().unwrap_or("");
                if child.is_some_and(|pid| owned.contains(&pid))
                    && !state.starts_with(['T', 't', 'Z'])
                {
                    running = true;
                }
                if let (Some(child), Some(parent)) = (child, parent)
                    && owned.contains(&parent)
                    && !owned.contains(&child)
                {
                    if let Some(pid) = Pid::from_raw(child as i32) {
                        let _ = kill_process(pid, Signal::STOP);
                    }
                    owned.push(child);
                    added = true;
                }
            }
            if !added && !running {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // Stopped parents cannot reap/reuse their children's PIDs or create more children.
        for child in owned[1..].iter().rev() {
            if let Some(pid) = Pid::from_raw(*child as i32) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
        // Leave the leader stopped until Drop either requests graceful shutdown or kills it.
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_listing(deadline: Instant) -> Option<String> {
    let output = tempfile::NamedTempFile::new().ok()?;
    let mut command = Command::new("ps");
    command.process_group(0);
    let mut child = command
        .args(["-axo", "pid=,ppid=,stat=,command="])
        .stdin(Stdio::null())
        .stdout(output.reopen().ok()?)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return status
                .success()
                .then(|| std::fs::read_to_string(output.path()).ok())
                .flatten();
        }
        if Instant::now() >= deadline {
            // Include a stalled scan shim's children; never block Drop on a pipe or wait().
            if let Some(pid) = Pid::from_raw(child.id() as i32) {
                let _ = kill_process_group(pid, Signal::KILL);
            }
            let _ = child.kill();
            let _ = child.try_wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn read_prefix(path: &std::path::Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl Drop for Process {
    fn drop(&mut self) {
        // Freeze/rescan while parent-child ownership is intact. Guards remain runnable and
        // watch the original test lifetime even if a subreaper adopts a stopped supervisor.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.discover {
            self.kill_descendants(Instant::now() + Duration::from_secs(2));
        }
        self.child.stdin.take();
        if self.graceful {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if let Some(pid) = Pid::from_raw(self.child.id() as i32) {
                let _ = kill_process(pid, Signal::TERM);
                let _ = kill_process(pid, Signal::CONT);
            }
            let end = Instant::now() + Duration::from_secs(2);
            while Instant::now() < end && matches!(self.exited(), Ok(false)) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // A job kill reaches every descendant, including ones the leader no longer tracks.
        #[cfg(windows)]
        self.job.terminate();
        // Kill the group even after a graceful exit: a fixture interrupted during its own cleanup
        // can leave members such as a listener behind. On macOS and Linux the leader is still
        // unreaped here, so its group ID cannot have been reused.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(pid) = Pid::from_raw(self.child.id() as i32) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = self.child.kill();
        let end = Instant::now() + Duration::from_millis(200);
        while matches!(self.child.try_wait(), Ok(None)) {
            if Instant::now() >= end {
                eprintln!("fixture leader did not reap within cleanup deadline");
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;

    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, GetCurrentProcess, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    /// A kill-on-close job held only by the test process, so its exit kills the fixture tree.
    pub struct Job(OwnedHandle);

    /// Starts `command` suspended, already inside the owner's kill-on-close job.
    ///
    /// The test process joins that job before its first fixture starts, so a child is contained
    /// from creation: an owner killed before [`Job::own`] still takes the suspended leader with it.
    /// The job's only handle lives in this static until the process exits.
    pub fn spawn_suspended(command: &mut std::process::Command) -> std::io::Result<Child> {
        static OWNER: std::sync::OnceLock<Result<Job, String>> = std::sync::OnceLock::new();
        let owner = OWNER.get_or_init(|| {
            let job = Job::new().map_err(|error| error.to_string())?;
            // SAFETY: the pseudo-handle for this process is always valid.
            if unsafe { AssignProcessToJobObject(job.handle(), GetCurrentProcess()) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(job)
        });
        if let Err(error) = owner {
            return Err(std::io::Error::other(format!("owner job: {error}")));
        }
        std::os::windows::process::CommandExt::creation_flags(command, CREATE_SUSPENDED);
        command.spawn()
    }

    impl Job {
        /// Assigns a suspended leader to a new job, then lets it run.
        pub fn own(mut child: Child) -> (Child, Self) {
            match Self::assign(&child).and_then(|job| resume(child.id()).map(|()| job)) {
                Ok(job) => (child, job),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("fixture job ownership: {error}");
                }
            }
        }

        fn assign(child: &Child) -> std::io::Result<Self> {
            let job = Self::new()?;
            // SAFETY: both handles are open; the child has not run yet, so it has no descendants.
            if unsafe { AssignProcessToJobObject(job.handle(), child.as_raw_handle()) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }

        fn new() -> std::io::Result<Self> {
            // SAFETY: null attributes and name create an unnamed, non-inheritable job.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `raw` is a new job handle owned by nothing else.
            let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the buffer is a valid extended limit structure of the stated size.
            let set = unsafe {
                SetInformationJobObject(
                    job.handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of_val(&limits) as u32,
                )
            };
            if set == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }

        pub fn terminate(&self) {
            // SAFETY: the job handle stays open for the lifetime of `self`.
            unsafe { TerminateJobObject(self.handle(), 1) };
        }

        fn handle(&self) -> HANDLE {
            self.0.as_raw_handle()
        }
    }

    /// Resumes the threads of a process created suspended; std does not expose its thread handle.
    fn resume(process: u32) -> std::io::Result<()> {
        // SAFETY: a thread snapshot takes no pointers.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is a new snapshot handle owned by nothing else.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut resumed = false;
        // SAFETY: `entry` is a valid THREADENTRY32 with its size set, for every call below.
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == process {
                // SAFETY: opening a thread by ID takes no pointers.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                // SAFETY: `thread` is a new handle owned by nothing else.
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                // SAFETY: the handle has suspend/resume access.
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(std::io::Error::last_os_error());
                }
                resumed = true;
            }
            // SAFETY: as above.
            more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
        }
        if resumed {
            Ok(())
        } else {
            Err(std::io::Error::other("suspended fixture thread not found"))
        }
    }
}

/// Abrupt owner exit: a stand-in owner starts real fixtures, then the test kills it.
#[cfg(test)]
// Benchmarks compile this module without running tests, so every item lives in a test body.
mod owner_tests {
    /// Runs only as the stand-in owner process: prints fixture PIDs, then waits to be killed.
    #[test]
    #[ignore = "helper process for abrupt_owner_exit_stops_every_fixture_process"]
    fn abrupt_owner_helper() {
        use std::path::Path;
        use std::process::Command;
        use std::time::Duration;

        use super::Process;

        fn python() -> Command {
            Command::new(if cfg!(windows) { "python" } else { "python3" })
        }
        fn fixture(name: &str) -> String {
            format!("{}/tests/fixtures/http/{name}", env!("CARGO_MANIFEST_DIR"))
        }

        let Some(root) = std::env::var_os("GIRT_FIXTURE_OWNER_ROOT") else {
            return;
        };
        let root = Path::new(&root);
        // The window between creating a suspended leader and moving it into its own job.
        #[cfg(windows)]
        if std::env::var_os("GIRT_FIXTURE_OWNER_STAGE").is_some_and(|stage| stage == "suspended") {
            let mut leader = python();
            leader.args(["-c", "import time; time.sleep(60)"]);
            #[expect(
                clippy::zombie_processes,
                reason = "the test kills this owner while the leader is still suspended"
            )]
            let leader = super::job::spawn_suspended(&mut leader).unwrap();
            println!("fixture-pids {}", leader.id());
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        let owner = std::process::id().to_string();
        let mut server = python();
        server
            .arg(fixture("server.py"))
            .arg(root)
            .arg("--requests")
            .arg(root.join("server-requests"))
            .args(["--owner", &owner]);
        let mut server = Process::spawn_grouped(&mut server, false);
        server.ready(|line| line.trim().parse::<u16>().map_err(|e| e.to_string()));
        let mut proxy = python();
        proxy
            .arg(fixture("connect_proxy.py"))
            .arg("1")
            .arg(root.join("proxy-requests"))
            .args(["--owner", &owner]);
        let mut proxy = Process::spawn_grouped(&mut proxy, false);
        proxy.ready(|line| line.trim().parse::<u16>().map_err(|e| e.to_string()));
        let pids = Vec::from([server.child.id(), proxy.child.id()]);
        // Only Windows jobs contain arbitrary descendants; POSIX fixtures carry their own watch.
        #[cfg(windows)]
        let (pids, _tree) = {
            let mut pids = pids;
            let mut tree = python();
            tree.args([
                "-c",
                "import subprocess,sys,time\nc=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])\nprint(c.pid,flush=True)\ntime.sleep(60)",
            ]);
            let mut tree = Process::spawn(&mut tree, false);
            let child = tree.ready(|line| line.trim().parse::<u32>().map_err(|e| e.to_string()));
            pids.extend([tree.child.id(), child]);
            (pids, tree)
        };
        let pids = pids.iter().map(u32::to_string).collect::<Vec<_>>();
        println!("fixture-pids {}", pids.join(" "));
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn abrupt_owner_exit_stops_every_fixture_process() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let stages: &[&str] = if cfg!(windows) {
            &["ready", "suspended"]
        } else {
            &["ready"]
        };
        for stage in stages {
            let root = tempfile::tempdir().unwrap();
            let mut owner = Command::new(std::env::current_exe().unwrap())
                .args(["abrupt_owner_helper", "--ignored", "--nocapture"])
                .env("GIRT_FIXTURE_OWNER_ROOT", root.path())
                .env("GIRT_FIXTURE_OWNER_STAGE", stage)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let stdout = owner.stdout.take().unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(pids) = line.strip_prefix("fixture-pids ") {
                        let _ = send.send(pids.to_owned());
                    }
                }
            });
            let Ok(pids) = receive.recv_timeout(Duration::from_secs(30)) else {
                let _ = owner.kill();
                let _ = owner.wait();
                panic!("stand-in owner did not start its fixtures");
            };
            let pids: Vec<u32> = pids.split(' ').map(|pid| pid.parse().unwrap()).collect();
            let watched = pids
                .iter()
                .map(|&pid| Watched::open(pid))
                .collect::<Vec<_>>();
            owner.kill().unwrap();
            owner.wait().unwrap();
            let end = Instant::now() + Duration::from_secs(10);
            for (pid, watched) in pids.iter().zip(&watched) {
                assert!(
                    watched.exits_by(end),
                    "fixture process {pid} survived its owner at stage {stage}"
                );
            }
        }

        /// A fixture process observed by PID; Windows holds a handle so the PID cannot be reused.
        struct Watched {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            pid: rustix::process::Pid,
            #[cfg(windows)]
            handle: std::os::windows::io::OwnedHandle,
        }

        // Benchmarks build this body without the test harness, which leaves the methods unused.
        #[allow(dead_code)]
        impl Watched {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            fn open(pid: u32) -> Self {
                Self {
                    pid: rustix::process::Pid::from_raw(pid as i32).unwrap(),
                }
            }

            #[cfg(any(target_os = "macos", target_os = "linux"))]
            fn exits_by(&self, end: Instant) -> bool {
                loop {
                    if rustix::process::test_kill_process(self.pid).is_err() {
                        return true;
                    }
                    // Reparented fixtures are reaped by another process; a zombie has exited.
                    let output = Command::new("ps")
                        .args(["-o", "stat=", "-p", &self.pid.as_raw_nonzero().to_string()])
                        .output()
                        .unwrap();
                    if String::from_utf8_lossy(&output.stdout)
                        .trim_start()
                        .starts_with('Z')
                    {
                        return true;
                    }
                    if Instant::now() >= end {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }

            #[cfg(windows)]
            fn open(pid: u32) -> Self {
                use std::os::windows::io::FromRawHandle;

                use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
                // SAFETY: opening a process by ID takes no pointers.
                let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                assert!(
                    !raw.is_null(),
                    "open fixture process {pid}: {}",
                    std::io::Error::last_os_error()
                );
                // SAFETY: `raw` is a new process handle owned by nothing else.
                Self {
                    handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) },
                }
            }

            #[cfg(windows)]
            fn exits_by(&self, end: Instant) -> bool {
                use std::os::windows::io::AsRawHandle;

                use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
                use windows_sys::Win32::System::Threading::WaitForSingleObject;
                let remaining = end.saturating_duration_since(Instant::now()).as_millis();
                // SAFETY: the handle has synchronize access and stays open for this call.
                let wait = unsafe {
                    WaitForSingleObject(self.handle.as_raw_handle(), remaining.try_into().unwrap())
                };
                wait == WAIT_OBJECT_0
            }
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {

    #[rstest::rstest]
    #[case::immediate(false, false)]
    #[case::graceful(true, false)]
    #[case::panic(true, true)]
    fn teardown_kills_descendants_in_separate_sessions(
        #[case] graceful: bool,
        #[case] panic: bool,
    ) {
        use super::*;
        let mut command = Command::new("python3");
        command.args(["-c", "import subprocess,time\nchild=subprocess.Popen(['sleep','60'],start_new_session=True)\nprint(child.pid,flush=True)\ntime.sleep(60)"]);
        let mut process = Process::spawn(&mut command, graceful);
        let child = process.ready(|line| line.parse::<i32>().map_err(|e| e.to_string()));
        let child = Pid::from_raw(child).unwrap();
        let result = std::panic::catch_unwind(move || {
            let _process = process;
            assert!(!panic, "fixture owner panicked");
        });
        assert_eq!(result.is_err(), panic);
        let end = Instant::now() + Duration::from_secs(5);
        while rustix::process::test_kill_process(child).is_ok() && Instant::now() < end {
            let output = Command::new("ps")
                .args(["-o", "stat=", "-p", &child.as_raw_nonzero().to_string()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&output.stdout)
                .trim_start()
                .starts_with('Z')
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            rustix::process::test_kill_process(child),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn startup_failure_retains_diagnostics() {
        use super::*;
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo missing-prerequisite >&2; exit 17"]);
        let mut process = Process::spawn(&mut command, false);
        assert!(
            process
                .line(Duration::from_secs(1))
                .unwrap_err()
                .contains("exited before readiness")
        );
        assert!(process.diagnostics().contains("missing-prerequisite"));
    }

    #[test]
    fn stalled_startup_has_a_finite_deadline() {
        use super::*;
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let mut process = Process::spawn(&mut command, false);
        assert_eq!(
            process.line(Duration::from_millis(30)).unwrap_err(),
            "readiness deadline expired"
        );
    }
    #[rstest::rstest]
    #[case::immediate(false)]
    #[case::graceful(true)]
    fn malformed_readiness_reaps_the_owned_process(#[case] graceful: bool) {
        use super::*;
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'invalid\n'; sleep 30"]);
        let mut process = Process::spawn(&mut command, graceful);
        let pid = Pid::from_raw(process.child.id() as i32).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            process.ready::<()>(|_| Err("invalid readiness".into()));
        }));
        assert!(result.is_err());
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        assert_eq!(
            waitid(WaitId::Pid(pid), options).unwrap_err(),
            rustix::io::Errno::CHILD
        );
    }
}
