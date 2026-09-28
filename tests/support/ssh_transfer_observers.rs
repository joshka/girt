//! Original byte-stream executable with callback-to-producer handshake; no network or accounts.
use std::ops::ControlFlow;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use girt::fetch::{self, FetchLimits, FetchOptions};
use girt::push::{self, PushError, PushFailure, PushLimits, SshPushOutcome};
use girt::remote::{ProtocolEnvironment, Remote};
use girt::transport::TransportControl;
use girt::transport::ssh::{OpenSshOptions, SshError, SshRemote};
use girt::{ObjectFormat, ObjectId};
use rstest::rstest;

struct Peer {
    root: tempfile::TempDir,
    remote: SshRemote,
}

impl Peer {
    fn new(advertisement: &[u8], first: &[u8], rest: &[u8], mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("advertisement"), advertisement).unwrap();
        std::fs::write(root.path().join("first"), first).unwrap();
        std::fs::write(root.path().join("rest"), rest).unwrap();
        std::fs::write(root.path().join("mode"), mode).unwrap();
        let executable = root.path().join("ssh");
        // EOF on protocol stdin proves the entire attempted request has been consumed. The
        // callback must acknowledge the first notice before the producer releases the tail.
        std::fs::write(
            &executable,
            br#"#!/usr/bin/env python3
import os,pathlib,sys,time
root=pathlib.Path(os.environ['FIXTURE_ROOT'])
(root/'pid').write_text(str(os.getpid()))
def write(fd,data):
    while data:
        count=os.write(fd,data)
        data=data[count:]
write(2,b'local diagnostic\xff without newline')
write(1,(root/'advertisement').read_bytes())
(root/'request').write_bytes(sys.stdin.buffer.read())
write(1,(root/'first').read_bytes())
mode=(root/'mode').read_text()
if mode in ('gated','cancel','exit'):
    while not (root/'seen').exists(): time.sleep(0.01)
if mode == 'cancel': time.sleep(60)
write(1,(root/'rest').read_bytes())
if mode in ('exit', 'exit-direct'): sys.exit(42)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = girt::Config::parse(b"[remote \"r\"]\nurl = host:repo\n").unwrap();
        let destination = Remote::find(&config, b"r")
            .unwrap()
            .unwrap()
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        let options = OpenSshOptions {
            default_executable: executable,
            environment: [
                ("PATH".into(), std::env::var_os("PATH").unwrap()),
                ("FIXTURE_ROOT".into(), root.path().as_os_str().to_owned()),
            ]
            .into(),
            ..Default::default()
        };
        let remote = SshRemote::openssh(&config, &destination, options).unwrap();
        Self { root, remote }
    }

    fn acknowledge(&self) {
        std::fs::write(self.root.path().join("seen"), b"seen").unwrap();
    }

    fn request(&self) -> Vec<u8> {
        std::fs::read(self.root.path().join("request")).unwrap()
    }

    fn assert_reaped(&self) {
        let pid: i32 = std::fs::read_to_string(self.root.path().join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        assert!(matches!(
            rustix::process::waitid(
                rustix::process::WaitId::Pid(pid),
                rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG
            ),
            Err(rustix::io::Errno::CHILD)
        ));
    }
}

fn packet(bytes: &[u8]) -> Vec<u8> {
    [format!("{:04x}", bytes.len() + 4).as_bytes(), bytes].concat()
}

fn push_advertisement(format: ObjectFormat, sideband: bool) -> Vec<u8> {
    let caps = if sideband { " side-band-64k" } else { "" };
    [
        packet(
            format!(
                "{} capabilities^{{}}\0report-status{caps} object-format={format}\n",
                ObjectId::null(format)
            )
            .as_bytes(),
        ),
        b"0000".to_vec(),
    ]
    .concat()
}

fn fetch_advertisement(id: ObjectId) -> Vec<u8> {
    [
        packet(
            format!(
                "{id} refs/heads/main\0side-band-64k shallow object-format={}\n",
                id.format()
            )
            .as_bytes(),
        ),
        b"0000".to_vec(),
    ]
    .concat()
}

fn status() -> Vec<u8> {
    [
        packet(b"unpack ok\n"),
        packet(b"ok refs/heads/main\n"),
        b"0000".to_vec(),
    ]
    .concat()
}

fn success() -> Vec<u8> {
    [
        packet(&[b"\x01".as_slice(), &status()].concat()),
        b"0000".to_vec(),
    ]
    .concat()
}

fn control(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(10)),
    }
}

fn prepared(format: ObjectFormat, limits: PushLimits) -> push::PreparedPush {
    let source = super::Fixture::new(format, false, 2);
    let id = super::tip(&source.repo, "refs/heads/main");
    push::PreparedPush::new(
        &source.repo.objects(girt::PackLimits::default()).unwrap(),
        vec![super::command("refs/heads/main", None, id)],
        limits,
        &AtomicBool::new(false),
    )
    .unwrap()
    .with_progress()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn checked_push_delivers_separate_live_output_and_retains_notices(#[case] format: ObjectFormat) {
    let peer = Peer::new(
        &push_advertisement(format, true),
        &packet(b"\x02notice\xff\r"),
        &success(),
        "gated",
    );
    let prepared = prepared(format, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let mut diagnostics = Vec::new();
    let mut progress = Vec::new();
    let outcome = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| true,
            |bytes| diagnostics.extend_from_slice(bytes),
            |bytes| {
                progress.push(bytes.to_vec());
                peer.acknowledge();
            },
        ))
        .unwrap();
    let SshPushOutcome::Sent(report) = outcome else {
        panic!("declined")
    };
    assert!(report.all_succeeded());
    assert_eq!(progress, [b"notice\xff\r".to_vec()]);
    assert_eq!(report.progress, progress);
    assert_eq!(diagnostics, b"local diagnostic\xff without newline");
    peer.assert_reaped();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn checked_push_declines_with_only_flush_and_no_pack(#[case] format: ObjectFormat) {
    let peer = Peer::new(&push_advertisement(format, true), &[], &[], "none");
    let prepared = prepared(format, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let outcome = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |advertisement| {
                assert!(
                    advertisement
                        .target(&girt::refs::RefName::new(b"refs/heads/main").unwrap())
                        .is_none()
                );
                false
            },
            |_| {},
            |_| panic!("declined progress"),
        ))
        .unwrap();
    assert!(matches!(outcome, SshPushOutcome::Declined));
    assert_eq!(peer.request(), b"0000");
    peer.assert_reaped();
}

#[test]
fn checked_push_cancellation_retains_live_notice_and_reaps() {
    let peer = Peer::new(
        &push_advertisement(ObjectFormat::Sha1, true),
        &packet(b"\x02first"),
        &[],
        "cancel",
    );
    let prepared = prepared(ObjectFormat::Sha1, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| true,
            |_| {},
            |_| {
                peer.acknowledge();
                cancel.store(true, Ordering::Relaxed);
            },
        ))
        .unwrap_err();
    assert!(
        matches!(error, PushError::Uncertain { cause: PushFailure::Cancelled, report } if report.progress == [b"first".to_vec()] && report.refs[0].attempted)
    );
    peer.assert_reaped();
}

#[test]
fn checked_push_failed_exit_preserves_acknowledgements_and_progress() {
    let first = [packet(b"\x02first"), success()].concat();
    let peer = Peer::new(
        &push_advertisement(ObjectFormat::Sha1, true),
        &first,
        &[],
        "exit",
    );
    let prepared = prepared(ObjectFormat::Sha1, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| true,
            |_| {},
            |_| peer.acknowledge(),
        ))
        .unwrap_err();
    assert!(
        matches!(error, PushError::Uncertain { cause: PushFailure::Ssh(SshError::Exit(Some(42))), report } if report.all_succeeded() && report.progress == [b"first".to_vec()])
    );
    peer.assert_reaped();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn fetch_workflow_delivers_live_notices_and_validation_replays(#[case] format: ObjectFormat) {
    let source = super::Fixture::new(format, true, 4);
    let id = super::tip(&source.repo, "refs/heads/main");
    let pack = super::git(
        source.root.path(),
        &["pack-objects", "--stdout", "--revs"],
        format!("{id}\n").as_bytes(),
    );
    let first = [packet(b"NAK\n"), packet(b"\x02notice\xff\r")].concat();
    let rest = [
        packet(&[b"\x01".as_slice(), &pack].concat()),
        b"0000".to_vec(),
    ]
    .concat();
    let peer = Peer::new(&fetch_advertisement(id), &first, &rest, "gated");
    let (_root, destination) = super::destination_for(format);
    let specs = girt::remote::Refspecs::parse(
        girt::remote::Direction::Fetch,
        ["refs/heads/*:refs/remotes/origin/*"],
    )
    .unwrap();
    let request = fetch::FetchRequest::prepare(
        destination,
        specs,
        Default::default(),
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut diagnostics = Vec::new();
    let mut notices = Vec::new();
    let transfer = request.receive_ssh_with_progress(
        &peer.remote,
        None,
        FetchLimits::default(),
        control(&cancel),
        |bytes| diagnostics.extend_from_slice(bytes),
        |bytes| {
            notices.push(bytes.to_vec());
            peer.acknowledge();
        },
    );
    fn is_send<T: Send>(_: &T) {}
    is_send(&transfer);
    let download = super::runtime().block_on(transfer).unwrap();
    assert_eq!(diagnostics, b"local diagnostic\xff without newline");
    assert_eq!(notices, [b"notice\xff\r".to_vec()]);
    let mut replay = Vec::new();
    let mut validation = Vec::new();
    let _ready = download
        .validate_with_progress(
            &cancel,
            |bytes| {
                replay.push(bytes.to_vec());
                ControlFlow::Continue(())
            },
            |event| validation.push(event),
        )
        .unwrap();
    assert_eq!(replay, notices);
    assert!(validation.last().unwrap().complete);
    peer.assert_reaped();
}

#[test]
fn fetch_live_notice_cancellation_never_returns_download() {
    let id = ObjectId::from_bytes(ObjectFormat::Sha1, &[1; 20]).unwrap();
    let first = [packet(b"NAK\n"), packet(b"\x02first")].concat();
    let peer = Peer::new(&fetch_advertisement(id), &first, &[], "cancel");
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(fetch::receive_ssh_with_progress(
            &peer.remote,
            super::all,
            None,
            FetchOptions {
                limits: FetchLimits::default(),
                depth: None,
            },
            control(&cancel),
            |_| {},
            |_| {
                peer.acknowledge();
                cancel.store(true, Ordering::Relaxed);
            },
        ))
        .unwrap_err();
    assert!(matches!(error, fetch::FetchError::Cancelled));
    peer.assert_reaped();
}

#[test]
fn push_notice_observer_cannot_see_bytes_outside_status_budget() {
    let first = [
        packet(b"\x02first"),
        packet(b"\x02outside"),
        b"0000".to_vec(),
    ]
    .concat();
    let peer = Peer::new(
        &push_advertisement(ObjectFormat::Sha1, true),
        &first,
        &[],
        "none",
    );
    let prepared = prepared(
        ObjectFormat::Sha1,
        PushLimits {
            max_status_bytes: 15,
            ..PushLimits::default()
        },
    );
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let error = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| true,
            |_| {},
            |bytes| notices.push(bytes.to_vec()),
        ))
        .unwrap_err();
    assert_eq!(notices, [b"first".to_vec()]);
    assert!(
        matches!(error, PushError::Uncertain { cause: PushFailure::Ssh(SshError::Limit), report } if report.progress == notices)
    );
    peer.assert_reaped();
}

#[test]
fn fetch_notice_observer_cannot_see_bytes_outside_wire_budget() {
    let id = ObjectId::from_bytes(ObjectFormat::Sha1, &[1; 20]).unwrap();
    let advertisement = fetch_advertisement(id);
    let first = [packet(b"NAK\n"), packet(b"\x02first")].concat();
    let rest = packet(b"\x02outside");
    let limit = advertisement.len() + first.len() + 3;
    let peer = Peer::new(&advertisement, &first, &rest, "gated");
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let error = super::runtime()
        .block_on(fetch::receive_ssh_with_progress(
            &peer.remote,
            super::all,
            None,
            FetchOptions {
                limits: FetchLimits {
                    max_wire_bytes: limit,
                    ..FetchLimits::default()
                },
                depth: None,
            },
            control(&cancel),
            |_| {},
            |bytes| {
                notices.push(bytes.to_vec());
                peer.acknowledge();
            },
        ))
        .unwrap_err();
    assert_eq!(notices, [b"first".to_vec()]);
    assert!(matches!(error, fetch::FetchError::Ssh(SshError::Limit)));
    peer.assert_reaped();
}

#[test]
fn fetch_unoffered_ack_suppresses_notices_and_fails_validation() {
    let id = ObjectId::from_bytes(ObjectFormat::Sha1, &[1; 20]).unwrap();
    let first = [
        packet(format!("ACK {id}\n").as_bytes()),
        packet(b"\x02invalid"),
        b"0000".to_vec(),
    ]
    .concat();
    let peer = Peer::new(&fetch_advertisement(id), &first, &[], "none");
    let cancel = AtomicBool::new(false);
    let downloaded = super::runtime()
        .block_on(fetch::receive_ssh_with_progress(
            &peer.remote,
            super::all,
            None,
            FetchOptions {
                limits: FetchLimits::default(),
                depth: None,
            },
            control(&cancel),
            |_| {},
            |_| panic!("notice after invalid acknowledgement"),
        ))
        .unwrap();
    let error = downloaded
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap_err();
    assert!(matches!(
        error,
        fetch::FetchError::Protocol("expected ACK of an offered have")
    ));
    peer.assert_reaped();
}

#[test]
fn push_without_sideband_never_delivers_remote_notices() {
    let peer = Peer::new(
        &push_advertisement(ObjectFormat::Sha1, false),
        &status(),
        &[],
        "none",
    );
    let prepared = prepared(ObjectFormat::Sha1, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let outcome = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| true,
            |_| {},
            |_| panic!("unnegotiated notice"),
        ))
        .unwrap();
    assert!(
        matches!(outcome, SshPushOutcome::Sent(report) if report.all_succeeded() && report.progress.is_empty())
    );
    peer.assert_reaped();
}

#[rstest]
#[case::remote_error(b"0009\x03fail0006\x02z")]
#[case::invalid_header(b"zzzz0006\x02z")]
#[case::truncated(b"0009\x02z")]
fn push_malformed_tail_retains_notices_and_uncertainty(#[case] rest: &[u8]) {
    let peer = Peer::new(
        &push_advertisement(ObjectFormat::Sha1, true),
        &packet(b"\x02first"),
        rest,
        "gated",
    );
    let prepared = prepared(ObjectFormat::Sha1, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let error = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| true,
            |_| {},
            |bytes| {
                notices.push(bytes.to_vec());
                peer.acknowledge();
            },
        ))
        .unwrap_err();
    assert_eq!(notices, [b"first".to_vec()]);
    assert!(
        matches!(error, PushError::Uncertain { report, .. } if report.refs[0].attempted && report.refs[0].status.is_none() && report.progress == notices)
    );
    peer.assert_reaped();
}

#[test]
fn live_advertised_tip_can_decline_a_prepared_new_reference() {
    let id = ObjectId::from_bytes(ObjectFormat::Sha1, &[1; 20]).unwrap();
    let advertisement = [
        packet(format!("{id} refs/heads/main\0report-status\n").as_bytes()),
        b"0000".to_vec(),
    ]
    .concat();
    let peer = Peer::new(&advertisement, &[], &[], "none");
    let prepared = prepared(ObjectFormat::Sha1, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let main = girt::refs::RefName::new(b"refs/heads/main").unwrap();
    let outcome = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |advertisement| {
                assert_eq!(advertisement.target(&main), Some(id));
                advertisement.target(&main).is_none()
            },
            |_| {},
            |_| {},
        ))
        .unwrap();
    assert!(matches!(outcome, SshPushOutcome::Declined));
    assert_eq!(peer.request(), b"0000");
    peer.assert_reaped();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn real_git_decline_exits_without_updating_refs(#[case] format: ObjectFormat) {
    let (_root, destination) = super::destination_for(format);
    let server = super::Server::new(destination.git_dir(), "none");
    let prepared = prepared(format, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let outcome = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &server.remote("config"),
            &prepared,
            control(&cancel),
            |_| false,
            |_| {},
            |_| {},
        ))
        .unwrap();
    assert!(matches!(outcome, SshPushOutcome::Declined));
    assert!(destination.references().unwrap().list().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(server.root.join("requests"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn declined_cleanup_failure_is_not_sent_and_never_retried() {
    let peer = Peer::new(
        &push_advertisement(ObjectFormat::Sha1, true),
        &[],
        &[],
        "exit-direct",
    );
    let prepared = prepared(ObjectFormat::Sha1, PushLimits::default());
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(push::send_ssh_checked_with_progress(
            &peer.remote,
            &prepared,
            control(&cancel),
            |_| false,
            |_| {},
            |_| {},
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        PushError::NotSent(PushFailure::Ssh(SshError::Exit(Some(42))))
    ));
    assert_eq!(peer.request(), b"0000");
    peer.assert_reaped();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn depth_fetch_observers_preserve_real_git_shallow_response(#[case] format: ObjectFormat) {
    let source = super::Fixture::new(format, true, 8);
    let server = super::Server::new(source.root.path(), "none");
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let download = super::runtime()
        .block_on(fetch::receive_ssh_with_progress(
            &server.remote("config"),
            super::all,
            None,
            FetchOptions {
                limits: FetchLimits::default(),
                depth: std::num::NonZeroU32::new(1),
            },
            control(&cancel),
            |_| {},
            |bytes| notices.push(bytes.to_vec()),
        ))
        .unwrap();
    let mut replay = Vec::new();
    let received = download
        .validate(&cancel, |bytes| {
            replay.push(bytes.to_vec());
            ControlFlow::Continue(())
        })
        .unwrap();
    assert!(!received.shallow_roots().is_empty());
    assert!(!notices.is_empty());
    assert_eq!(notices, replay);
}
