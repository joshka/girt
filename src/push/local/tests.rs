//! Independently encoded disposable servers exercise interruption without changing PATH.
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rstest::rstest;

use super::send_server;
use crate::push::{
    ForcePolicy, PreparedPush, PushCommand, PushError, PushFailure, PushLimits, Status,
};
use crate::refs::RefName;
use crate::transport::TransportControl;
use crate::{PackLimits, Repository};

struct Fixture {
    root: tempfile::TempDir,
    prepared: PreparedPush,
}
impl Fixture {
    fn new() -> Self {
        Self::with_blob(b"fixture")
    }

    fn with_blob(payload: &[u8]) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("objects")).unwrap();
        fs::create_dir(root.path().join("refs")).unwrap();
        fs::write(root.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(root.path().join("config"), "[core]\nbare=true\n").unwrap();
        let repo = Repository::open(root.path()).unwrap();
        let id = repo.loose_objects().write_blob(payload).unwrap();
        let commands = ["refs/tags/one", "refs/tags/two"].map(|name| PushCommand {
            name: RefName::new(name).unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::default(),
        });
        let prepared = PreparedPush::new(
            &repo.objects(PackLimits::default()).unwrap(),
            commands.to_vec(),
            PushLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        fs::write(root.path().join("advertisement"), b"005d0000000000000000000000000000000000000000 capabilities^{}\0report-status object-format=sha10000").unwrap();
        Self { root, prepared }
    }
    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .current_dir(self.root.path())
            .arg("-c")
            .arg(format!(
                "(sleep 8; kill -KILL -$$) </dev/null >/dev/null 2>&1 &\n{script}"
            ))
            .arg("fixture")
            .arg((self.prepared.request.len() + self.prepared.pack.len()).to_string())
            .arg(self.prepared.request.len().to_string());
        command
    }
    fn status(&self, complete: bool) {
        let mut bytes = b"000eunpack ok\n0015ok refs/tags/one\n".to_vec();
        if complete {
            bytes.extend_from_slice(b"0015ok refs/tags/two\n0000");
        }
        fs::write(self.root.path().join("status"), bytes).unwrap();
    }
}
fn deadline(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(1)),
    }
}

#[test]
fn silent_server_times_out_before_commands() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let result = send_server(&mut f.command("sleep 3"), &f.prepared, deadline(&cancel));
    assert!(matches!(
        result,
        Err(PushError::NotSent(PushFailure::Deadline))
    ));
}

#[rstest]
#[case::prefix(false, "cat status; sleep 3", None)]
#[case::complete_before_eof(true, "cat status; sleep 3", Some(Status::Ok))]
#[case::complete_before_exit(true, "cat status; exec 1>&-; sleep 3", Some(Status::Ok))]
fn deadline_preserves_acknowledgements(
    #[case] complete: bool,
    #[case] after: &str,
    #[case] second: Option<Status>,
) {
    let f = Fixture::new();
    f.status(complete);
    let script =
        format!("cat advertisement; dd bs=1 count=\"$1\" of=/dev/null 2>/dev/null; {after}");
    let cancel = AtomicBool::new(false);
    let error = send_server(&mut f.command(&script), &f.prepared, deadline(&cancel)).unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertain result: {error:?}")
    };
    assert!(matches!(cause, PushFailure::Deadline));
    assert_eq!(report.unpack, Some(Status::Ok));
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(report.refs[1].status, second);
}

// Cancel only after the client has consumed the complete first acknowledgement. This avoids
// inferring client progress from sleeps or from when the server wrote its bytes.
struct CancelAfterPrefix<'a, R> {
    reader: R,
    remaining: usize,
    cancel: &'a AtomicBool,
}
impl<R: std::io::Read> std::io::Read for CancelAfterPrefix<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.reader.read(bytes)?;
        self.remaining -= count;
        if self.remaining == 0 {
            self.cancel.store(true, Ordering::Relaxed);
        }
        Ok(count)
    }
}

#[test]
fn cancellation_preserves_acknowledged_prefix() {
    let f = Fixture::new();
    f.status(false);
    let cancel = AtomicBool::new(false);
    let mut command = f.command(
        "cat advertisement; dd bs=1 count=\"$1\" of=/dev/null 2>/dev/null; cat status; sleep 3",
    );
    let mut server = crate::transport::Server::spawn(&mut command, deadline(&cancel)).unwrap();
    let (reader, mut writer) = server.streams();
    let remaining = fs::read(f.root.path().join("advertisement")).unwrap().len()
        + fs::read(f.root.path().join("status")).unwrap().len();
    let mut reader = CancelAfterPrefix {
        reader,
        remaining,
        cancel: &cancel,
    };
    let error = crate::push::send(&mut reader, &mut writer, &f.prepared, &cancel).unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertain result")
    };
    assert!(matches!(cause, PushFailure::Cancelled));
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(report.refs[1].status, None);
}

#[test]
fn protocol_rejection_is_not_replaced_by_cleanup_deadline() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let result = send_server(
        &mut f.command("printf '000cERR no!\\n'; sleep 3"),
        &f.prepared,
        deadline(&cancel),
    );
    assert!(
        matches!(result, Err(PushError::NotSent(PushFailure::Remote(message))) if message == b"no!\n")
    );
}

#[test]
fn deadline_during_blocked_pack_write_is_uncertain() {
    // Original deterministic bytes defeat compression enough to exceed ordinary pipe capacity.
    let mut state = 1u32;
    let payload: Vec<u8> = (0..1024 * 1024)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        })
        .collect();
    let f = Fixture::with_blob(&payload);
    assert!(f.prepared.pack_bytes() > 512 * 1024);
    let cancel = AtomicBool::new(false);
    let error = send_server(
        &mut f.command("cat advertisement; sleep 3"),
        &f.prepared,
        deadline(&cancel),
    )
    .unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertain push")
    };
    assert!(matches!(cause, PushFailure::Deadline));
    assert_eq!(report.unpack, None);
    assert_eq!(report.refs[0].status, None);
    assert_eq!(report.refs[1].status, None);
}

#[test]
fn early_rejection_does_not_deadlock_upload_and_preserves_status() {
    let mut state = 1u32;
    let payload: Vec<u8> = (0..1024 * 1024)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        })
        .collect();
    let f = Fixture::with_blob(&payload);
    let reason = vec![b'x'; 60000];
    write_large_rejection(&f, &reason);
    // Both directions exceed pipe capacity. The peer finishes its finite rejection before
    // resuming input consumption; a write-all-before-read client cannot make progress.
    let cancel = AtomicBool::new(false);
    let result = send_server(
        &mut f.command("cat advertisement; dd bs=1 count=\"$2\" of=/dev/null 2>/dev/null; cat status; python3 -c 'import sys; sys.stdin.buffer.read(int(sys.argv[1])-int(sys.argv[2]))' \"$1\" \"$2\""),
        &f.prepared, deadline(&cancel),
    );
    let report = result.unwrap();
    assert_eq!(report.unpack, Some(Status::Rejected(b"rejected".to_vec())));
    assert_eq!(
        report.refs[0].status,
        Some(Status::Rejected(reason.clone()))
    );
    assert_eq!(report.refs[1].status, Some(Status::Rejected(reason)));
}

#[test]
fn early_status_limit_preserves_retained_acknowledgement() {
    let mut f = Fixture::new();
    f.status(true);
    f.prepared.limits.max_status_bytes = 14; // Complete "unpack ok" packet only.
    let cancel = AtomicBool::new(false);
    let error = send_server(
        &mut f.command(
            "cat advertisement; dd bs=1 count=\"$2\" of=/dev/null 2>/dev/null; cat status; sleep 3",
        ),
        &f.prepared,
        deadline(&cancel),
    )
    .unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertain push");
    };
    assert!(matches!(cause, PushFailure::Limit("wire bytes")));
    assert_eq!(report.unpack, Some(Status::Ok));
    assert_eq!(report.refs[0].status, None);
}

fn write_large_rejection(f: &Fixture, reason: &[u8]) {
    let mut status = Vec::new();
    for line in [
        b"unpack rejected\n".to_vec(),
        [b"ng refs/tags/one ".as_slice(), reason, b"\n"].concat(),
        [b"ng refs/tags/two ".as_slice(), reason, b"\n"].concat(),
    ] {
        status.extend(format!("{:04x}", line.len() + 4).bytes());
        status.extend(line);
    }
    status.extend(b"0000");
    fs::write(f.root.path().join("status"), status).unwrap();
}
