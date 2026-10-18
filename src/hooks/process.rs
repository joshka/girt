//! One owner for hook pipes and the reserved process-group leader PID.
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

use crate::transport::TransportControl;

struct Process {
    child: Child,
    reaped: bool,
}

/// Runs `command` in its own process group, writing `input` and capturing stdout then stderr up
/// to `limit` bytes, until the leader exits and every pipe closes.
pub(crate) fn run(
    command: &mut Command,
    input: &[u8],
    control: TransportControl<'_>,
    limit: usize,
) -> io::Result<(ExitStatus, Vec<u8>)> {
    control.check()?;
    command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = Process {
        child: command.spawn()?,
        reaped: false,
    };
    // Establish the owner before fallible setup, so errors always kill and reap the child.
    nonblocking(process.child.stdin.as_ref().expect("piped stdin"))?;
    nonblocking(process.child.stdout.as_ref().expect("piped stdout"))?;
    nonblocking(process.child.stderr.as_ref().expect("piped stderr"))?;
    let mut written = 0;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut exited = false;
    loop {
        if !exited {
            exited = process.observe_exit()?;
        }
        if exited && !stdout_open && !stderr_open {
            let status = process.child.wait()?;
            process.reaped = true;
            stdout.extend_from_slice(&stderr);
            return Ok((status, stdout));
        }
        control.check()?;
        let before = (
            written,
            stdout.len(),
            stderr.len(),
            stdout_open,
            stderr_open,
        );
        if written == input.len() || exited {
            process.child.stdin.take();
        }
        if let Some(stdin) = &mut process.child.stdin {
            let end = input.len().min(written.saturating_add(8192));
            match stdin.write(&input[written..end]) {
                Ok(0) => {
                    process.child.stdin.take();
                }
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                    process.child.stdin.take();
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        let remaining = limit
            .saturating_sub(stdout.len())
            .saturating_sub(stderr.len());
        let mut read_progress = false;
        if stdout_open {
            let (open, progress) = read_output(
                process.child.stdout.as_mut().expect("piped stdout"),
                &mut stdout,
                remaining,
            )?;
            stdout_open = open;
            read_progress |= progress;
        }
        let remaining = limit
            .saturating_sub(stdout.len())
            .saturating_sub(stderr.len());
        if stderr_open {
            let (open, progress) = read_output(
                process.child.stderr.as_mut().expect("piped stderr"),
                &mut stderr,
                remaining,
            )?;
            stderr_open = open;
            read_progress |= progress;
        }
        // One bounded pass per pipe keeps cancellation fair even with continuous output.
        let pause = control.deadline.map_or(Duration::from_millis(10), |end| {
            end.saturating_duration_since(std::time::Instant::now())
                .min(Duration::from_millis(10))
        });
        if !read_progress
            && before
                == (
                    written,
                    stdout.len(),
                    stderr.len(),
                    stdout_open,
                    stderr_open,
                )
        {
            std::thread::sleep(pause);
        }
    }
}

impl Process {
    fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id() as i32).expect("positive child PID")
    }

    fn observe_exit(&mut self) -> io::Result<bool> {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        // Keep the leader PID reserved while descendants hold pipes. Error cleanup can then
        // safely address its group; successful completion reaps without killing background work.
        match waitid(WaitId::Pid(self.pid()), options) {
            Ok(Some(_)) => Ok(true),
            Ok(None) | Err(rustix::io::Errno::INTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = kill_process_group(self.pid(), Signal::KILL);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn nonblocking(pipe: &impl AsFd) -> io::Result<()> {
    let flags = fcntl_getfl(pipe)?;
    fcntl_setfl(pipe, flags | OFlags::NONBLOCK)?;
    Ok(())
}

fn read_output(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
    remaining: usize,
) -> io::Result<(bool, bool)> {
    let mut buffer = [0; 8192];
    match reader.read(&mut buffer) {
        Ok(0) => Ok((false, false)),
        Ok(count) => {
            output.extend_from_slice(&buffer[..count.min(remaining)]);
            Ok((true, true))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok((true, false))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn drains_output_while_writing_more_than_a_pipe_buffer() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "head -c 131072 /dev/zero; cat >/dev/null; head -c 131072 /dev/zero >&2",
        ]);
        let cancel = AtomicBool::new(false);
        let control = TransportControl {
            cancel: &cancel,
            deadline: Some(std::time::Instant::now() + Duration::from_secs(10)),
        };
        let output = run(&mut command, &vec![b'x'; 131072], control, 262144).unwrap();
        assert!(output.0.success());
        assert_eq!(output.1, vec![0; 262144]);
    }

    #[test]
    fn leader_exit_drains_descendant_output_without_killing_it() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "(sleep 0.1; printf descendant) & exit 0"]);
        let cancel = AtomicBool::new(false);
        let control = TransportControl {
            cancel: &cancel,
            deadline: Some(std::time::Instant::now() + Duration::from_secs(3)),
        };
        let output = run(&mut command, b"", control, 1024).unwrap();
        assert!(output.0.success());
        assert_eq!(output.1, b"descendant");
    }
    #[test]
    fn deadline_still_interrupts_descendant_pipes_after_leader_exit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 3 & exit 0"]);
        let cancel = AtomicBool::new(false);
        let control = TransportControl {
            cancel: &cancel,
            deadline: Some(std::time::Instant::now() + Duration::from_millis(100)),
        };
        let error = run(&mut command, b"", control, 1024).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
