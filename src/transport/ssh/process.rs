//! A single async owner for nonblocking pipes and the unreaped SSH process group.
use std::future::Future;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use tokio::io::unix::AsyncFd;

use super::{SshError, TransportControl};

pub(crate) struct Session {
    // Declared first so cleanup kills the process before closing registered pipes.
    process: Process,
    input: Option<AsyncFd<ChildStdin>>,
    output: AsyncFd<ChildStdout>,
    diagnostics: AsyncFd<ChildStderr>,
    diagnostics_open: bool,
}
struct Process {
    child: Child,
    reaped: bool,
}
impl Process {
    fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id() as i32).expect("positive child PID")
    }
    fn kill_group(&self) {
        let _ = kill_process_group(self.pid(), Signal::KILL);
    }
    async fn wait(&mut self) -> Result<(), SshError> {
        loop {
            // Never reap before group cleanup: the leader reserves its PID against reuse.
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
            match waitid(WaitId::Pid(self.pid()), options) {
                Ok(Some(_)) => {
                    self.kill_group();
                    let status = self.child.wait()?;
                    self.reaped = true;
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(SshError::Exit(status.code()))
                    };
                }
                Ok(None) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(std::io::Error::from(e).into()),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill_group();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
impl Session {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self, SshError> {
        let child = command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut process = Process {
            child,
            reaped: false,
        };
        let input = register(process.child.stdin.take().expect("piped stdin"))?;
        let output = register(process.child.stdout.take().expect("piped stdout"))?;
        let diagnostics = register(process.child.stderr.take().expect("piped stderr"))?;
        Ok(Self {
            process,
            input: Some(input),
            output,
            diagnostics,
            diagnostics_open: true,
        })
    }

    pub(crate) async fn advertise(
        &mut self,
        limit: usize,
        control: TransportControl<'_>,
    ) -> Result<Vec<u8>, SshError> {
        let result = controlled(
            advertisement(&mut self.output, limit),
            &mut self.diagnostics,
            &mut self.diagnostics_open,
            control,
        )
        .await;
        // EOF before an advertisement often means host-key/authentication failure. Observe the
        // process under the same deadline so it is reported as transport failure, not Git refusal.
        if matches!(result, Err(SshError::Protocol("truncated advertisement"))) {
            self.input.take();
            self.finish(control).await?;
        }
        result
    }

    pub(crate) async fn exchange(
        &mut self,
        request: &[u8],
        pack: &[u8],
        limit: usize,
        control: TransportControl<'_>,
    ) -> (Vec<u8>, Result<(), SshError>, bool) {
        let mut body = Vec::new();
        let mut attempted = false;
        let input = self.input.take().expect("one request per session");
        let exchange = async {
            // Drain response concurrently with upload: a rejecting server can fill stdout while
            // no longer consuming input. Retain received status bytes even when writing fails.
            tokio::try_join!(
                write_request(input, request, pack, &mut attempted),
                read_body(&mut self.output, &mut body, limit)
            )?;
            Ok(())
        };
        let mut result = controlled(
            exchange,
            &mut self.diagnostics,
            &mut self.diagnostics_open,
            control,
        )
        .await;
        if result.is_ok() {
            result = self.finish(control).await;
        }
        (body, result, attempted)
    }

    async fn finish(&mut self, control: TransportControl<'_>) -> Result<(), SshError> {
        controlled(
            self.process.wait(),
            &mut self.diagnostics,
            &mut self.diagnostics_open,
            control,
        )
        .await
    }
}

fn register<T: AsFd + AsRawFd>(pipe: T) -> Result<AsyncFd<T>, SshError> {
    let flags = fcntl_getfl(&pipe).map_err(std::io::Error::from)?;
    fcntl_setfl(&pipe, flags | OFlags::NONBLOCK).map_err(std::io::Error::from)?;
    Ok(AsyncFd::new(pipe)?)
}
async fn read<T: Read + AsRawFd>(
    pipe: &mut AsyncFd<T>,
    bytes: &mut [u8],
) -> Result<usize, SshError> {
    loop {
        let mut ready = pipe.readable_mut().await?;
        match ready.try_io(|fd| fd.get_mut().read(bytes)) {
            Ok(result) => return result.map_err(Into::into),
            Err(_) => continue,
        }
    }
}
async fn exact<T: Read + AsRawFd>(
    pipe: &mut AsyncFd<T>,
    mut bytes: &mut [u8],
) -> Result<(), SshError> {
    while !bytes.is_empty() {
        let count = read(pipe, bytes).await?;
        if count == 0 {
            return Err(SshError::Protocol("truncated advertisement"));
        }
        bytes = &mut bytes[count..];
    }
    Ok(())
}
async fn advertisement(
    output: &mut AsyncFd<ChildStdout>,
    limit: usize,
) -> Result<Vec<u8>, SshError> {
    let mut bytes = Vec::new();
    loop {
        if limit.saturating_sub(bytes.len()) < 4 {
            return Err(SshError::Limit);
        }
        let mut header = [0; 4];
        exact(output, &mut header).await?;
        if !header.iter().all(u8::is_ascii_hexdigit) {
            return Err(SshError::Protocol("pkt-line header"));
        }
        let length = usize::from_str_radix(std::str::from_utf8(&header).unwrap(), 16).unwrap();
        bytes.extend_from_slice(&header);
        if length == 0 {
            return Ok(bytes);
        }
        if !(4..=65520).contains(&length) {
            return Err(SshError::Protocol("pkt-line length"));
        }
        let payload = length - 4;
        if payload > limit.saturating_sub(bytes.len()) {
            return Err(SshError::Limit);
        }
        let offset = bytes.len();
        bytes.resize(offset + payload, 0);
        exact(output, &mut bytes[offset..]).await?;
        tokio::task::yield_now().await;
    }
}
async fn write_request(
    mut input: AsyncFd<ChildStdin>,
    request: &[u8],
    pack: &[u8],
    attempted: &mut bool,
) -> Result<(), SshError> {
    for mut bytes in [request, pack] {
        while !bytes.is_empty() {
            let mut ready = input.writable_mut().await?;
            if let Ok(result) = ready.try_io(|fd| {
                *attempted = true;
                fd.get_mut().write(bytes)
            }) {
                let count = result?;
                if count == 0 {
                    return Err(SshError::Io(std::io::ErrorKind::WriteZero));
                }
                bytes = &bytes[count..];
                tokio::task::yield_now().await;
            }
        }
    }
    // Closing stdin sends EOF after the entire prepared request, including a no-op flush.
    Ok(())
}
async fn read_body(
    output: &mut AsyncFd<ChildStdout>,
    body: &mut Vec<u8>,
    limit: usize,
) -> Result<(), SshError> {
    let mut bytes = [0; 8192];
    loop {
        let count = read(output, &mut bytes).await?;
        if count == 0 {
            return Ok(());
        }
        let keep = count.min(limit.saturating_sub(body.len()));
        body.extend_from_slice(&bytes[..keep]);
        if keep != count {
            return Err(SshError::Limit);
        }
        // AsyncFd readiness can remain immediately ready for a prolific peer. Yield explicitly
        // so the outer cancellation/deadline/diagnostic loop and other runtime tasks get polled.
        tokio::task::yield_now().await;
    }
}
async fn controlled<T>(
    future: impl Future<Output = Result<T, SshError>>,
    diagnostics: &mut AsyncFd<ChildStderr>,
    diagnostics_open: &mut bool,
    control: TransportControl<'_>,
) -> Result<T, SshError> {
    tokio::pin!(future);
    let mut discard = [0; 8192];
    loop {
        control.check()?;
        let interval = control.deadline.map_or(Duration::from_millis(20), |end| {
            end.saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20))
        });
        tokio::select! {
            biased;
            _ = tokio::time::sleep(interval) => {},
            result = &mut future => return result,
            result = read(diagnostics, &mut discard), if *diagnostics_open => {
                if result? == 0 { *diagnostics_open = false; }
                tokio::task::yield_now().await;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    fn assert_reaped(pid: Pid) {
        assert!(matches!(
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG
            ),
            Err(rustix::io::Errno::CHILD)
        ));
    }
    #[test]
    fn dropping_pending_future_kills_and_reaps_the_child() {
        let rt = runtime();
        let cancel = AtomicBool::new(false);
        let pid = rt.block_on(async {
            let mut session =
                Session::spawn(Command::new("/bin/sh").args(["-c", "sleep 10"])).unwrap();
            let pid = session.process.pid();
            let operation =
                async move { session.advertise(100, TransportControl::new(&cancel)).await };
            assert!(
                tokio::time::timeout(Duration::from_millis(50), operation)
                    .await
                    .is_err()
            );
            pid
        });
        assert_reaped(pid);
    }
    #[test]
    fn deadline_cleans_up_child_with_blocked_stdout_and_stderr() {
        let rt = runtime();
        let cancel = AtomicBool::new(false);
        let pid = rt.block_on(async {
            let mut command = Command::new("python3");
            command.args(["-c", "import os,time\nos.write(1,b'0000')\nwhile True: os.write(2,b'x'*8192); os.write(1,b'x'*8192)"]);
            let mut session = Session::spawn(&mut command).unwrap();
            let pid = session.process.pid();
            let control = TransportControl { cancel: &cancel, deadline: Some(Instant::now()+Duration::from_millis(200)) };
            session.advertise(4, control).await.unwrap();
            let (_, result, _) = session.exchange(b"0000", &[], usize::MAX, control).await;
            assert!(matches!(result, Err(SshError::Deadline)));
            pid
        });
        assert_reaped(pid);
    }
    #[test]
    fn successful_exit_kills_pipe_holding_descendants_only() {
        let rt = runtime();
        let cancel = AtomicBool::new(false);
        let mut unrelated = Command::new("sleep").arg("10").spawn().unwrap();
        rt.block_on(async {
            let mut session = Session::spawn(
                Command::new("/bin/sh").args(["-c", "sleep 10 & printf 0000; exit 0"]),
            )
            .unwrap();
            let control = TransportControl {
                cancel: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(2)),
            };
            session.advertise(4, control).await.unwrap();
            session.finish(control).await.unwrap();
            let count =
                tokio::time::timeout(Duration::from_secs(2), read(&mut session.output, &mut [0]))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(count, 0);
        });
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }
    #[test]
    fn already_cancelled_exchange_attempts_no_write() {
        let rt = runtime();
        rt.block_on(async {
            let mut session =
                Session::spawn(Command::new("/bin/sh").args(["-c", "sleep 10"])).unwrap();
            let cancel = AtomicBool::new(true);
            let (_, result, attempted) = session
                .exchange(b"commands", &[], 100, TransportControl::new(&cancel))
                .await;
            assert!(!attempted);
            assert!(matches!(result, Err(SshError::Cancelled)));
        });
    }
}
