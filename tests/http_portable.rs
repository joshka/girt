//! Real Git smart HTTP without girt reference storage or Unix hooks/process adapters.
//! Python and Git serve disposable loopback repositories; all network calls have deadlines.
#![cfg(feature = "http")]
#[path = "support/http_git.rs"]
mod http_git;
#[path = "support/pack_git.rs"]
mod pack_git;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::fetch::{self, Advertisement, FetchError, FetchLimits, KnownHistory};
use girt::push::{self, ForcePolicy, PreparedPush, PushCommand, PushLimits, Status};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::transport::http::{HttpError, HttpRemote};
use girt::{InitKind, ObjectId, PackLimits, ReadLimits, Repository};
use http_git::Server;
use pack_git::{Fixture, git};
use rstest::rstest;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn control(cancel: &AtomicBool) -> TransportControl<'_> {
    TransportControl {
        cancel,
        deadline: Some(Instant::now() + Duration::from_secs(20)),
    }
}

fn all(advertisement: &Advertisement) -> Vec<ObjectId> {
    advertisement
        .refs
        .iter()
        .filter(|r| !r.peeled)
        .map(|r| r.id)
        .collect()
}

#[test]
fn downloads_validates_installs_and_reuses_known_objects() {
    let source = Fixture::new(true, 4);
    let source_index = std::fs::read(&source.index_path).unwrap();
    let server = Server::new(source.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let rt = runtime();
    let downloaded = rt
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    let received = rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            downloaded.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
        })
        .await
        .unwrap()
        .unwrap()
    });
    let root = tempfile::tempdir().unwrap();
    let destination = Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
    received
        .install(&destination, PackLimits::default(), &cancel)
        .unwrap();
    let objects = destination.objects(PackLimits::default()).unwrap();
    let expected = &source.records[0];
    let actual = objects
        .read(expected.0, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(actual.kind(), expected.1);
    assert_eq!(actual.data(), expected.2);
    assert_eq!(
        git(
            &root.path().join("repo"),
            &["cat-file", "blob", &expected.0.to_string()],
            b""
        ),
        expected.2
    );
    let known =
        KnownHistory::new(&objects, received.wants(), FetchLimits::default(), &cancel).unwrap();
    let downloaded = rt
        .block_on(fetch::receive_http(
            &remote,
            all,
            Some(Arc::new(known)),
            FetchLimits::default(),
            control(&cancel),
        ))
        .unwrap();
    let received = downloaded
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap();
    let installed = received
        .install(&destination, PackLimits::default(), &cancel)
        .unwrap();
    assert!(installed.checksum.is_none());
    assert_eq!(server.requests(), ["GET", "POST", "GET"]);
    assert!(
        objects
            .read(source.ordinary, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert!(
        objects
            .read(source.delta, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(std::fs::read(&source.index_path).unwrap(), source_index);
}

#[test]
fn pushes_objects_and_git_publishes_the_remote_ref() {
    let source = Fixture::new(true, 4);
    let root = tempfile::tempdir().unwrap();
    Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
    git(
        &root.path().join("repo"),
        &["config", "http.receivepack", "true"],
        b"",
    );
    let server = Server::new(&root.path().join("repo"), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let bytes = git(source.root.path(), &["rev-parse", "main"], b"");
    let tip: ObjectId = std::str::from_utf8(&bytes).unwrap().trim().parse().unwrap();
    let prepared = PreparedPush::new(
        &source.repo.objects(PackLimits::default()).unwrap(),
        vec![PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: None,
            new: tip,
            force: ForcePolicy::FastForwardOnly,
        }],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = runtime()
        .block_on(push::send_http(&remote, prepared, control(&cancel)))
        .unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(
        git(&root.path().join("repo"), &["rev-parse", "main"], b""),
        bytes
    );
    git(&root.path().join("repo"), &["fsck", "--strict"], b"");
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[rstest]
#[case::unauthorized("", &[], 401)]
#[case::server_failure("failure", &[("Authorization", "secret")], 503)]
#[case::redirect("redirect", &[("Authorization", "secret")], 302)]
fn rejects_http_status_without_retry(
    #[case] fault: &str,
    #[case] headers: &[(&str, &str)],
    #[case] expected: u16,
) {
    let root = tempfile::tempdir().unwrap();
    Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
    let server = Server::new(&root.path().join("repo"), fault, "secret", None);
    let remote = HttpRemote::new(&server.url, headers, &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control(&cancel),
    ));
    assert!(
        matches!(result, Err(FetchError::Http(HttpError::Status(status))) if status == expected)
    );
    assert_eq!(server.requests(), ["GET"]);
}

#[test]
fn stalled_discovery_observes_deadline() {
    let root = tempfile::tempdir().unwrap();
    Repository::init(root.path().join("repo"), InitKind::Bare).unwrap();
    let server = Server::new(&root.path().join("repo"), "stall", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl {
        cancel: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(1)),
    };
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control,
    ));
    assert!(matches!(result, Err(FetchError::Deadline)));
}

#[test]
fn truncated_rpc_never_produces_installable_objects() {
    let source = Fixture::new(true, 4);
    let server = Server::new(source.root.path(), "post-truncate", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        control(&cancel),
    ));
    assert!(matches!(result, Err(FetchError::Http(HttpError::Network))));
    assert_eq!(server.requests(), ["GET", "POST"]);
}
