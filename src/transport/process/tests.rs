//! Original finite shell fixtures; every stall exits independently even if interruption regresses.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rstest::rstest;
use rustix::process::{WaitOptions, waitpid};

use super::{Server, TransportControl};
use crate::transport::{Interruption, interruption};

fn command(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    // The watchdog is inside the owned group and cannot outlive normal cleanup.
    let script = format!("(sleep 8; kill -KILL -$$) </dev/null >/dev/null 2>&1 &\n{script}");
    command.arg("-c").arg(script);
    command
}
fn control(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_millis(250)),
    }
}
fn is_deadline(error: std::io::Error) {
    assert!(matches!(interruption(&error), Some(Interruption::Deadline)));
}

#[test]
fn deadline_interrupts_silent_advertisement() {
    let cancel = AtomicBool::new(false);
    let mut server = Server::spawn(&mut command("sleep 3"), control(&cancel)).unwrap();
    let (mut reader, writer) = server.streams();
    is_deadline(reader.read(&mut [0]).unwrap_err());
    drop((reader, writer));
    let pid = server.pid();
    drop(server);
    assert!(matches!(
        waitpid(Some(pid), WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
}

#[test]
fn deadline_interrupts_full_input_pipe() {
    let cancel = AtomicBool::new(false);
    let mut server = Server::spawn(&mut command("sleep 3"), control(&cancel)).unwrap();
    let (reader, mut writer) = server.streams();
    is_deadline(writer.write_all(&vec![0; 8 * 1024 * 1024]).unwrap_err());
    drop((reader, writer));
}

#[test]
fn deadline_interrupts_exit_wait_after_eof() {
    let cancel = AtomicBool::new(false);
    let mut server = Server::spawn(&mut command("exec 1>&-; sleep 3"), control(&cancel)).unwrap();
    let (mut reader, writer) = server.streams();
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    drop((reader, writer));
    is_deadline(server.wait().unwrap_err());
}

fn blocked_read(reader: &mut dyn Read, _: &mut dyn Write) -> std::io::Error {
    reader.read(&mut [0]).unwrap_err()
}
fn blocked_write(_: &mut dyn Read, writer: &mut dyn Write) -> std::io::Error {
    // Do not let write_all retry Interrupted: this test must terminate on any pipe error.
    let bytes = [0; 65536];
    loop {
        match writer.write(&bytes) {
            Ok(0) => panic!("unexpected zero write"),
            Ok(_) => {}
            Err(error) => return error,
        }
    }
}

#[rstest]
#[case::read(blocked_read)]
#[case::write(blocked_write)]
fn flag_interrupts_waiting_pipe(
    #[case] operation: fn(&mut dyn Read, &mut dyn Write) -> std::io::Error,
) {
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(2)),
    };
    let mut server = Server::spawn(&mut command("printf R; sleep 3"), control).unwrap();
    let (mut reader, mut writer) = server.streams();
    let mut ready = [0];
    reader.read_exact(&mut ready).unwrap();
    assert_eq!(ready, [b'R']);
    // The server has reached its finite stall. The flag is changed while client I/O is pending.
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
        });
        operation(&mut reader, &mut writer)
    });
    assert!(matches!(
        interruption(&error),
        Some(Interruption::Cancelled)
    ));
}

#[test]
fn drains_full_diagnostic_pipe_before_reading_response() {
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    };
    let mut server = Server::spawn(
        &mut command("dd if=/dev/zero bs=65536 count=32 >&2 2>/dev/null; printf R"),
        control,
    )
    .unwrap();
    let (mut reader, writer) = server.streams();
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    assert_eq!(output, b"R");
    drop((reader, writer));
    assert!(server.wait().unwrap().success());
}

#[test]
fn unread_full_stdout_does_not_prevent_exit_deadline_or_cleanup() {
    let cancel = AtomicBool::new(false);
    let mut server = Server::spawn(
        &mut command("dd if=/dev/zero bs=65536 count=128 2>/dev/null; sleep 3"),
        control(&cancel),
    )
    .unwrap();
    // Retain stdout without consuming it, forcing a full pipe during the exit wait.
    is_deadline(server.wait().unwrap_err());
}

#[test]
fn observable_exit_wins_over_later_cancellation() {
    let cancel = AtomicBool::new(false);
    let mut server = Server::spawn(&mut command("exit 0"), TransportControl::new(&cancel)).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while rustix::process::waitid(
        rustix::process::WaitId::Pid(server.pid()),
        rustix::process::WaitIdOptions::EXITED
            | rustix::process::WaitIdOptions::NOHANG
            | rustix::process::WaitIdOptions::NOWAIT,
    )
    .unwrap()
    .is_none()
    {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    cancel.store(true, Ordering::Relaxed);
    assert!(server.wait().unwrap().success());
}

#[test]
fn group_cleanup_closes_descendant_pipe_and_preserves_unrelated_child() {
    let cancel = AtomicBool::new(false);
    let mut unrelated = Command::new("sleep").arg("3").spawn().unwrap();
    let mut server =
        Server::spawn(&mut command("sleep 3 & printf R; exit 0"), control(&cancel)).unwrap();
    let (mut reader, writer) = server.streams();
    let mut ready = [0];
    reader.read_exact(&mut ready).unwrap();
    // Keep an independent handle to the inherited pipe as observable evidence of descendant exit.
    let mut pipe = reader.pipe;
    drop(writer);
    assert!(server.wait().unwrap().success());
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match pipe.read(&mut ready) {
            Ok(0) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until, "descendant retained stdout");
                std::thread::sleep(Duration::from_millis(10));
            }
            result => panic!("unexpected pipe state: {result:?}"),
        }
    }
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[test]
fn expired_control_prevents_spawn() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("spawned");
    let mut command = command("touch \"$1\"");
    command.arg("fixture").arg(&marker).stdout(Stdio::null());
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now()),
    };
    assert!(Server::spawn(&mut command, control).is_err());
    assert!(!marker.exists());
}

#[test]
fn cancellation_interrupts_exit_wait() {
    let cancel = AtomicBool::new(false);
    let mut server = Server::spawn(
        &mut command("exec 1>&-; sleep 3"),
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(2)),
        },
    )
    .unwrap();
    let (mut reader, writer) = server.streams();
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    drop((reader, writer));
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
        });
        server.wait().unwrap_err()
    });
    assert!(matches!(
        interruption(&error),
        Some(Interruption::Cancelled)
    ));
}

#[test]
fn unwinding_reaps_owned_child() {
    let cancel = AtomicBool::new(false);
    let server = Server::spawn(&mut command("sleep 3"), control(&cancel)).unwrap();
    let pid = server.pid();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _server = server;
        panic!("simulated caller callback panic");
    }));
    assert!(result.is_err());
    assert!(matches!(
        waitpid(Some(pid), WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
}

#[test]
fn drains_full_stderr_while_writing_full_stdin() {
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    };
    let mut server = Server::spawn(
        &mut command("dd if=/dev/zero bs=65536 count=32 >&2 2>/dev/null; cat >/dev/null; printf R"),
        control,
    )
    .unwrap();
    let (mut reader, mut writer) = server.streams();
    writer.write_all(&vec![0; 2 * 1024 * 1024]).unwrap();
    drop(writer);
    let mut output = Vec::new();
    reader.read_to_end(&mut output).unwrap();
    assert_eq!(output, b"R");
    drop(reader);
    assert!(server.wait().unwrap().success());
}
