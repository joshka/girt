//! Own fixture processes before startup parsing, with bounded readiness and useful diagnostics.
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
    output: tempfile::NamedTempFile,
    diagnostics: tempfile::NamedTempFile,
    graceful: bool,
}

impl Process {
    pub fn spawn(command: &mut Command, graceful: bool) -> Self {
        let output = tempfile::NamedTempFile::new().unwrap();
        let diagnostics = tempfile::NamedTempFile::new().unwrap();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        // Keep the test's session: stopped fixture groups recover on orphan HUP/CONT.
        // Independent lifetime guards are exempt from tree freezes, including under subreapers.
        command.process_group(0);
        let child = command
            .stdin(Stdio::piped())
            .stdout(output.reopen().unwrap())
            .stderr(diagnostics.reopen().unwrap())
            .spawn()
            .unwrap_or_else(|error| panic!("fixture spawn {command:?}: {error}"));
        Self {
            child,
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
        self.kill_descendants(Instant::now() + Duration::from_secs(2));
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
