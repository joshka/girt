//! Real Git over disposable loopback OpenSSH; Python 3, sshd, ssh-keygen and Git required.
#![cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
#[path = "support/pack_git.rs"]
mod pack_git;
#[path = "support/ssh_git.rs"]
mod ssh_git;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use girt::fetch::{self, Advertisement, FetchError, FetchLimits, KnownHistory};
use girt::push::{self, ForcePolicy, PreparedPush, PushCommand, PushError, PushLimits, Status};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::transport::ssh::SshError;
use girt::{ObjectId, PackCompression, PackLimits, ReadLimits, Repository};
use pack_git::{Fixture, git};
use rstest::rstest;
use ssh_git::Server;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn all(a: &Advertisement) -> Vec<ObjectId> {
    a.refs.iter().filter(|r| !r.peeled).map(|r| r.id).collect()
}
fn destination() -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--object-format=sha1",
            "--template=",
            "--initial-branch=main",
            ".",
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    (root, repo)
}
fn tip(repo: &Repository, name: &str) -> ObjectId {
    repo.references()
        .unwrap()
        .resolve(&RefName::new(name).unwrap(), 8)
        .unwrap()
        .id
        .unwrap()
}
fn command(name: &str, expected: Option<ObjectId>, new: ObjectId) -> PushCommand {
    PushCommand {
        name: RefName::new(name).unwrap(),
        expected,
        new,
        force: ForcePolicy::FastForwardOnly,
    }
}
fn prepared(f: &Fixture, commands: Vec<PushCommand>, roots: &[ObjectId]) -> PreparedPush {
    let limits = PushLimits {
        compression: PackCompression::Delta(Default::default()),
        ..Default::default()
    };
    PreparedPush::new_excluding(
        &f.repo.objects(PackLimits::default()).unwrap(),
        commands,
        roots,
        limits,
        &AtomicBool::new(false),
    )
    .unwrap()
}
fn next(f: &Fixture) -> ObjectId {
    let old = tip(&f.repo, "refs/heads/main");
    let tree = git(f.root.path(), &["rev-parse", "main^{tree}"], b"");
    let id = git(
        f.root.path(),
        &[
            "commit-tree",
            std::str::from_utf8(&tree).unwrap().trim(),
            "-p",
            &old.to_string(),
        ],
        b"SSH next\n",
    );
    let id = std::str::from_utf8(&id)
        .unwrap()
        .trim()
        .parse::<ObjectId>()
        .unwrap();
    git(
        f.root.path(),
        &["update-ref", "refs/heads/main", &id.to_string()],
        b"",
    );
    id
}
fn verify(f: &Fixture, repo: &Repository) {
    let objects = repo.objects(PackLimits::default()).unwrap();
    for (id, kind, data) in &f.records {
        assert_eq!(
            objects
                .read(*id, ReadLimits::default())
                .unwrap()
                .unwrap()
                .data(),
            data
        );
        assert_eq!(
            git(
                repo.git_dir(),
                &["cat-file", kind.as_str(), &id.to_string()],
                b""
            ),
            *data
        );
    }
    assert!(
        objects
            .read(f.ordinary, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert!(
        objects
            .read(f.delta, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert!(f.index_path.exists());
}

#[test]
fn real_git_full_incremental_and_known_only_fetch() {
    let f = Fixture::new(true, 8);
    let server = Server::new(f.root.path(), "none");
    let remote = server.remote("config");
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let limits = FetchLimits::default();
    let rt = runtime();
    let received = rt
        .block_on(fetch::receive_ssh(&remote, all, None, limits, control))
        .unwrap();
    let received = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            received.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
        })
        .await
        .unwrap()
        .unwrap()
    });
    let (_root, dest) = destination();
    let installed = received
        .install(&dest, girt::PackLimits::default(), &cancel)
        .unwrap();
    let index = dest
        .object_dir()
        .join(format!("pack/pack-{}.idx", installed.checksum.unwrap()));
    let report = git(
        dest.git_dir(),
        &["verify-pack", "-v", index.to_str().unwrap()],
        b"",
    );
    assert!(
        std::str::from_utf8(&report)
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().count() == 7)
    );
    verify(&f, &dest);
    assert!(received.advertisement().refs.iter().any(|r| r.peeled));
    let known = KnownHistory::new(
        &dest.objects(PackLimits::default()).unwrap(),
        received.wants(),
        limits,
        &cancel,
    )
    .unwrap();
    let known = Arc::new(known);
    let retained = Arc::downgrade(&known);
    let noop = rt
        .block_on(fetch::receive_ssh(
            &remote,
            all,
            Some(Arc::clone(&known)),
            limits,
            control,
        ))
        .unwrap();

    let new = next(&f);
    let incremental = rt
        .block_on(fetch::receive_ssh(
            &remote,
            all,
            Some(Arc::clone(&known)),
            limits,
            control,
        ))
        .unwrap();
    // Both downloads keep the exact history alive after its initiating owner disappears.
    drop(known);
    assert!(retained.upgrade().is_some());
    let noop = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            noop.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
        })
        .await
        .unwrap()
        .unwrap()
    });
    assert_eq!(noop.pack_bytes(), 0);
    assert!(retained.upgrade().is_some());
    let incremental = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            incremental.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
        })
        .await
        .unwrap()
        .unwrap()
    });
    assert!(retained.upgrade().is_none());
    // Owning the negotiation snapshot never exempts installation from checking local dependencies.
    let (_missing_root, missing) = destination();
    assert!(matches!(
        noop.install(&missing, girt::PackLimits::default(), &cancel),
        Err(FetchError::Missing(_))
    ));
    assert!(matches!(
        incremental.install(&missing, girt::PackLimits::default(), &cancel),
        Err(FetchError::Missing(_))
    ));
    assert!(
        !missing
            .object_dir()
            .join("pack")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
    noop.install(&dest, girt::PackLimits::default(), &cancel)
        .unwrap();
    assert_eq!(incremental.object_count(), 1);
    incremental
        .install(&dest, girt::PackLimits::default(), &cancel)
        .unwrap();
    assert!(
        dest.objects(PackLimits::default())
            .unwrap()
            .read(new, ReadLimits::default())
            .unwrap()
            .is_some()
    );
}

#[rstest]
#[case::cancelled(true, FetchLimits::default())]
#[case::decode_limit(false, FetchLimits { max_decode_bytes: 0, ..FetchLimits::default() })]
fn failed_worker_validation_releases_negotiated_history(
    #[case] cancelled: bool,
    #[case] limits: FetchLimits,
) {
    let f = Fixture::new(false, 2);
    let cancel = AtomicBool::new(false);
    let known = Arc::new(
        KnownHistory::new(
            &f.repo.objects(PackLimits::default()).unwrap(),
            &[tip(&f.repo, "refs/heads/main")],
            limits,
            &cancel,
        )
        .unwrap(),
    );
    let retained = Arc::downgrade(&known);
    let new = next(&f);
    let server = Server::new(f.root.path(), "none");
    let remote = server.remote("config");
    let rt = runtime();
    let download = rt
        .block_on(fetch::receive_ssh(
            &remote,
            |_| vec![new],
            Some(Arc::clone(&known)),
            limits,
            TransportControl::new(&cancel),
        ))
        .unwrap();
    drop(known);
    assert!(retained.upgrade().is_some());
    let result = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            download.validate(&AtomicBool::new(cancelled), |_| ControlFlow::Continue(()))
        })
        .await
        .unwrap()
    });
    assert!(result.is_err());
    assert!(retained.upgrade().is_none());
}

#[test]
fn discarding_download_releases_negotiated_history() {
    let f = Fixture::new(false, 2);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default();
    let known = Arc::new(
        KnownHistory::new(
            &f.repo.objects(PackLimits::default()).unwrap(),
            &[tip(&f.repo, "refs/heads/main")],
            limits,
            &cancel,
        )
        .unwrap(),
    );
    let retained = Arc::downgrade(&known);
    let server = Server::new(f.root.path(), "none");
    let remote = server.remote("config");
    let download = runtime()
        .block_on(fetch::receive_ssh(
            &remote,
            all,
            Some(known),
            limits,
            TransportControl::new(&cancel),
        ))
        .unwrap();
    assert!(retained.upgrade().is_some());
    drop(download);
    assert!(retained.upgrade().is_none());
}

#[test]
fn real_git_delta_push_incremental_and_empty_commands() {
    let f = Fixture::new(false, 8);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "none");
    let remote = server.remote("config");
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let old = tip(&f.repo, "refs/heads/main");
    let tag = tip(&f.repo, "refs/tags/packed");
    let initial = prepared(
        &f,
        vec![
            command("refs/heads/main", None, old),
            command("refs/tags/packed", None, tag),
        ],
        &[],
    );
    let rt = runtime();
    assert!(
        rt.block_on(push::send_ssh(&remote, &initial, control))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), old);
    assert_eq!(tip(&dest, "refs/tags/packed"), tag);
    verify(&f, &dest);
    let new = next(&f);
    let incremental = prepared(&f, vec![command("refs/heads/main", Some(old), new)], &[old]);
    assert_eq!(incremental.object_count(), 1);
    assert!(
        rt.block_on(push::send_ssh(&remote, &incremental, control))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), new);
    let unchanged = prepared(&f, vec![command("refs/heads/main", Some(new), new)], &[new]);
    assert_eq!(unchanged.object_count(), 0);
    assert!(
        rt.block_on(push::send_ssh(&remote, &unchanged, control))
            .unwrap()
            .all_succeeded()
    );
    let noop = prepared(&f, vec![], &[]);
    assert!(
        rt.block_on(push::send_ssh(&remote, &noop, control))
            .unwrap()
            .all_succeeded()
    );
}

fn deadline(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    }
}

#[rstest]
#[case::unknown("unknown")]
#[case::changed("changed")]
#[case::authentication("unauthorized")]
fn rejects_untrusted_hosts_and_failed_authentication(#[case] config: &str) {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "none");
    let cancel = AtomicBool::new(false);
    let remote = server.remote(config);
    let error = runtime()
        .block_on(fetch::receive_ssh(
            &remote,
            all,
            None,
            FetchLimits::default(),
            deadline(&cancel),
        ))
        .unwrap_err();
    assert!(
        matches!(error, FetchError::Ssh(SshError::Exit(Some(255)))),
        "{error:?}"
    );
    assert!(!format!("{error:?} {error}").contains(server.root.to_str().unwrap()));
    assert!(!server.root.join("requests").exists());
}

#[test]
fn trust_failure_is_not_sent_for_push() {
    let f = Fixture::new(false, 2);
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "none");
    let prepared = prepared(
        &f,
        vec![command(
            "refs/heads/main",
            None,
            tip(&f.repo, "refs/heads/main"),
        )],
        &[],
    );
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(push::send_ssh(
            &server.remote("unknown"),
            &prepared,
            deadline(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        PushError::NotSent(girt::push::PushFailure::Ssh(SshError::Exit(Some(255))))
    ));
}

#[rstest]
#[case::spaces("space path")]
#[case::quotes("quote ' double \"")]
#[case::shell("$(touch INJECTED); & `touch INJECTED` #")]
fn literal_repository_path_round_trips(#[case] path: &str) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join(path);
    std::fs::create_dir(&repo).unwrap();
    git(
        &repo,
        &["init", "--bare", "--template=", "--object-format=sha1", "."],
        b"",
    );
    let server = Server::new(&repo, "none");
    let cancel = AtomicBool::new(false);
    let download = runtime()
        .block_on(fetch::receive_ssh(
            &server.remote("config"),
            all,
            None,
            FetchLimits::default(),
            deadline(&cancel),
        ))
        .unwrap();
    assert_eq!(
        download
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .unwrap()
            .object_count(),
        0
    );
    assert!(!root.path().join("INJECTED").exists());
}

#[test]
fn git_rejection_is_a_report_and_preserves_mixed_outcomes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(false, 2);
    let (_root, repo) = destination();
    let hook = repo.git_dir().join("hooks/update");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\n[ \"$1\" != refs/heads/rejected ]\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = Server::new(repo.git_dir(), "none");
    let id = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(
        &f,
        vec![
            command("refs/heads/main", None, id),
            command("refs/heads/rejected", None, id),
        ],
        &[],
    );
    let cancel = AtomicBool::new(false);
    let report = runtime()
        .block_on(push::send_ssh(
            &server.remote("config"),
            &prepared,
            deadline(&cancel),
        ))
        .unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert!(matches!(report.refs[1].status, Some(Status::Rejected(_))));
    assert_eq!(tip(&repo, "refs/heads/main"), id);
}

#[rstest]
#[case::diagnostics("diagnostics")]
#[case::normal("none")]
fn stderr_cannot_block_service_io(#[case] fault: &str) {
    let f = Fixture::new(false, 2);
    let server = Server::new(f.root.path(), fault);
    let cancel = AtomicBool::new(false);
    let remote = server.remote("config");
    let download = runtime()
        .block_on(fetch::receive_ssh(
            &remote,
            all,
            None,
            FetchLimits::default(),
            deadline(&cancel),
        ))
        .unwrap();
    assert!(
        download
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .unwrap()
            .object_count()
            > 0
    );
}

#[test]
fn stalled_service_has_a_deadline() {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "before-advertisement");
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(fetch::receive_ssh(
            &server.remote("config"),
            all,
            None,
            FetchLimits::default(),
            TransportControl {
                cancel: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(2)),
            },
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Deadline), "{error:?}");
}

#[rstest]
#[case::response("after-report")]
#[case::exit("exit-stall")]
#[case::partial("partial-report")]
fn cancellation_preserves_acknowledged_statuses(#[case] fault: &str) {
    let f = Fixture::new(false, 2);
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), fault);
    let prepared = prepared(
        &f,
        vec![command(
            "refs/heads/main",
            None,
            tip(&f.repo, "refs/heads/main"),
        )],
        &[],
    );
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let end = Instant::now() + Duration::from_secs(5);
            while !server.root.join("report").exists() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            // Let the client consume the just-emitted status before requesting cancellation.
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
        });
        let result = runtime().block_on(push::send_ssh(
            &server.remote("config"),
            &prepared,
            TransportControl::new(&cancel),
        ));
        let PushError::Uncertain { cause, report } = result.unwrap_err() else {
            panic!("push must be uncertain")
        };
        assert!(
            matches!(cause, girt::push::PushFailure::Cancelled),
            "{cause:?}"
        );
        assert_eq!(report.unpack, Some(Status::Ok));
        assert_eq!(report.refs[0].status, Some(Status::Ok));
    });
}

#[test]
fn nonzero_service_exit_retains_complete_report() {
    let f = Fixture::new(false, 2);
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "exit-failure");
    let prepared = prepared(
        &f,
        vec![command(
            "refs/heads/main",
            None,
            tip(&f.repo, "refs/heads/main"),
        )],
        &[],
    );
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(push::send_ssh(
            &server.remote("config"),
            &prepared,
            deadline(&cancel),
        ))
        .unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("push must be uncertain")
    };
    assert!(matches!(
        cause,
        girt::push::PushFailure::Ssh(SshError::Exit(Some(42)))
    ));
    assert!(report.all_succeeded());
}

#[test]
fn stalled_ssh_handshake_expires_without_using_an_account() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cancel = AtomicBool::new(false);
    let remote = girt::transport::ssh::SshRemote::new(
        "127.0.0.1",
        "fixture",
        port,
        "repo",
        std::path::Path::new("/usr/bin/ssh"),
        std::path::Path::new("/dev/null"),
    )
    .unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"SSH-2.0-girt-stall-fixture\r\n").unwrap();
            let mut bytes = [0; 4096];
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            while std::io::Read::read(&mut stream, &mut bytes).unwrap_or(0) != 0 {}
        });
        let error = runtime()
            .block_on(fetch::receive_ssh(
                &remote,
                all,
                None,
                FetchLimits::default(),
                TransportControl {
                    cancel: &cancel,
                    deadline: Some(Instant::now() + Duration::from_millis(300)),
                },
            ))
            .unwrap_err();
        assert!(matches!(error, FetchError::Deadline));
    });
}

#[test]
fn blocked_upload_expires_with_unknown_outcome() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "--template=", "."], b"");
    let repo = Repository::open(root.path()).unwrap();
    let mut state = 17_u64;
    let bytes: Vec<_> = (0..4_000_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let id = repo.loose_objects().unwrap().write_blob(&bytes).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = PreparedPush::new(
        &repo.objects(PackLimits::default()).unwrap(),
        vec![command("refs/tags/large", None, id)],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let server = Server::new(root.path(), "blocked-upload");
    let error = runtime()
        .block_on(push::send_ssh(
            &server.remote("config"),
            &prepared,
            TransportControl {
                cancel: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(2)),
            },
        ))
        .unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("push must be uncertain")
    };
    assert!(matches!(cause, girt::push::PushFailure::Deadline));
    assert_eq!(report.refs[0].status, None);
}

#[test]
fn caller_config_cannot_disable_host_verification_or_batch_mode() {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "none");
    let path = server.root.join("unknown");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        format!("StrictHostKeyChecking no\nBatchMode no\nUpdateHostKeys yes\n{config}"),
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(fetch::receive_ssh(
            &server.remote("unknown"),
            all,
            None,
            FetchLimits::default(),
            deadline(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Ssh(SshError::Exit(Some(255)))));
    assert_eq!(std::fs::read(server.root.join("empty_hosts")).unwrap(), b"");
}

#[test]
fn malformed_advertisement_is_rejected() {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "malformed");
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(fetch::receive_ssh(
            &server.remote("config"),
            all,
            None,
            FetchLimits::default(),
            deadline(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Ssh(SshError::Protocol(_))));
}

#[rstest]
#[case::advertisement(FetchLimits { max_advertisement_bytes: 4, ..FetchLimits::default() })]
#[case::body(FetchLimits { max_wire_bytes: 1024, ..FetchLimits::default() })]
fn download_respects_wire_budgets(#[case] limits: FetchLimits) {
    let f = Fixture::new(false, 4);
    let server = Server::new(f.root.path(), "none");
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(fetch::receive_ssh(
            &server.remote("config"),
            all,
            None,
            limits,
            deadline(&cancel),
        ))
        .unwrap_err();
    assert!(
        matches!(error, FetchError::Ssh(SshError::Limit)),
        "{error:?}"
    );
}

#[test]
fn cancellation_after_real_git_ref_commit_is_uncertain() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(false, 2);
    let (_root, repo) = destination();
    let hook = repo.git_dir().join("hooks/post-receive");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\ntouch committed\nsleep 10\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = Server::new(repo.git_dir(), "none");
    let id = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(&f, vec![command("refs/heads/main", None, id)], &[]);
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let end = Instant::now() + Duration::from_secs(5);
            while !repo.git_dir().join("committed").exists() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel.store(true, Ordering::Relaxed);
        });
        let result = runtime().block_on(push::send_ssh(
            &server.remote("config"),
            &prepared,
            TransportControl::new(&cancel),
        ));
        assert!(
            matches!(
                result,
                Err(PushError::Uncertain {
                    cause: girt::push::PushFailure::Cancelled,
                    ..
                })
            ),
            "{result:?}"
        );
    });
    assert!(repo.git_dir().join("committed").exists());
    assert_eq!(tip(&repo, "refs/heads/main"), id);
}

#[test]
fn orchestration_installs_and_publishes_on_owned_worker() {
    let f = Fixture::new(true, 4);
    let server = Server::new(f.root.path(), "none");
    let remote = server.remote("config");
    let (_root, dest) = destination();
    let destination_path = dest.git_dir().to_path_buf();
    let specs = girt::remote::Refspecs::parse(
        girt::remote::Direction::Fetch,
        [
            "refs/heads/*:refs/remotes/origin/*",
            "refs/tags/*:refs/tags/*",
        ],
    )
    .unwrap();
    let request = fetch::FetchRequest::prepare(
        dest,
        specs,
        Default::default(),
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let rt = runtime();
    let download = rt
        .block_on(request.receive_ssh(
            &remote,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    let report = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            download
                .validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
                .unwrap()
                .finish(fetch::FetchUpdateLimits::default(), &AtomicBool::new(false))
                .unwrap()
        })
        .await
        .unwrap()
    });
    let destination = Repository::open(destination_path).unwrap();
    assert_eq!(report.references.len(), 2);
    assert!(report.installed.is_some());
    assert_eq!(
        tip(&destination, "refs/remotes/origin/main"),
        tip(&f.repo, "refs/heads/main")
    );
    assert_eq!(
        tip(&destination, "refs/tags/packed"),
        tip(&f.repo, "refs/tags/packed")
    );
    git(
        destination.git_dir(),
        &["fsck", "--full", "--no-reflogs"],
        b"",
    );
}

#[test]
fn orchestration_rejects_malformed_transfer_without_installation() {
    let f = Fixture::new(false, 2);
    let server = Server::new(f.root.path(), "malformed");
    let (_root, dest) = destination();
    let path = dest.git_dir().to_path_buf();
    let specs = girt::remote::Refspecs::parse(
        girt::remote::Direction::Fetch,
        ["refs/heads/*:refs/remotes/origin/*"],
    )
    .unwrap();
    let request = fetch::FetchRequest::prepare(
        dest,
        specs,
        Default::default(),
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(request.receive_ssh(
        &server.remote("config"),
        None,
        FetchLimits::default(),
        deadline(&cancel),
    ));
    assert!(matches!(
        result,
        Err(fetch::FetchWorkflowError::Transfer(_))
    ));
    let repo = Repository::open(path).unwrap();
    assert!(repo.references().unwrap().list().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn clone_download_finishes_on_owned_worker_without_checkout() {
    let f = Fixture::new(true, 4);
    let server = Server::new(f.root.path(), "none");
    let remote = server.remote("config");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("clone");
    let request = girt::clone::CloneRequest::prepare_tracking(
        &path,
        girt::InitKind::Worktree,
        b"ssh://fixture/repository",
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let rt = runtime();
    let download = rt
        .block_on(request.receive_ssh(
            &remote,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    assert!(!path.exists());
    let report = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            download
                .validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
                .unwrap()
                .finish(fetch::FetchUpdateLimits::default(), &AtomicBool::new(false))
                .unwrap()
        })
        .await
        .unwrap()
    });
    let repo = report.repository.unwrap();
    assert_eq!(
        tip(&repo, "refs/heads/main"),
        tip(&f.repo, "refs/heads/main")
    );
    assert_eq!(
        tip(&repo, "refs/tags/packed"),
        tip(&f.repo, "refs/tags/packed")
    );
    assert!(
        girt::remote::Remote::find(repo.config(), b"origin")
            .unwrap()
            .is_some()
    );
    assert!(!repo.git_dir().join("index").exists());
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    git(repo.git_dir(), &["fsck", "--strict", "--full"], b"");
}

#[test]
fn clone_validation_cancellation_leaves_destination_absent() {
    let f = Fixture::new(true, 4);
    let server = Server::new(f.root.path(), "none");
    let remote = server.remote("config");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("clone");
    let request = girt::clone::CloneRequest::prepare_tracking(
        &path,
        girt::InitKind::Bare,
        b"ssh://fixture/repository",
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let download = runtime()
        .block_on(request.receive_ssh(
            &remote,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    let result = download.validate(&AtomicBool::new(true), |_| ControlFlow::Continue(()));
    assert!(matches!(
        result,
        Err(girt::clone::CloneTransferError::Transfer(
            FetchError::Cancelled
        ))
    ));
    assert!(!path.exists());
}

#[test]
fn clone_malformed_transfer_leaves_destination_absent() {
    let f = Fixture::new(false, 2);
    let server = Server::new(f.root.path(), "malformed");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("clone");
    let request = girt::clone::CloneRequest::prepare_tracking(
        &path,
        girt::InitKind::Bare,
        b"ssh://fixture/repository",
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(request.receive_ssh(
        &server.remote("config"),
        FetchLimits::default(),
        deadline(&cancel),
    ));
    assert!(matches!(
        result,
        Err(girt::clone::CloneTransferError::Transfer(_))
    ));
    assert!(!path.exists());
}
