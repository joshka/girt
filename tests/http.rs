//! Real Git http-backend on disposable loopback servers; Python 3 and Git required.
#![cfg(all(unix, feature = "http"))]
#[path = "support/http_git.rs"]
mod http_git;
#[path = "support/pack_git.rs"]
mod pack_git;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use girt::fetch::{self, Advertisement, FetchError, FetchLimits, KnownHistory};
use girt::push::{self, ForcePolicy, PreparedPush, PushCommand, PushError, PushLimits, Status};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::transport::http::{HttpError, HttpRemote};
use girt::{ObjectId, PackCompression, PackLimits, ReadLimits, Repository};
use http_git::Server;
use pack_git::{Fixture, git};
use rstest::rstest;

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
    git(root.path(), &["config", "http.receivepack", "true"], b"");
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
        b"HTTP next\n",
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 8);
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let limits = FetchLimits::default();
    let rt = runtime();
    let received = rt
        .block_on(fetch::receive_http(&remote, all, None, limits, control))
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
        .block_on(fetch::receive_http(
            &remote,
            all,
            Some(Arc::clone(&known)),
            limits,
            control,
        ))
        .unwrap();

    assert_eq!(server.requests(), ["GET", "POST", "GET"]);
    let new = next(&f);
    let incremental = rt
        .block_on(fetch::receive_http(
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, false, 2);
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
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let rt = runtime();
    let download = rt
        .block_on(fetch::receive_http(
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, false, 2);
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
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let download = runtime()
        .block_on(fetch::receive_http(
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, false, 8);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "", "Bearer supplied", None);
    let remote =
        HttpRemote::new(&server.url, &[("Authorization", "Bearer supplied")], &[]).unwrap();
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
        rt.block_on(push::send_http(&remote, initial, control))
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
        rt.block_on(push::send_http(&remote, incremental, control))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), new);
    let unchanged = prepared(&f, vec![command("refs/heads/main", Some(new), new)], &[new]);
    assert_eq!(unchanged.object_count(), 0);
    assert!(
        rt.block_on(push::send_http(&remote, unchanged, control))
            .unwrap()
            .all_succeeded()
    );
    let noop = prepared(&f, vec![], &[]);
    assert!(
        rt.block_on(push::send_http(&remote, noop, control))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(
        server.requests(),
        ["GET", "POST", "GET", "POST", "GET", "POST", "GET"]
    );
}

#[rstest]
#[case::anonymous(false)]
#[case::explicit(true)]
fn authentication_is_explicit(#[case] supplied: bool) {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "", "Basic Zml4dHVyZTpzZWNyZXQ=", None);
    let headers = if supplied {
        vec![("Authorization", "Basic Zml4dHVyZTpzZWNyZXQ=")]
    } else {
        vec![]
    };
    let remote = HttpRemote::new(&server.url, &headers, &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        TransportControl::new(&cancel),
    ));
    assert_eq!(result.is_ok(), supplied);
    assert_eq!(server.requests(), ["GET"]);
    assert!(!format!("{remote:?} {result:?}").contains("Zml4"));
}

#[rstest]
#[case::redirect("redirect")]
#[case::failure("failure")]
#[case::media("media")]
#[case::duplicate_type("duplicate-type")]
#[case::encoded("encoding")]
#[case::headers("headers")]
#[case::malformed("malformed-header")]
#[case::truncated("truncate")]
#[case::prelude("prelude")]
#[case::version("v2")]
fn rejects_invalid_http_discovery(#[case] fault: &str) {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), fault, "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        TransportControl::new(&cancel),
    ));
    assert!(result.is_err(), "{result:?}");
    assert_eq!(server.requests(), ["GET"]);
}

#[test]
fn truncated_mutating_response_retains_acknowledged_prefix() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "partial", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let old = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(
        &f,
        vec![
            command("refs/heads/main", None, old),
            command("refs/heads/other", None, old),
        ],
        &[],
    );
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(push::send_http(
            &remote,
            prepared,
            TransportControl::new(&cancel),
        ))
        .unwrap_err();
    let PushError::Uncertain { report, .. } = error else {
        panic!("expected uncertain")
    };
    assert_eq!(report.unpack, Some(Status::Ok));
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(report.refs[1].status, None);
    assert_eq!(tip(&dest, "refs/heads/other"), old);
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[rstest]
#[case::deadline(false)]
#[case::cancellation(true)]
fn stalled_discovery_is_interruptible(#[case] cancellation: bool) {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "stall", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let begin = Instant::now();
    let result = std::thread::scope(|scope| {
        if cancellation {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(80));
                cancel.store(true, Ordering::Relaxed);
            });
        }
        let control = TransportControl {
            cancel: &cancel,
            deadline: Some(begin + Duration::from_millis(200)),
        };
        runtime().block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            control,
        ))
    });
    assert!(matches!(
        result,
        Err(FetchError::Cancelled | FetchError::Deadline)
    ));
    assert!(begin.elapsed() < Duration::from_secs(2));
}

fn certificates(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
    fn openssl(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("openssl")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(root.join("ca.cnf"), "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=Girt disposable CA\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n").unwrap();
    std::fs::write(
        root.join("leaf.cnf"),
        "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n",
    )
    .unwrap();
    std::fs::write(root.join("extensions"), "subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n").unwrap();
    openssl(
        root,
        &[
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", "ca.cnf",
            "-keyout", "ca.key", "-out", "ca.pem",
        ],
    );
    openssl(
        root,
        &[
            "req", "-new", "-newkey", "rsa:2048", "-nodes", "-config", "leaf.cnf", "-keyout",
            "leaf.key", "-out", "leaf.csr",
        ],
    );
    openssl(
        root,
        &[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "1",
            "-extfile",
            "extensions",
            "-out",
            "leaf.pem",
        ],
    );
    (
        root.join("leaf.pem"),
        root.join("leaf.key"),
        std::fs::read(root.join("ca.pem")).unwrap(),
    )
}

#[rstest]
#[case::trusted(true, true, true)]
#[case::untrusted(false, true, false)]
#[case::wrong_hostname(true, false, false)]
fn https_validates_chain_and_hostname(
    #[case] trust: bool,
    #[case] hostname: bool,
    #[case] succeeds: bool,
) {
    let (_root, repo) = destination();
    let certs = tempfile::tempdir().unwrap();
    let (cert, key, ca) = certificates(certs.path());
    let server = Server::new(repo.git_dir(), "", "", Some((&cert, &key)));
    let url = if hostname {
        server.url.replace("127.0.0.1", "localhost")
    } else {
        server.url.clone()
    };
    let roots = if trust { vec![ca.as_slice()] } else { vec![] };
    let remote = HttpRemote::new(&url, &[], &roots).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        TransportControl::new(&cancel),
    ));
    assert_eq!(result.is_ok(), succeeds, "{result:?}");
}

#[rstest]
#[case::server_failure("post-failure")]
#[case::stalled_status("post-stall")]
#[case::invalid_rpc_media("post-media")]
fn attempted_push_http_failures_are_uncertain_and_not_retried(#[case] fault: &str) {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), fault, "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
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
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(1)),
    };
    let error = runtime()
        .block_on(push::send_http(&remote, prepared, control))
        .unwrap_err();
    let PushError::Uncertain { report, .. } = error else {
        panic!("expected uncertain")
    };
    assert_eq!(report.refs[0].status, None);
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[test]
fn server_can_accept_one_ref_and_reject_another() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let hook = dest.git_dir().join("hooks/update");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\n[ \"$1\" != refs/heads/reject ]\n").unwrap();
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let id = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(
        &f,
        vec![
            command("refs/heads/main", None, id),
            command("refs/heads/reject", None, id),
        ],
        &[],
    );
    let cancel = AtomicBool::new(false);
    let report = runtime()
        .block_on(push::send_http(
            &remote,
            prepared,
            TransportControl::new(&cancel),
        ))
        .unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert!(matches!(report.refs[1].status, Some(Status::Rejected(_))));
    assert!(!report.all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), id);
}

#[test]
fn download_and_decoding_limits_are_independent() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits {
        max_decode_bytes: 1,
        ..Default::default()
    };
    let downloaded = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            limits,
            TransportControl::new(&cancel),
        ))
        .unwrap();
    assert!(
        downloaded
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .is_err()
    );
    let limits = FetchLimits {
        max_wire_bytes: 16,
        ..Default::default()
    };
    let error = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            limits,
            TransportControl::new(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(error, FetchError::Http(HttpError::Limit(_))));
}

#[test]
fn cancellation_during_stalled_upload_is_uncertain() {
    let (_source_root, source) = destination();
    let (_root, dest) = destination();
    // Incompressible bytes exceed ordinary TCP send buffers; the fixture never consumes the POST.
    let mut state = 0x1234_5678_u64;
    let payload: Vec<u8> = (0..16 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let id = source.loose_objects().write_blob(&payload).unwrap();
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        vec![command("refs/tags/large", None, id)],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(prepared.pack_bytes() > 15 * 1024 * 1024);
    let server = Server::new(dest.git_dir(), "post-stall", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            while server.requests().len() < 2 && start.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            cancel.store(true, Ordering::Relaxed);
        });
        runtime()
            .block_on(push::send_http(
                &remote,
                prepared,
                TransportControl::new(&cancel),
            ))
            .unwrap_err()
    });
    assert!(matches!(
        error,
        PushError::Uncertain {
            cause: girt::push::PushFailure::Cancelled,
            ..
        }
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[test]
fn cancelled_status_body_preserves_completed_acknowledgements() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "partial-stall", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let id = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(
        &f,
        vec![
            command("refs/heads/main", None, id),
            command("refs/heads/other", None, id),
        ],
        &[],
    );
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(1)),
    };
    let error = runtime()
        .block_on(push::send_http(&remote, prepared, control))
        .unwrap_err();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertain")
    };
    assert!(matches!(cause, girt::push::PushFailure::Deadline));
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(report.refs[1].status, None);
    assert_eq!(tip(&dest, "refs/heads/other"), id);
}

#[test]
fn stalled_tls_handshake_observes_deadline() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = HttpRemote::new(
        &format!("https://{}/repo", listener.local_addr().unwrap()),
        &[],
        &[],
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            let (_socket, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });
        runtime().block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            TransportControl {
                cancel: &cancel,
                deadline: Some(start + Duration::from_millis(100)),
            },
        ))
    });
    assert!(matches!(result, Err(FetchError::Deadline)));
}

#[test]
fn push_authentication_rejection_is_not_sent() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "", "Bearer required", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
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
        .block_on(push::send_http(
            &remote,
            prepared,
            TransportControl::new(&cancel),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        PushError::NotSent(girt::push::PushFailure::Http(HttpError::Status(401)))
    ));
    assert_eq!(server.requests(), ["GET"]);
}

#[test]
fn complete_git_report_does_not_hide_truncated_http() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "post-truncate", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let id = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(&f, vec![command("refs/heads/main", None, id)], &[]);
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(push::send_http(
            &remote,
            prepared,
            TransportControl::new(&cancel),
        ))
        .unwrap_err();
    let PushError::Uncertain { report, .. } = error else {
        panic!("expected uncertain")
    };
    assert!(report.all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), id);
}

#[test]
fn https_push_and_fetch_agree_with_git() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let certs = tempfile::tempdir().unwrap();
    let (cert, key, ca) = certificates(certs.path());
    let server = Server::new(dest.git_dir(), "", "", Some((&cert, &key)));
    let url = server.url.replace("127.0.0.1", "localhost");
    let remote = HttpRemote::new(&url, &[], &[&ca]).unwrap();
    let id = tip(&f.repo, "refs/heads/main");
    let prepared = prepared(&f, vec![command("refs/heads/main", None, id)], &[]);
    let cancel = AtomicBool::new(false);
    let rt = runtime();
    assert!(
        rt.block_on(push::send_http(
            &remote,
            prepared,
            TransportControl::new(&cancel)
        ))
        .unwrap()
        .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), id);
    let download = rt
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    let received = download
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap();
    assert!(received.wants().contains(&id));
    let (_copy_root, copy) = destination();
    received
        .install(&copy, girt::PackLimits::default(), &cancel)
        .unwrap();
    assert_eq!(
        git(
            copy.git_dir(),
            &["cat-file", "commit", &id.to_string()],
            b""
        ),
        git(
            dest.git_dir(),
            &["cat-file", "commit", &id.to_string()],
            b""
        )
    );
}

#[test]
fn network_wait_leaves_single_thread_executor_responsive() {
    let (_root, repo) = destination();
    let server = Server::new(repo.git_dir(), "stall", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(async {
        let interrupt = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.store(true, Ordering::Relaxed);
        };
        let transfer = fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        );
        fn is_send<T: Send>(_: &T) {}
        is_send(&transfer);
        let (result, ()) = tokio::join!(transfer, interrupt);
        result
    });
    assert!(matches!(result, Err(FetchError::Cancelled)));
}

#[test]
fn orchestration_installs_and_publishes_on_owned_worker() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
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
        .block_on(request.receive_http(
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
fn clone_download_finishes_on_owned_worker_without_checkout() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("clone");
    let request = girt::clone::CloneRequest::prepare_tracking(
        &path,
        girt::InitKind::Worktree,
        server.url.as_bytes(),
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let rt = runtime();
    let download = rt
        .block_on(request.receive_http(
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let server = Server::new(f.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("clone");
    let request = girt::clone::CloneRequest::prepare_tracking(
        &path,
        girt::InitKind::Bare,
        server.url.as_bytes(),
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let download = runtime()
        .block_on(request.receive_http(
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
