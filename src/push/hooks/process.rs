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

pub(super) fn run(
    command: &mut Command,
    input: &[u8],
    control: TransportControl<'_>,
    limit: usize,
) -> io::Result<(bool, Vec<u8>)> {
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
    let mut status = None;
    loop {
        if status.is_none() {
            status = process.observe_exit()?;
        }
        if let Some(status) = status
            && !stdout_open
            && !stderr_open
        {
            stdout.extend_from_slice(&stderr);
            return Ok((status.success(), stdout));
        }
        control.check()?;
        let before = (
            written,
            stdout.len(),
            stderr.len(),
            stdout_open,
            stderr_open,
        );
        if written == input.len() || status.is_some() {
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
        if stdout_open {
            stdout_open = read_output(
                process.child.stdout.as_mut().expect("piped stdout"),
                &mut stdout,
                remaining,
            )?;
        }
        let remaining = limit
            .saturating_sub(stdout.len())
            .saturating_sub(stderr.len());
        if stderr_open {
            stderr_open = read_output(
                process.child.stderr.as_mut().expect("piped stderr"),
                &mut stderr,
                remaining,
            )?;
        }
        // One bounded pass per pipe keeps cancellation fair even with continuous output.
        let pause = control.deadline.map_or(Duration::from_millis(10), |end| {
            end.saturating_duration_since(std::time::Instant::now())
                .min(Duration::from_millis(10))
        });
        if before
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

    fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        match waitid(WaitId::Pid(self.pid()), options) {
            Ok(Some(_)) => {
                // Reserve the leader PID until its group is killed; descendants may hold pipes.
                let _ = kill_process_group(self.pid(), Signal::KILL);
                let status = self.child.wait()?;
                self.reaped = true;
                Ok(Some(status))
            }
            Ok(None) | Err(rustix::io::Errno::INTR) => Ok(None),
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

fn read_output(reader: &mut impl Read, output: &mut Vec<u8>, remaining: usize) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    let capacity = buffer.len().min(remaining.saturating_add(1));
    match reader.read(&mut buffer[..capacity]) {
        Ok(0) => Ok(false),
        Ok(count) if count <= remaining => {
            output.extend_from_slice(&buffer[..count]);
            Ok(true)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "receive hook output limit",
        )),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(true)
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
        assert!(output.0);
        assert_eq!(output.1, vec![0; 262144]);
    }

    #[test]
    fn leader_exit_closes_pipes_held_by_owned_descendants() {
        let mut command = Command::new("/bin/sh");
        // Finite even if group cleanup regresses; an inherited pipe is still held when the leader
        // exits.
        command.args(["-c", "sleep 3 & exit 0"]);
        let cancel = AtomicBool::new(false);
        let control = TransportControl {
            cancel: &cancel,
            deadline: Some(std::time::Instant::now() + Duration::from_secs(1)),
        };
        let output = run(&mut command, b"", control, 1024).unwrap();
        assert!(output.0);
        assert!(output.1.is_empty());
    }
}
