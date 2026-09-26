//! Nonblocking owned pipes, with one thread responsible for I/O and process lifetime.

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

use super::TransportControl;

pub(crate) struct Server<'a> {
    child: Child,
    diagnostics: RefCell<ChildStderr>,
    control: TransportControl<'a>,
    reaped: bool,
}

impl<'a> Server<'a> {
    pub(crate) fn spawn(command: &mut Command, control: TransportControl<'a>) -> io::Result<Self> {
        control.check()?;
        command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let diagnostics = RefCell::new(child.stderr.take().expect("piped stderr"));
        // Establish the owner before fallible setup so every spawned process has cleanup.
        let server = Self {
            child,
            diagnostics,
            control,
            reaped: false,
        };
        nonblocking(server.child.stdin.as_ref().expect("piped stdin"))?;
        nonblocking(server.child.stdout.as_ref().expect("piped stdout"))?;
        nonblocking(&*server.diagnostics.borrow())?;
        Ok(server)
    }

    pub(crate) fn streams(&mut self) -> (Pipe<'_, ChildStdout>, Pipe<'_, ChildStdin>) {
        let reader = self.child.stdout.take().expect("stdout taken once");
        let writer = self.child.stdin.take().expect("stdin taken once");
        (
            Pipe {
                pipe: reader,
                diagnostics: &self.diagnostics,
                control: self.control,
            },
            Pipe {
                pipe: writer,
                diagnostics: &self.diagnostics,
                control: self.control,
            },
        )
    }

    /// Upload while draining bounded output, so an early rejecting peer cannot fill both pipes.
    /// Return retained bytes even on interruption for protocol-level acknowledgement recovery.
    pub(crate) fn exchange(
        mut reader: Pipe<'_, ChildStdout>,
        writer: Pipe<'_, ChildStdin>,
        request: &[u8],
        pack: &[u8],
        limit: usize,
    ) -> (Vec<u8>, Result<(), crate::packet::Error>, usize) {
        let mut body = Vec::new();
        let mut written = 0;
        let result = exchange(
            &mut reader,
            writer,
            request,
            pack,
            limit,
            &mut body,
            &mut written,
        );
        (body, result, written)
    }

    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            // WNOWAIT keeps the leader's PID reserved until group cleanup; try_wait would reap it.
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
            match waitid(WaitId::Pid(self.pid()), options) {
                Ok(Some(_)) => {
                    self.kill_group();
                    let status = self.child.wait()?;
                    self.reaped = true;
                    return Ok(status);
                }
                Ok(None) => {}
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
            self.control.check()?;
            drain(&self.diagnostics)?;
            // No stdout remains after protocol completion. Sleep is bounded by the same control.
            pause(None, self.control)?;
        }
    }

    fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id() as i32).expect("positive child PID")
    }

    fn kill_group(&self) {
        let _ = kill_process_group(self.pid(), Signal::KILL);
    }
}

impl Drop for Server<'_> {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill_group();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn exchange(
    reader: &mut Pipe<'_, ChildStdout>,
    writer: Pipe<'_, ChildStdin>,
    request: &[u8],
    pack: &[u8],
    limit: usize,
    body: &mut Vec<u8>,
    written: &mut usize,
) -> Result<(), crate::packet::Error> {
    let mut writer = Some(writer);
    let mut sent = 0;
    let mut write_error = None;
    let mut buffer = [0; 8192];
    loop {
        reader.control.check()?;
        drain(reader.diagnostics)?;
        let mut progressed = false;
        // One bounded read and write per pass preserves cancellation and diagnostic fairness.
        let capacity = buffer
            .len()
            .min(limit.saturating_sub(body.len()).saturating_add(1));
        match reader.pipe.read(&mut buffer[..capacity]) {
            Ok(0) => {
                if let Some(error) = write_error {
                    return Err(error);
                }
                if sent < request.len() + pack.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed during upload",
                    )
                    .into());
                }
                return Ok(());
            }
            Ok(count) => {
                let retained = count.min(limit - body.len());
                body.extend_from_slice(&buffer[..retained]);
                if count > retained {
                    return Err(crate::packet::Error::Limit("wire bytes"));
                }
                progressed = true;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        let pending = if sent < request.len() {
            &request[sent..]
        } else {
            &pack[sent - request.len()..]
        };
        if pending.is_empty() {
            writer.take();
        } else if let Some(output) = writer.as_mut() {
            match output.pipe.write(&pending[..pending.len().min(65536)]) {
                Ok(0) => {
                    write_error = Some(
                        io::Error::new(io::ErrorKind::WriteZero, "local upload stalled").into(),
                    );
                    writer.take();
                }
                Ok(count) => {
                    sent += count;
                    *written = sent;
                    progressed = true;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    write_error = Some(error.into());
                    writer.take();
                }
            }
        }
        if !progressed {
            // Poll both directions; neither a full stdin nor an empty stdout should busy-spin.
            let mut fds = vec![PollFd::new(&reader.pipe, PollFlags::IN)];
            if let Some(output) = &writer {
                fds.push(PollFd::new(&output.pipe, PollFlags::OUT));
            }
            let duration = reader
                .control
                .deadline
                .map_or(Duration::from_millis(20), |end| {
                    end.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(20))
                });
            match poll(&mut fds, Some(&Timespec::try_from(duration).unwrap())) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(io::Error::from(error).into()),
            }
        }
    }
}

fn nonblocking(fd: &impl AsFd) -> io::Result<()> {
    fcntl_setfl(fd, fcntl_getfl(fd)? | OFlags::NONBLOCK)?;
    Ok(())
}

pub(crate) struct Pipe<'a, T> {
    pipe: T,
    diagnostics: &'a RefCell<ChildStderr>,
    control: TransportControl<'a>,
}

impl<T: Read + AsFd> Read for Pipe<'_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.control.check()?;
            drain(self.diagnostics)?;
            match self.pipe.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    pause(Some(PollFd::new(&self.pipe, PollFlags::IN)), self.control)?;
                }
                result => return result,
            }
        }
    }
}

impl<T: Write + AsFd> Write for Pipe<'_, T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.control.check()?;
            drain(self.diagnostics)?;
            match self.pipe.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    pause(Some(PollFd::new(&self.pipe, PollFlags::OUT)), self.control)?;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.control.check()
    }
}

// Bound work per pass even if a child produces diagnostics continuously.
fn drain(diagnostics: &RefCell<ChildStderr>) -> io::Result<()> {
    let mut stderr = diagnostics.borrow_mut();
    let mut buffer = [0; 8192];
    for _ in 0..8 {
        match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn pause(mut fd: Option<PollFd<'_>>, control: TransportControl<'_>) -> io::Result<()> {
    control.check()?;
    let duration = control.deadline.map_or(Duration::from_millis(20), |end| {
        end.saturating_duration_since(Instant::now())
            .min(Duration::from_millis(20))
    });
    let timeout = Timespec::try_from(duration).expect("at most 20 ms");
    match poll(fd.as_mut_slice(), Some(&timeout)) {
        Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
