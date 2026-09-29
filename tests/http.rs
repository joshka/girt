//! Real Git http-backend on disposable loopback servers; Python 3 and Git required.
#![cfg(feature = "http")]
#[path = "support/connect_proxy.rs"]
mod connect_proxy;
#[path = "support/http_git.rs"]
mod http_git;
#[path = "support/pack_git.rs"]
mod pack_git;

use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use girt::fetch::{self, Advertisement, FetchError, FetchLimits, KnownHistory};
use girt::push::{
    self, ForcePolicy, HttpPushOutcome, PreparedPush, PushCommand, PushError, PushFailure,
    PushLimits, Status,
};
use girt::refs::RefName;
use girt::remote::{ProtocolEnvironment, Remote};
use girt::transport::TransportControl;
use girt::transport::http::{HttpEnvironment, HttpError, HttpRemote, HttpSettings};
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn discovery_reports_http_head_without_fetch_rpc(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let server = Server::new(fixture.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime()
        .block_on(fetch::discover_http(
            &remote,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    assert!(matches!(result.head, fetch::RemoteHead::Symbolic { .. }));
    assert_eq!(result.object_format, format);
    assert_eq!(server.requests().len(), 1);
}
fn all(a: &Advertisement) -> Vec<ObjectId> {
    a.refs.iter().filter(|r| !r.peeled).map(|r| r.id).collect()
}
fn destination() -> (tempfile::TempDir, Repository) {
    destination_for(girt::ObjectFormat::Sha1)
}
fn destination_for(format: girt::ObjectFormat) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let format = format!("--object-format={format}");
    git(
        root.path(),
        &[
            "init",
            "--bare",
            &format,
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

#[test]
fn real_git_sha256_http_fetch_installs_objects() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha256, true, 8);
    let server = Server::new(fixture.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let downloaded = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    let received = downloaded
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap();
    let (_root, destination) = destination_for(girt::ObjectFormat::Sha256);
    received
        .install(&destination, PackLimits::default(), &cancel)
        .unwrap();
    assert!(
        destination
            .objects(PackLimits::default())
            .unwrap()
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .is_some()
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn http_depth_fetch_reports_boundary(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let server = Server::new(fixture.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let downloaded = runtime()
        .block_on(fetch::receive_http_with_depth_and_progress(
            &remote,
            all,
            None,
            NonZeroU32::new(1),
            FetchLimits::default(),
            TransportControl::new(&cancel),
            |bytes| notices.push(bytes.to_vec()),
        ))
        .unwrap();
    let mut replay = Vec::new();
    let received = downloaded
        .validate(&cancel, |bytes| {
            replay.push(bytes.to_vec());
            ControlFlow::Continue(())
        })
        .unwrap();
    assert!(!received.shallow_roots().is_empty());
    assert_eq!(notices, replay);
}

fn finish_depth(ready: fetch::FetchReady, cancel: &AtomicBool) -> fetch::FetchReport {
    ready
        .finish(fetch::FetchUpdateLimits::default(), cancel)
        .unwrap()
}

fn finish_depth_retained(ready: fetch::FetchReady, cancel: &AtomicBool) -> fetch::FetchReport {
    let installed = ready
        .install_retained(fetch::FetchUpdateLimits::default(), cancel)
        .unwrap();
    assert!(installed.retention().path().exists());
    assert!(!installed.report().shallow_published);
    let (report, retention) = installed.finish(cancel).unwrap();
    retention.release().unwrap();
    report
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1, finish_depth)]
#[case::sha256(girt::ObjectFormat::Sha256, finish_depth)]
#[case::retained_sha1(girt::ObjectFormat::Sha1, finish_depth_retained)]
#[case::retained_sha256(girt::ObjectFormat::Sha256, finish_depth_retained)]
fn http_workflow_publishes_depth_and_tracking_ref(
    #[case] format: girt::ObjectFormat,
    #[case] finish: fn(fetch::FetchReady, &AtomicBool) -> fetch::FetchReport,
) {
    let fixture = Fixture::new(format, true, 8);
    let server = Server::new(fixture.root.path(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let (_root, destination) = destination_for(format);
    let path = destination.git_dir().to_path_buf();
    let specs = girt::remote::Refspecs::parse(
        girt::remote::Direction::Fetch,
        ["refs/heads/main:refs/remotes/origin/main"],
    )
    .unwrap();
    let request = fetch::FetchRequest::prepare(
        destination,
        specs,
        Default::default(),
        girt::refs::Reflog::Preserve,
    )
    .unwrap()
    .with_depth(NonZeroU32::new(1).unwrap());
    let cancel = AtomicBool::new(false);
    let download = runtime()
        .block_on(request.receive_http(
            &remote,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    let ready = download
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap();
    let report = finish(ready, &cancel);
    let opened = Repository::open(path).unwrap();
    assert!(report.shallow_published);
    assert_eq!(
        opened.shallow_roots().iter().collect::<Vec<_>>(),
        vec![tip(&fixture.repo, "refs/heads/main")]
    );
    assert_eq!(
        tip(&opened, "refs/remotes/origin/main"),
        tip(&fixture.repo, "refs/heads/main")
    );
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
        &[
            "verify-pack",
            "-v",
            index
                .strip_prefix(dest.git_dir())
                .unwrap()
                .to_str()
                .unwrap(),
        ],
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
    )
    .with_progress();
    let rt = runtime();
    let mut progress = Vec::new();
    let outcome = rt
        .block_on(push::send_http_checked_with_progress(
            &remote,
            initial,
            control,
            |_| true,
            |message| progress.push(message.to_vec()),
        ))
        .unwrap();
    let HttpPushOutcome::Sent(report) = outcome else {
        panic!("push declined")
    };
    assert!(report.all_succeeded());
    assert_eq!(report.progress, progress);
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
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn checked_http_push_can_decline_after_validated_discovery(#[case] format: girt::ObjectFormat) {
    let source = Fixture::new(format, true, 4);
    let (_root, dest) = destination_for(format);
    let old = tip(&source.repo, "refs/heads/main");
    git(
        dest.git_dir(),
        &[
            "fetch",
            source.root.path().to_str().unwrap(),
            "refs/heads/main:refs/heads/main",
        ],
        b"",
    );
    let new = next(&source);
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = prepared(
        &source,
        vec![command("refs/heads/main", Some(old), new)],
        &[],
    );
    let main = RefName::new("refs/heads/main").unwrap();
    let missing = RefName::new("refs/heads/missing").unwrap();
    let outcome = runtime()
        .block_on(push::send_http_checked(
            &remote,
            prepared,
            TransportControl::new(&cancel),
            |advertisement| {
                assert_eq!(advertisement.target(&main), Some(old));
                assert_eq!(advertisement.target(&missing), None);
                false
            },
        ))
        .unwrap();
    assert!(matches!(outcome, HttpPushOutcome::Declined));
    assert_eq!(server.requests(), ["GET"]);
    assert_eq!(tip(&dest, "refs/heads/main"), old);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn checked_http_push_sends_after_accepting_discovery(#[case] format: girt::ObjectFormat) {
    let source = Fixture::new(format, true, 4);
    let (_root, dest) = destination_for(format);
    let id = tip(&source.repo, "refs/heads/main");
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = prepared(&source, vec![command("refs/heads/main", None, id)], &[]);
    let main = RefName::new("refs/heads/main").unwrap();
    let outcome = runtime()
        .block_on(push::send_http_checked(
            &remote,
            prepared,
            TransportControl::new(&cancel),
            |advertisement| {
                assert_eq!(advertisement.target(&main), None);
                true
            },
        ))
        .unwrap();
    assert!(matches!(outcome, HttpPushOutcome::Sent(report) if report.all_succeeded()));
    assert_eq!(server.requests(), ["GET", "POST"]);
    assert_eq!(tip(&dest, "refs/heads/main"), id);
}

#[test]
fn checked_http_push_declines_stale_lease_before_post() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let old = tip(&source.repo, "refs/heads/main");
    git(
        dest.git_dir(),
        &[
            "fetch",
            source.root.path().to_str().unwrap(),
            "refs/heads/main:refs/heads/main",
        ],
        b"",
    );
    let new = next(&source);
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = prepared(&source, vec![command("refs/heads/main", None, new)], &[]);
    let main = RefName::new("refs/heads/main").unwrap();
    let outcome = runtime()
        .block_on(push::send_http_checked(
            &remote,
            prepared,
            TransportControl::new(&cancel),
            |advertisement| advertisement.target(&main).is_none(),
        ))
        .unwrap();
    assert!(matches!(outcome, HttpPushOutcome::Declined));
    assert_eq!(server.requests(), ["GET"]);
    assert_eq!(tip(&dest, "refs/heads/main"), old);
}

#[test]
fn checked_http_push_declines_whole_batch_for_current_tip() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let current = tip(&source.repo, "refs/heads/main");
    git(
        dest.git_dir(),
        &[
            "fetch",
            source.root.path().to_str().unwrap(),
            "refs/heads/main:refs/heads/main",
        ],
        b"",
    );
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = prepared(
        &source,
        vec![
            command("refs/heads/main", Some(current), current),
            command("refs/tags/new", None, current),
        ],
        &[],
    );
    let main = RefName::new("refs/heads/main").unwrap();
    let outcome = runtime()
        .block_on(push::send_http_checked(
            &remote,
            prepared,
            TransportControl::new(&cancel),
            |advertisement| advertisement.target(&main) != Some(current),
        ))
        .unwrap();
    assert!(matches!(outcome, HttpPushOutcome::Declined));
    assert_eq!(server.requests(), ["GET"]);
    assert_eq!(tip(&dest, "refs/heads/main"), current);
    assert_eq!(
        dest.references()
            .unwrap()
            .read(&RefName::new("refs/tags/new").unwrap())
            .unwrap(),
        None
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn checked_http_push_keeps_receiver_lease_after_discovery(#[case] format: girt::ObjectFormat) {
    let source = Fixture::new(format, true, 4);
    let (_root, dest) = destination_for(format);
    let old = tip(&source.repo, "refs/heads/main");
    git(
        dest.git_dir(),
        &[
            "fetch",
            source.root.path().to_str().unwrap(),
            "refs/heads/main:refs/heads/main",
        ],
        b"",
    );
    let new = next(&source);
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = prepared(
        &source,
        vec![command("refs/heads/main", Some(old), new)],
        &[],
    );
    let main = RefName::new("refs/heads/main").unwrap();
    let outcome = runtime()
        .block_on(push::send_http_checked(
            &remote,
            prepared,
            TransportControl::new(&cancel),
            |advertisement| {
                assert_eq!(advertisement.target(&main), Some(old));
                dest.references()
                    .unwrap()
                    .delete_without_reflog(
                        &main,
                        girt::refs::Expected::Value(girt::refs::Target::Direct(old)),
                    )
                    .unwrap();
                true
            },
        ))
        .unwrap();
    assert!(matches!(
        outcome,
        HttpPushOutcome::Sent(report)
            if matches!(report.refs[0].status, Some(Status::Rejected(_)))
    ));
    assert_eq!(server.requests(), ["GET", "POST"]);
    assert_eq!(dest.references().unwrap().read(&main).unwrap(), None);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn mixed_push_commands_and_deletion_agree_with_git(#[case] format: girt::ObjectFormat) {
    let source = Fixture::new(format, true, 4);
    let (_root, dest) = destination_for(format);
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let id = tip(&source.repo, "refs/heads/main");
    let objects = source.repo.objects(PackLimits::default()).unwrap();
    let initial = PreparedPush::new(
        &objects,
        vec![command("refs/for/main", None, id)],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let rt = runtime();
    let mut progress = Vec::new();
    let outcome = rt
        .block_on(push::send_http_checked_with_progress(
            &remote,
            initial,
            control,
            |_| true,
            |message| progress.push(message.to_vec()),
        ))
        .unwrap();
    let HttpPushOutcome::Sent(report) = outcome else {
        panic!("push declined")
    };
    assert!(report.all_succeeded());
    assert_eq!(report.progress, progress);
    assert_eq!(tip(&dest, "refs/for/main"), id);

    let commands = vec![
        command("refs/heads/main", None, id),
        command("refs/tags/deletable", None, id),
        command("refs/for/main", Some(id), ObjectId::null(format)),
    ];
    let mixed = PreparedPush::new(&objects, commands, PushLimits::default(), &cancel).unwrap();
    let report = rt
        .block_on(push::send_http(&remote, mixed, control))
        .unwrap();
    assert!(report.all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), id);
    assert_eq!(
        dest.references()
            .unwrap()
            .read(&RefName::new("refs/for/main").unwrap())
            .unwrap(),
        None
    );
    let deletion = PreparedPush::new(
        &objects,
        vec![command(
            "refs/tags/deletable",
            Some(id),
            ObjectId::null(format),
        )],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(deletion.pack_bytes(), 0);
    let report = rt
        .block_on(push::send_http(&remote, deletion, control))
        .unwrap();
    assert!(report.all_succeeded(), "{report:?}");
    assert_eq!(
        dest.references()
            .unwrap()
            .read(&RefName::new("refs/tags/deletable").unwrap())
            .unwrap(),
        None
    );
}

#[test]
fn push_options_require_advertisement_and_reach_git() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let id = tip(&source.repo, "refs/heads/main");
    let objects = source.repo.objects(PackLimits::default()).unwrap();
    let prepare = || {
        PreparedPush::new(
            &objects,
            vec![command("refs/heads/main", None, id)],
            PushLimits::default(),
            &cancel,
        )
        .unwrap()
        .with_push_options(vec![b"review=123".to_vec()])
        .unwrap()
    };
    let rt = runtime();
    assert!(matches!(
        rt.block_on(push::send_http(&remote, prepare(), control)),
        Err(PushError::NotSent(PushFailure::Unsupported(_)))
    ));
    git(
        dest.git_dir(),
        &["config", "receive.advertisePushOptions", "true"],
        b"",
    );
    assert!(
        rt.block_on(push::send_http(&remote, prepare(), control))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), id);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn stale_wire_lease_preserves_independent_success(#[case] format: girt::ObjectFormat) {
    let source = Fixture::new(format, true, 4);
    let (_root, dest) = destination_for(format);
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let control = TransportControl::new(&cancel);
    let id = tip(&source.repo, "refs/heads/main");
    let objects = source.repo.objects(PackLimits::default()).unwrap();
    let initial = PreparedPush::new(
        &objects,
        vec![command("refs/heads/main", None, id)],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let rt = runtime();
    let mut progress = Vec::new();
    let outcome = rt
        .block_on(push::send_http_checked_with_progress(
            &remote,
            initial,
            control,
            |_| true,
            |message| progress.push(message.to_vec()),
        ))
        .unwrap();
    let HttpPushOutcome::Sent(report) = outcome else {
        panic!("push declined")
    };
    assert!(report.all_succeeded());
    assert_eq!(report.progress, progress);
    let mixed = PreparedPush::new(
        &objects,
        vec![
            command("refs/heads/main", None, id),
            command("refs/tags/independent", None, id),
        ],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = rt
        .block_on(push::send_http(&remote, mixed, control))
        .unwrap();
    assert!(matches!(report.refs[0].status, Some(Status::Rejected(_))));
    assert_eq!(report.refs[1].status, Some(Status::Ok));
    assert_eq!(tip(&dest, "refs/heads/main"), id);
    assert_eq!(tip(&dest, "refs/tags/independent"), id);
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
        panic!(
            "expected uncertain, got {error:?}; requests: {:?}",
            server.requests()
        )
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

// The timeout is a fixture watchdog, not an injected transport deadline. Fault assertions
// must reach POST regardless of how long Git discovery takes on the native host.
fn observed_post_fault(fault: &str) -> PushError {
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
    let error = runtime().block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            push::send_http(&remote, prepared, TransportControl::new(&cancel)),
        )
        .await
        .expect("HTTP fixture did not finish within its watchdog")
        .unwrap_err()
    });
    assert_eq!(server.requests(), ["GET", "POST"], "{error:?}");
    error
}

#[rstest]
#[case::ordinary("post-failure")]
#[case::slow_discovery("delayed-discovery/post-failure")]
fn server_failure_after_post_is_uncertain_and_not_retried(#[case] fault: &str) {
    let error = observed_post_fault(fault);
    assert!(
        matches!(
            &error,
            PushError::Uncertain { cause: PushFailure::Http(HttpError::Status(503)), report }
                if report.refs[0].status.is_none()
        ),
        "{error:?}"
    );
}

#[rstest]
#[case::ordinary("post-media")]
#[case::slow_discovery("delayed-discovery/post-media")]
fn invalid_media_after_post_is_uncertain_and_not_retried(#[case] fault: &str) {
    let error = observed_post_fault(fault);
    assert!(
        matches!(
            &error,
            PushError::Uncertain { cause: PushFailure::Http(HttpError::Protocol("media type (dumb HTTP unsupported)")), report }
                if report.refs[0].status.is_none()
        ),
        "{error:?}"
    );
}

#[rstest]
#[case::ordinary("post-stall")]
#[case::slow_discovery("delayed-discovery/post-stall")]
fn cancellation_after_observed_post_is_uncertain_and_not_retried(#[case] fault: &str) {
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
    let (error, observed_post) = std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            let end = Instant::now() + Duration::from_secs(30);
            while !server.requests().iter().any(|method| method == "POST") {
                if Instant::now() >= end {
                    cancel.store(true, Ordering::Relaxed);
                    return false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            cancel.store(true, Ordering::Relaxed);
            true
        });
        let error = runtime()
            .block_on(push::send_http(
                &remote,
                prepared,
                TransportControl::new(&cancel),
            ))
            .unwrap_err();
        (error, observer.join().unwrap())
    });
    assert!(
        observed_post,
        "POST was not observed before fixture watchdog: {error:?}"
    );
    assert!(
        matches!(
            &error,
            PushError::Uncertain { cause: PushFailure::Cancelled, report }
                if report.refs[0].status.is_none()
        ),
        "{error:?}"
    );
    assert_eq!(server.requests(), ["GET", "POST"]);
}

#[rstest]
#[case::server_failure("delayed-discovery/post-failure")]
#[case::stalled_status("delayed-discovery/post-stall")]
#[case::invalid_media("delayed-discovery/post-media")]
fn discovery_deadline_does_not_reach_post_fault(#[case] fault: &str) {
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
    let error = runtime()
        .block_on(push::send_http(
            &remote,
            prepared,
            TransportControl {
                cancel: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(1)),
            },
        ))
        .unwrap_err();
    eprintln!("{fault}: {error:?}; requests: {:?}", server.requests());
    assert!(
        matches!(&error, PushError::NotSent(PushFailure::Deadline)),
        "{error:?}"
    );
    assert!(!server.requests().iter().any(|method| method == "POST"));
}

#[cfg(unix)]
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

#[cfg(unix)]
#[test]
fn git_proc_receive_reports_rewritten_destination() {
    use std::os::unix::fs::PermissionsExt;
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination();
    let id = tip(&source.repo, "refs/heads/main");
    git(
        dest.git_dir(),
        &[
            "fetch",
            source.root.path().to_str().unwrap(),
            "refs/heads/main",
        ],
        b"",
    );
    git(
        dest.git_dir(),
        &["config", "receive.procReceiveRefs", "refs/for/"],
        b"",
    );
    let hook = dest.git_dir().join("hooks/proc-receive");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        r##"#!/usr/bin/env python3
import subprocess
import sys

reader = sys.stdin.buffer
writer = sys.stdout.buffer

def read_packet():
    header = reader.read(4)
    length = int(header, 16)
    return None if length == 0 else reader.read(length - 4)

def write_packet(data):
    writer.write(f"{len(data) + 4:04x}".encode() + data)
    writer.flush()

assert read_packet().startswith(b"version=1")
assert read_packet() is None
write_packet(b"version=1")
writer.write(b"0000")
writer.flush()
old, new, name = read_packet().strip().split(b" ", 2)
assert read_packet() is None
subprocess.run(["git", "update-ref", "refs/heads/reviewed", new.decode()], check=True)
write_packet(b"ok " + name)
write_packet(b"option refname refs/heads/reviewed")
write_packet(b"option old-oid " + old)
write_packet(b"option new-oid " + new)
write_packet(b"option forced-update")
writer.write(b"0000")
writer.flush()
"##,
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = Server::new(dest.git_dir(), "", "", None);
    let remote = HttpRemote::new(&server.url, &[], &[]).unwrap();
    let prepared = prepared(&source, vec![command("refs/for/main", None, id)], &[]);
    let cancel = AtomicBool::new(false);
    let report = runtime()
        .block_on(push::send_http(
            &remote,
            prepared,
            TransportControl::new(&cancel),
        ))
        .unwrap();
    assert!(report.all_succeeded());
    assert_eq!(report.refs[0].rewrites.len(), 1);
    assert_eq!(
        report.refs[0].rewrites[0].name,
        Some(RefName::new("refs/heads/reviewed").unwrap())
    );
    assert_eq!(report.refs[0].rewrites[0].new, Some(id));
    assert!(report.refs[0].rewrites[0].forced);
    assert_eq!(tip(&dest, "refs/heads/reviewed"), id);
    assert_eq!(
        dest.references()
            .unwrap()
            .read(&RefName::new("refs/for/main").unwrap())
            .unwrap(),
        None
    );
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
        panic!(
            "expected uncertain, got {error:?}; requests: {:?}",
            server.requests()
        )
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
        panic!(
            "expected uncertain, got {error:?}; requests: {:?}",
            server.requests()
        )
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
    let remote = configured_https(&url, Some(ca), "");
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

fn configured_https(url: &str, ca: Option<Vec<u8>>, policy: &str) -> HttpRemote {
    let config =
        girt::Config::parse(format!("[remote \"r\"]\nurl={url}\n{policy}").as_bytes()).unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let environment = HttpEnvironment {
        ssl_ca_info: ca,
        ..Default::default()
    };
    let settings =
        HttpSettings::resolve_for_remote(&config, b"r", &destination, &environment).unwrap();
    HttpRemote::configured(settings).unwrap()
}

#[rstest]
#[case::matching_bundle(true, true, Ok(()), 2)]
#[case::wrong_bundle(false, true, Err(true), 0)]
#[case::wrong_hostname(true, false, Err(true), 0)]
fn configured_https_selects_ca_bundle_and_verifies_peer(
    #[case] trust: bool,
    #[case] hostname: bool,
    #[case] expected: Result<(), bool>,
    #[case] requests: usize,
) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let certs = tempfile::tempdir().unwrap();
    let (cert, key, ca) = certificates(certs.path());
    let other = tempfile::tempdir().unwrap();
    let (_, _, mut bundle) = certificates(other.path());
    bundle.extend(trust.then_some(ca).into_iter().flatten());
    let server = Server::new(fixture.root.path(), "", "", Some((&cert, &key)));
    let host = ["127.0.0.1", "localhost"][usize::from(hostname)];
    let url = server.url.replace("127.0.0.1", host);
    let config = girt::Config::parse(format!("[remote \"r\"]\nurl={url}\n[http]\nsslCAInfo=wrong-file\n[http \"{url}\"]\nsslCAInfo=selected.pem\n").as_bytes()).unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let selected = HttpSettings::configured_ca_info(&config, &destination)
        .unwrap()
        .unwrap();
    assert_eq!(selected, b"selected.pem");
    std::fs::write(certs.path().join("selected.pem"), &bundle).unwrap();
    let environment = HttpEnvironment {
        ssl_ca_info: Some(
            std::fs::read(certs.path().join(std::str::from_utf8(selected).unwrap())).unwrap(),
        ),
        ..Default::default()
    };
    let remote =
        HttpRemote::configured(HttpSettings::resolve(&config, &destination, &environment).unwrap())
            .unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime().block_on(fetch::receive_http(
        &remote,
        all,
        None,
        FetchLimits::default(),
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(15)),
        },
    ));
    assert_eq!(
        result
            .map(|_| ())
            .map_err(|error| matches!(error, FetchError::Http(HttpError::Network))),
        expected
    );
    assert_eq!(server.requests().len(), requests);
}

#[rstest]
#[case::empty(b"")]
#[case::garbage(b"private malformed trust bytes")]
#[case::truncated(b"-----BEGIN CERTIFICATE-----\nAAAA\n")]
#[case::invalid_der(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n")]
fn configured_https_rejects_invalid_ca_before_network(#[case] ca: &[u8]) {
    let config =
        girt::Config::parse(b"[remote \"r\"]\nurl=https://example.invalid/repo\n").unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let environment = HttpEnvironment {
        ssl_ca_info: Some(ca.to_vec()),
        ..Default::default()
    };
    let settings = HttpSettings::resolve(&config, &destination, &environment).unwrap();
    let error = HttpRemote::configured(settings).unwrap_err();
    assert!(matches!(error, HttpError::Configuration(_)));
    assert!(!format!("{error:?} {error}").contains("private"));
}

#[rstest]
#[case::proxy(None, true)]
#[case::bypass(Some("localhost"), false)]
#[case::empty_exclusions(Some(""), true)]
fn configured_https_routes_connect_and_honors_exclusions(
    #[case] no_proxy: Option<&str>,
    #[case] routed: bool,
) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let certs = tempfile::tempdir().unwrap();
    let (cert, key, ca) = certificates(certs.path());
    let server = Server::new(fixture.root.path(), "", "", Some((&cert, &key)));
    let url = server.url.replace("127.0.0.1", "localhost");
    let proxy = connect_proxy::Proxy::new(&url, "Basic cHU6cHA=");
    let config = girt::Config::parse(
        format!(
            "[remote \"r\"]\nurl={url}\nproxy={}\n[http]\nproxy=http://unreachable.invalid:1\n",
            proxy.url
        )
        .as_bytes(),
    )
    .unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let environment = HttpEnvironment {
        ssl_ca_info: Some(ca),
        proxy: Some("http://also-unreachable.invalid:1".into()),
        no_proxy: no_proxy.map(str::to_owned),
        proxy_credentials: Some(("pu".into(), "pp".into())),
        ..Default::default()
    };
    let settings =
        HttpSettings::resolve_for_remote(&config, b"r", &destination, &environment).unwrap();
    let remote = HttpRemote::configured(settings).unwrap();
    let cancel = AtomicBool::new(false);
    let result = runtime()
        .block_on(fetch::receive_http(
            &remote,
            all,
            None,
            FetchLimits::default(),
            TransportControl {
                cancel: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(15)),
            },
        ))
        .unwrap();
    assert!(
        !result
            .validate(&cancel, |_| ControlFlow::Continue(()))
            .unwrap()
            .wants()
            .is_empty()
    );
    assert_eq!(!proxy.requests().is_empty(), routed);
    assert_eq!(server.requests(), ["GET", "POST"]);
}

// Git's executable is the oracle; these scopes/configurations are original fixtures.
#[rstest]
#[case::global("https://example.test/repo", "[http]\nsslCAInfo=global\n", "global")]
#[case::longest(
    "https://example.test/repo/sub",
    "[http \"https://example.test/\"]\nsslCAInfo=root\n[http \"https://example.test/repo\"]\nsslCAInfo=path\n",
    "path"
)]
#[case::boundary(
    "https://example.test/repository",
    "[http]\nsslCAInfo=global\n[http \"https://example.test/repo\"]\nsslCAInfo=wrong\n",
    "global"
)]
#[case::default_port(
    "https://example.test/repo",
    "[http \"https://EXAMPLE.test:443\"]\nsslCAInfo=port\n",
    "port"
)]
#[case::escape(
    "https://example.test/repo/sub",
    "[http \"https://example.test/%72epo\"]\nsslCAInfo=escape\n",
    "escape"
)]
#[case::escaped_separator(
    "https://example.test/repo%2fsub",
    "[http]\nsslCAInfo=global\n[http \"https://example.test/repo/sub\"]\nsslCAInfo=wrong\n",
    "global"
)]
#[case::host_specificity(
    "https://example.test/repo/sub",
    "[http \"https://*.test/repo/sub\"]\nsslCAInfo=wild\n[http \"https://example.test\"]\nsslCAInfo=exact\n",
    "exact"
)]
#[case::wildcard(
    "https://a.example.test/repo",
    "[http \"https://*.example.test\"]\nsslCAInfo=wild\n",
    "wild"
)]
#[case::wildcard_depth(
    "https://a.b.example.test/repo",
    "[http]\nsslCAInfo=global\n[http \"https://*.example.test\"]\nsslCAInfo=wrong\n",
    "global"
)]
#[case::query_scope(
    "https://example.test/repo",
    "[http]\nsslCAInfo=global\n[http \"https://example.test/repo?x\"]\nsslCAInfo=wrong\n",
    "global"
)]
#[case::fragment_scope(
    "https://example.test/repo",
    "[http]\nsslCAInfo=global\n[http \"https://example.test/repo#x\"]\nsslCAInfo=wrong\n",
    "global"
)]
#[case::double_slash(
    "https://example.test/repo/x",
    "[http]\nsslCAInfo=global\n[http \"https://example.test/repo//\"]\nsslCAInfo=wrong\n",
    "global"
)]
#[case::single_slash(
    "https://example.test/repo",
    "[http \"https://example.test/repo/\"]\nsslCAInfo=slash\n",
    "slash"
)]
#[case::latest(
    "https://example.test/repo",
    "[http \"https://example.test\"]\nsslCAInfo=first\n[http \"https://example.test/\"]\nsslCAInfo=latest\n",
    "latest"
)]
fn ca_url_selection_agrees_with_git(
    #[case] url: &str,
    #[case] policy: &str,
    #[case] expected: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let config =
        girt::Config::parse(format!("[remote \"r\"]\nurl={url}\n{policy}").as_bytes()).unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let selected = HttpSettings::configured_ca_info(&config, &destination)
        .unwrap()
        .unwrap();
    assert_eq!(selected, expected.as_bytes());
    let observed = git(
        root.path(),
        &[
            "config",
            "--file",
            "-",
            "--get-urlmatch",
            "http.sslCAInfo",
            url,
        ],
        policy.as_bytes(),
    );
    assert_eq!(observed, [selected, b"\n"].concat());
}

#[test]
fn configured_https_and_git_use_explicit_ca_and_proxy_precedence() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let certs = tempfile::tempdir().unwrap();
    let (cert, key, ca) = certificates(certs.path());
    let server = Server::new(fixture.root.path(), "", "", Some((&cert, &key)));
    let url = server.url.replace("127.0.0.1", "localhost");
    let proxy = connect_proxy::Proxy::new(&url, "");
    let remote = configured_https(&url, Some(ca), &format!("[http]\nproxy={}\n", proxy.url));
    let cancel = AtomicBool::new(false);
    let advertisement = runtime()
        .block_on(fetch::discover_http(
            &remote,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap();
    let mut command = std::process::Command::new("git");
    // This oracle tests explicit PEM trust and proxy precedence, not Windows
    // default trust. Select Git for Windows' OpenSSL backend for the disposable
    // CA; certificate-chain and hostname verification remain enabled.
    #[cfg(windows)]
    command.args(["-c", "http.sslBackend=openssl"]);
    let output = command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env(
            "SYSTEMROOT",
            std::env::var_os("SYSTEMROOT").unwrap_or_default(),
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", certs.path().join("absent"))
        .env("https_proxy", "http://unreachable.invalid:1")
        .args([
            "-c",
            &format!("http.sslCAInfo={}", certs.path().join("ca.pem").display()),
            "-c",
            &format!("remote.r.url={url}"),
            "-c",
            &format!("remote.r.proxy={}", proxy.url),
            "-c",
            "http.proxy=http://also-unreachable.invalid:1",
            "ls-remote",
            "r",
        ])
        .current_dir(certs.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git fixture HTTPS request failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = tip(&fixture.repo, "refs/heads/main");
    assert!(
        advertisement
            .advertisement
            .refs
            .iter()
            .any(|reference| reference.id == expected)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(&expected.to_string())
    );
    assert_eq!(proxy.requests(), ["CONNECT", "CONNECT"]);
}

#[path = "support/push_response.rs"]
mod push_response;

fn progress_prepared(limits: PushLimits) -> PreparedPush {
    plain_prepared(limits).with_progress()
}

fn plain_prepared(limits: PushLimits) -> PreparedPush {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    PreparedPush::new(
        &source.repo.objects(PackLimits::default()).unwrap(),
        vec![command(
            "refs/heads/main",
            None,
            tip(&source.repo, "refs/heads/main"),
        )],
        limits,
        &AtomicBool::new(false),
    )
    .unwrap()
}

fn success_status() -> Vec<u8> {
    [
        push_response::packet(b"unpack ok\n"),
        push_response::packet(b"ok refs/heads/main\n"),
        b"0000".to_vec(),
    ]
    .concat()
}

#[test]
fn http_push_progress_arrives_before_response_completion() {
    let first = push_response::packet(b"\x02receiving\xff\r");
    let status = [b"\x01".as_slice(), &success_status()].concat();
    let rest = [
        push_response::packet(b"\x02done\n"),
        push_response::packet(&status),
        b"0000".to_vec(),
    ]
    .concat();
    let (url, acknowledge, server) = push_response::serve(true, first, rest, true);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut messages = Vec::new();
    let outcome = runtime()
        .block_on(push::send_http_checked_with_progress(
            &remote,
            progress_prepared(PushLimits::default()),
            TransportControl::new(&cancel),
            |_| true,
            |message| {
                messages.push(message.to_vec());
                let _ = acknowledge.send(());
            },
        ))
        .unwrap();
    server.join().unwrap();
    let HttpPushOutcome::Sent(report) = outcome else {
        panic!("push declined")
    };
    assert!(report.all_succeeded());
    assert_eq!(messages, [b"receiving\xff\r".to_vec(), b"done\n".to_vec()]);
    assert_eq!(report.progress, messages);
}

#[rstest]
#[case::not_supported(false, progress_prepared)]
#[case::not_requested(true, plain_prepared)]
fn http_push_progress_requires_sideband_negotiation(
    #[case] supported: bool,
    #[case] prepare: fn(PushLimits) -> PreparedPush,
) {
    let (url, _acknowledge, server) =
        push_response::serve(supported, success_status(), vec![], false);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let prepared = prepare(PushLimits::default());
    let cancel = AtomicBool::new(false);
    let outcome = runtime()
        .block_on(push::send_http_checked_with_progress(
            &remote,
            prepared,
            TransportControl::new(&cancel),
            |_| true,
            |_| panic!("unnegotiated progress"),
        ))
        .unwrap();
    server.join().unwrap();
    assert!(
        matches!(outcome, HttpPushOutcome::Sent(report) if report.all_succeeded() && report.progress.is_empty())
    );
}

#[rstest]
#[case::remote_error(b"0009\x03fail0006\x02z", PushFailure::Remote(b"fail".to_vec()))]
#[case::invalid_header(b"zzzz0006\x02z", PushFailure::Protocol("pkt-line header"))]
#[case::truncated(b"0009\x02z", PushFailure::Protocol("truncated pkt-line"))]
fn http_push_progress_retains_uncertain_prefix(#[case] rest: &[u8], #[case] expected: PushFailure) {
    let (url, acknowledge, server) = push_response::serve(
        true,
        push_response::packet(b"\x02first"),
        rest.to_vec(),
        true,
    );
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut messages = Vec::new();
    let error = runtime()
        .block_on(push::send_http_checked_with_progress(
            &remote,
            progress_prepared(PushLimits::default()),
            TransportControl::new(&cancel),
            |_| true,
            |message| {
                messages.push(message.to_vec());
                let _ = acknowledge.send(());
            },
        ))
        .unwrap_err();
    server.join().unwrap();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertainty")
    };
    assert_eq!(cause.to_string(), expected.to_string());
    assert_eq!(messages, [b"first".to_vec()]);
    assert_eq!(report.progress, messages);
    assert!(report.refs[0].attempted);
    assert!(report.refs[0].status.is_none());
}

#[test]
fn http_push_progress_callback_can_request_cancellation() {
    let (url, acknowledge, server) = push_response::serve(
        true,
        push_response::packet(b"\x02first"),
        b"0000".to_vec(),
        true,
    );
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let error = runtime()
        .block_on(push::send_http_checked_with_progress(
            &remote,
            progress_prepared(PushLimits::default()),
            TransportControl::new(&cancel),
            |_| true,
            |_| {
                cancel.store(true, Ordering::Relaxed);
                let _ = acknowledge.send(());
            },
        ))
        .unwrap_err();
    server.join().unwrap();
    assert!(
        matches!(error, PushError::Uncertain { cause: PushFailure::Cancelled, report } if report.progress == [b"first".to_vec()])
    );
}

#[test]
fn http_push_progress_excludes_bytes_beyond_status_budget() {
    let response = [
        push_response::packet(b"\x02first"),
        push_response::packet(b"\x02outside"),
        b"0000".to_vec(),
    ]
    .concat();
    let (url, _acknowledge, server) = push_response::serve(true, response, vec![], false);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let limits = PushLimits {
        max_status_bytes: 15,
        ..PushLimits::default()
    };
    let mut messages = Vec::new();
    let error = runtime()
        .block_on(push::send_http_checked_with_progress(
            &remote,
            progress_prepared(limits),
            TransportControl::new(&cancel),
            |_| true,
            |message| messages.push(message.to_vec()),
        ))
        .unwrap_err();
    server.join().unwrap();
    assert_eq!(messages, [b"first".to_vec()]);
    assert!(
        matches!(error, PushError::Uncertain { cause: PushFailure::Http(HttpError::Limit("response body")), report } if report.progress == messages)
    );
}

#[test]
fn http_push_progress_preserves_acknowledgements_before_failure() {
    let status = [b"\x01".as_slice(), &success_status()].concat();
    let rest = [push_response::packet(&status), b"zzzz".to_vec()].concat();
    let (url, acknowledge, server) =
        push_response::serve(true, push_response::packet(b"\x02first"), rest, true);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = progress_prepared(PushLimits::default());
    let transfer = push::send_http_checked_with_progress(
        &remote,
        prepared,
        TransportControl::new(&cancel),
        |_| true,
        |_| {
            let _ = acknowledge.send(());
        },
    );
    fn is_send<T: Send>(_: &T) {}
    is_send(&transfer);
    let error = runtime().block_on(transfer).unwrap_err();
    server.join().unwrap();
    let PushError::Uncertain { cause, report } = error else {
        panic!("expected uncertainty")
    };
    assert!(matches!(cause, PushFailure::Protocol("pkt-line header")));
    assert_eq!(report.progress, [b"first".to_vec()]);
    assert_eq!(report.refs[0].status, Some(Status::Ok));
}

fn fetch_progress_prefix() -> Vec<u8> {
    [
        push_response::packet(b"NAK\n"),
        push_response::packet(b"\x02receiving\xff\r"),
    ]
    .concat()
}

fn progress_fetch_request(destination: Repository) -> fetch::FetchRequest {
    let specs = girt::remote::Refspecs::parse(
        girt::remote::Direction::Fetch,
        ["refs/heads/*:refs/remotes/origin/*"],
    )
    .unwrap();
    fetch::FetchRequest::prepare(
        destination,
        specs,
        Default::default(),
        girt::refs::Reflog::Preserve,
    )
    .unwrap()
}

#[test]
fn http_fetch_progress_arrives_before_eof_and_validation_replays_notices() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let id = tip(&source.repo, "refs/heads/main");
    let pack = git(
        source.root.path(),
        &["pack-objects", "--stdout", "--revs"],
        format!("{id}\n").as_bytes(),
    );
    let rest = [
        push_response::packet(&[b"\x01".as_slice(), &pack].concat()),
        b"0000".to_vec(),
    ]
    .concat();
    let (url, acknowledge, server) =
        push_response::serve_fetch(id, fetch_progress_prefix(), rest, true);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let (_root, destination) = destination();
    let request = progress_fetch_request(destination);
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let transfer = request.receive_http_with_progress(
        &remote,
        None,
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |bytes| {
            notices.push(bytes.to_vec());
            let _ = acknowledge.send(());
        },
    );
    fn is_send<T: Send>(_: &T) {}
    is_send(&transfer);
    let download = runtime().block_on(transfer).unwrap();
    server.join().unwrap();
    assert_eq!(notices, [b"receiving\xff\r".to_vec()]);
    let mut replay = Vec::new();
    let mut validation = Vec::new();
    let _ready = download
        .validate_with_progress(
            &cancel,
            |bytes| {
                replay.push(bytes.to_vec());
                ControlFlow::Continue(())
            },
            |state| validation.push(state),
        )
        .unwrap();
    assert_eq!(replay, notices);
    let completed = validation.last().unwrap();
    assert!(completed.complete);
    assert!(completed.objects.0 > 0);
    assert_eq!(completed.objects.0, completed.objects.1);
    assert!(
        validation[..validation.len() - 1]
            .iter()
            .all(|state| !state.complete)
    );
}

#[rstest]
#[case::remote(
    b"0009\x03fail0006\x02z",
    FetchError::Remote(b"fail".to_vec())
)]
#[case::truncated(b"0009\x02z", FetchError::Protocol("truncated pkt-line"))]
fn http_fetch_progress_failure_never_publishes(#[case] rest: &[u8], #[case] expected: FetchError) {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let id = tip(&source.repo, "refs/heads/main");
    let (url, acknowledge, server) =
        push_response::serve_fetch(id, fetch_progress_prefix(), rest.to_vec(), true);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let (_root, destination) = destination();
    let git_dir = destination.git_dir().to_owned();
    let request = progress_fetch_request(destination);
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let download = runtime()
        .block_on(request.receive_http_with_progress(
            &remote,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
            |bytes| {
                notices.push(bytes.to_vec());
                let _ = acknowledge.send(());
            },
        ))
        .unwrap();
    server.join().unwrap();
    let error = download
        .validate(&cancel, |_| ControlFlow::Continue(()))
        .unwrap_err();
    assert_eq!(notices, [b"receiving\xff\r".to_vec()]);
    let fetch::FetchWorkflowError::Transfer(cause) = error else {
        panic!("expected transfer failure")
    };
    assert_eq!(cause.to_string(), expected.to_string());
    assert!(!git_dir.join("refs/remotes/origin/main").exists());
    assert_eq!(
        std::fs::read_dir(git_dir.join("objects/pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn http_fetch_progress_rejects_unoffered_ack_before_notices() {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let id = tip(&source.repo, "refs/heads/main");
    let response = [
        push_response::packet(format!("ACK {id}\n").as_bytes()),
        push_response::packet(b"\x02invalid"),
        b"0000".to_vec(),
    ]
    .concat();
    let (url, _acknowledge, server) = push_response::serve_fetch(id, response, vec![], false);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let download = runtime()
        .block_on(fetch::receive_http_with_progress(
            &remote,
            all,
            None,
            FetchLimits::default(),
            TransportControl::new(&cancel),
            |_| panic!("invalid negotiation must suppress notices"),
        ))
        .unwrap();
    server.join().unwrap();
    let error = download
        .validate(&cancel, |_| {
            panic!("invalid negotiation must suppress replay")
        })
        .unwrap_err();
    assert!(matches!(
        error,
        FetchError::Protocol("expected ACK of an offered have")
    ));
}

#[rstest]
#[case::cancelled(true, FetchError::Cancelled)]
#[case::budget(false, FetchError::Http(HttpError::Limit("response body")))]
fn http_fetch_progress_respects_cancellation_and_wire_budget(
    #[case] cancel_on_notice: bool,
    #[case] expected: FetchError,
) {
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let id = tip(&source.repo, "refs/heads/main");
    let rest = push_response::packet(&[b"\x02".as_slice(), &[b'x'; 1024]].concat());
    let (url, acknowledge, server) =
        push_response::serve_fetch(id, fetch_progress_prefix(), rest, true);
    let remote = HttpRemote::new(&url, &[], &[]).unwrap();
    let cancel = AtomicBool::new(false);
    let mut notices = Vec::new();
    let error = runtime()
        .block_on(fetch::receive_http_with_progress(
            &remote,
            all,
            None,
            FetchLimits {
                max_wire_bytes: 200,
                ..FetchLimits::default()
            },
            TransportControl::new(&cancel),
            |bytes| {
                notices.push(bytes.to_vec());
                cancel.store(cancel_on_notice, Ordering::Relaxed);
                let _ = acknowledge.send(());
            },
        ))
        .unwrap_err();
    server.join().unwrap();
    assert_eq!(notices, [b"receiving\xff\r".to_vec()]);
    assert_eq!(error.to_string(), expected.to_string());
}
