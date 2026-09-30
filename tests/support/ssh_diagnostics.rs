//! Independently written local processes exercise diagnostics without accounts or network access.
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use girt::fetch::{self, FetchError, FetchLimits};
use girt::remote::{ProtocolEnvironment, Remote};
use girt::transport::TransportControl;
use girt::transport::ssh::{OpenSshOptions, SshError, SshRemote};

struct Fixture {
    root: tempfile::TempDir,
    remote: SshRemote,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("ssh");
        let script = format!(
            "#!/usr/bin/env python3\nimport os,pathlib,sys,time\nroot=pathlib.Path(os.environ['FIXTURE_ROOT'])\n(root/'pid').write_text(str(os.getpid()))\n{body}\n"
        );
        std::fs::write(&executable, script).unwrap();
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

    fn assert_reaped(&self) {
        let pid = std::fs::read_to_string(self.root.path().join("pid")).unwrap();
        let pid = rustix::process::Pid::from_raw(pid.parse().unwrap()).unwrap();
        assert!(matches!(
            rustix::process::waitid(
                rustix::process::WaitId::Pid(pid),
                rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG
            ),
            Err(rustix::io::Errno::CHILD)
        ));
    }
}

fn control(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    }
}

#[test]
fn partial_diagnostics_arrive_before_advertisement_and_tail_is_preserved() {
    let fixture = Fixture::new(
        "os.write(2,b'prompt\\xff without newline')\nwhile not (root/'seen').exists(): time.sleep(0.01)\nos.write(1,b'0000')\nsys.stdin.buffer.read()\nos.write(2,b'\\ntail without newline')",
    );
    let cancel = AtomicBool::new(false);
    let mut received = Vec::new();
    let result = super::runtime().block_on(fetch::discover_ssh_with_diagnostics(
        &fixture.remote,
        FetchLimits::default(),
        control(&cancel),
        |bytes| {
            received.extend_from_slice(bytes);
            std::fs::write(fixture.root.path().join("seen"), b"yes").unwrap();
        },
    ));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        received,
        b"prompt\xff without newline\ntail without newline"
    );
    fixture.assert_reaped();
}

#[test]
fn flooding_diagnostics_use_bounded_chunks_without_blocking_protocol() {
    let fixture = Fixture::new(
        "for _ in range(512): os.write(2,b'x'*4096)\nos.write(1,b'0000')\nsys.stdin.buffer.read()",
    );
    let cancel = AtomicBool::new(false);
    let mut received = 0;
    let mut largest = 0;
    let result = super::runtime().block_on(fetch::discover_ssh_with_diagnostics(
        &fixture.remote,
        FetchLimits::default(),
        control(&cancel),
        |bytes| {
            received += bytes.len();
            largest = largest.max(bytes.len());
        },
    ));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(received, 512 * 4096);
    assert!(largest <= 8192);
    fixture.assert_reaped();
}

#[test]
fn flooding_diagnostics_obey_deadline_and_reap_process() {
    let fixture = Fixture::new("while True: os.write(2,b'x'*4096)");
    let cancel = AtomicBool::new(false);
    let mut received = 0;
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    };
    let error = super::runtime()
        .block_on(fetch::discover_ssh_with_diagnostics(
            &fixture.remote,
            FetchLimits::default(),
            control,
            |bytes| {
                received += bytes.len();
            },
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Deadline));
    assert!(received > 0);
    fixture.assert_reaped();
}

#[test]
fn cancellation_while_delivering_diagnostics_reaps_process() {
    let fixture = Fixture::new("while True: os.write(2,b'x'*4096)");
    let cancel = AtomicBool::new(false);
    let error = super::runtime()
        .block_on(fetch::discover_ssh_with_diagnostics(
            &fixture.remote,
            FetchLimits::default(),
            control(&cancel),
            |_| {
                cancel.store(true, Ordering::Relaxed);
            },
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Cancelled));
    fixture.assert_reaped();
}

#[test]
fn discarded_and_observed_diagnostics_preserve_redacted_exit_error() {
    let fixture = Fixture::new("os.write(2,b'secret diagnostic')\nsys.exit(42)");
    let cancel = AtomicBool::new(false);
    let discarded = super::runtime()
        .block_on(fetch::discover_ssh(
            &fixture.remote,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(
        discarded,
        FetchError::Ssh(SshError::Exit(Some(42)))
    ));
    let mut observed = Vec::new();
    let error = super::runtime()
        .block_on(fetch::discover_ssh_with_diagnostics(
            &fixture.remote,
            FetchLimits::default(),
            control(&cancel),
            |bytes| observed.extend_from_slice(bytes),
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Ssh(SshError::Exit(Some(42)))));
    assert_eq!(observed, b"secret diagnostic");
    assert!(!format!("{error:?} {error} {discarded:?} {discarded}").contains("secret"));
    fixture.assert_reaped();
}

#[test]
fn display_io_errors_do_not_change_successful_discovery() {
    use std::io::Write as _;
    let fixture =
        Fixture::new("os.write(2,b'notice')\nos.write(1,b'0000')\nsys.stdin.buffer.read()");
    let cancel = AtomicBool::new(false);
    let mut destination = &mut [][..];
    let mut failed = false;
    let result = super::runtime().block_on(fetch::discover_ssh_with_diagnostics(
        &fixture.remote,
        FetchLimits::default(),
        control(&cancel),
        |bytes| {
            failed |= destination.write_all(bytes).is_err();
        },
    ));
    assert!(result.is_ok(), "{result:?}");
    assert!(failed);
    fixture.assert_reaped();
}

#[test]
fn callback_panic_still_reaps_owned_process() {
    let fixture = Fixture::new("os.write(2,b'notice')\ntime.sleep(60)");
    let cancel = AtomicBool::new(false);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::runtime().block_on(fetch::discover_ssh_with_diagnostics(
            &fixture.remote,
            FetchLimits::default(),
            control(&cancel),
            |_| panic!("display panicked"),
        ))
    }));
    assert!(panic.is_err());
    fixture.assert_reaped();
}
