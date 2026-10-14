//! Original Git-generated packs; Git verify-pack is the independent counter oracle.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use girt::fetch::{
    FetchLimits, KnownHistory, ValidationProgress, receive_local_with_validation_progress,
};
use girt::push::{
    ForcePolicy, LocalPushContext, PreparedPush, PushCommand, PushLimits, send_local_with_progress,
};
use girt::refs::RefName;
use girt::transport::TransportControl;
use girt::{InitKind, ObjectFormat, PackLimits, Repository};
use pack_git::{Fixture, git};
use rstest::rstest;

fn check(events: &[ValidationProgress], objects: u64, deltas: u64) {
    assert!(deltas > 0);
    let last = events
        .last()
        .expect("validation publishes a final snapshot");
    assert_eq!(last.objects, (objects, objects));
    assert_eq!(last.deltas, Some((deltas, deltas)));
    assert!(last.complete);
    assert_eq!(events.iter().filter(|event| event.complete).count(), 1);
    for pair in events.windows(2) {
        assert!(pair[0].objects.0 <= pair[1].objects.0);
        assert!(
            pair[0].deltas.map_or(0, |value| value.0) <= pair[1].deltas.map_or(0, |value| value.0)
        );
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_fetch_reports_git_verified_entries_and_deltas(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let mut events = Vec::new();
    let received = receive_local_with_validation_progress(
        fixture.root.path(),
        |advertisement| {
            advertisement
                .refs
                .iter()
                .filter(|reference| !reference.peeled)
                .map(|reference| reference.id)
                .collect()
        },
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let destination =
        Repository::init(format, root.path().join("destination"), InitKind::Bare).unwrap();
    let installed = received
        .install(&destination, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let index = destination
        .object_dir()
        .join(format!("pack/pack-{}.idx", installed.checksum.unwrap()));
    let report = git(
        destination.git_dir(),
        &["verify-pack", "-v", index.to_str().unwrap()],
        b"",
    );
    let report = std::str::from_utf8(&report).unwrap();
    let deltas = report
        .lines()
        .filter(|line| line.split_whitespace().count() == 7)
        .count();
    check(&events, installed.objects as u64, deltas as u64);
    assert_eq!(installed.objects, fixture.records.len());
    let plain = girt::fetch::receive_local(
        fixture.root.path(),
        |advertisement| {
            advertisement
                .refs
                .iter()
                .filter(|reference| !reference.peeled)
                .map(|reference| reference.id)
                .collect()
        },
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let plain_root = tempfile::tempdir().unwrap();
    let plain_destination = Repository::init(
        format,
        plain_root.path().join("destination"),
        InitKind::Bare,
    )
    .unwrap();
    let plain_installed = plain
        .install(
            &plain_destination,
            PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(plain_installed.checksum, installed.checksum);
    assert_eq!(plain.object_count(), received.object_count());
    assert_eq!(
        std::fs::read(index.with_extension("pack")).unwrap(),
        std::fs::read(plain_destination.object_dir().join(format!(
            "pack/pack-{}.pack",
            plain_installed.checksum.unwrap()
        )))
        .unwrap()
    );
    assert!(fixture.index_path.exists());
    assert_ne!(fixture.ordinary, fixture.delta);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_push_validation_cancellation_never_publishes(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let id = std::str::from_utf8(&git(fixture.root.path(), &["rev-parse", "main"], b""))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let prepared = PreparedPush::new_local(
        &fixture.repo.objects(PackLimits::default()).unwrap(),
        vec![
            PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    for delta in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let destination =
            Repository::init(format, root.path().join("destination"), InitKind::Bare).unwrap();
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let inputs = girt::config::ConfigInputs::default();
        let result = send_local_with_progress(
            destination.git_dir(),
            &prepared,
            TransportControl::new(&cancel),
            LocalPushContext::new(&inputs).with_identity(None),
            |event| {
                events.push(event);
                if if delta {
                    event.deltas.is_some_and(|value| value.0 > 0)
                } else {
                    event.objects.0 > 0
                } {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(matches!(result, Err(girt::push::PushError::NotSent(_))));
        assert!(events.iter().all(|event| !event.complete));
        assert!(destination.references().unwrap().list().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(destination.object_dir().join("pack"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn git_local_transfer_progress_and_repository_callbacks() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 8);
    let root = tempfile::tempdir().unwrap();
    let destination = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("destination"),
        InitKind::Bare,
    )
    .unwrap();
    git(
        destination.git_dir(),
        &["config", "receive.unpackLimit", "0"],
        b"",
    );
    let output = pack_git::git_output(
        fixture.root.path(),
        &[
            "push",
            "--progress",
            destination.git_dir().to_str().unwrap(),
            "main",
        ],
        b"",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Resolving deltas:"), "{stderr}");
    let fetch_root = tempfile::tempdir().unwrap();
    let fetch_destination = Repository::init(
        ObjectFormat::Sha1,
        fetch_root.path().join("destination"),
        InitKind::Bare,
    )
    .unwrap();
    let output = pack_git::git_output(
        fetch_destination.git_dir(),
        &[
            "-c",
            "fetch.unpackLimit=1",
            "fetch",
            "--progress",
            fixture.root.path().to_str().unwrap(),
            "main",
        ],
        b"",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Receiving objects:") && stderr.contains("Resolving deltas:"),
        "{stderr}"
    );

    #[derive(Default)]
    struct Observer(Vec<ValidationProgress>);
    impl girt::transfer::TransferCallbacks for Observer {
        fn validation_progress(&mut self, progress: ValidationProgress) {
            self.0.push(progress);
        }
    }
    let repo_root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        repo_root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    git(
        repo.git_dir(),
        &[
            "remote",
            "add",
            "origin",
            fixture.root.path().to_str().unwrap(),
        ],
        b"",
    );
    let repo = Repository::open(repo.git_dir()).unwrap();
    let mut callbacks = Observer::default();
    repo.fetch(
        "origin",
        &girt::transfer::FetchOptions::new(["refs/heads/main:refs/remotes/origin/main"]),
        &girt::transfer::Environment::default(),
        &mut callbacks,
    )
    .unwrap();
    assert!(callbacks.0.last().unwrap().complete);
    callbacks.0.clear();
    repo.fetch(
        "origin",
        &girt::transfer::FetchOptions::new(["refs/heads/main:refs/remotes/origin/main"]),
        &girt::transfer::Environment::default(),
        &mut callbacks,
    )
    .unwrap();
    assert!(
        callbacks.0.is_empty(),
        "no-op fetch must not fabricate receiving work"
    );
    let push_root = tempfile::tempdir().unwrap();
    let push_destination = Repository::init(
        ObjectFormat::Sha1,
        push_root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    git(
        repo.git_dir(),
        &[
            "remote",
            "add",
            "push",
            push_destination.git_dir().to_str().unwrap(),
        ],
        b"",
    );
    let id = std::str::from_utf8(&git(fixture.root.path(), &["rev-parse", "main"], b""))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let repo = Repository::open(repo.git_dir()).unwrap();
    repo.push(
        "push",
        vec![
            PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &girt::transfer::PushOptions::default(),
        &girt::transfer::Environment::default(),
        &mut callbacks,
    )
    .unwrap();
    assert!(callbacks.0.last().unwrap().complete);
    let index = std::fs::read_dir(push_destination.object_dir().join("pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|value| value == "idx"))
        .unwrap();
    let report = git(
        push_destination.git_dir(),
        &["verify-pack", "-v", index.to_str().unwrap()],
        b"",
    );
    let report = std::str::from_utf8(&report).unwrap();
    let objects = report
        .lines()
        .filter(|line| matches!(line.split_whitespace().count(), 5 | 7))
        .count();
    let deltas = report
        .lines()
        .filter(|line| line.split_whitespace().count() == 7)
        .count();
    check(&callbacks.0, objects as u64, deltas as u64);
    git(push_destination.git_dir(), &["fsck", "--strict"], b"");
}

#[test]
fn fetch_validation_cancellation_and_panic_leave_source_unchanged() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 8);
    let before = std::fs::read(&fixture.index_path).unwrap();
    for delta in [false, true] {
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let result = receive_local_with_validation_progress(
            fixture.root.path(),
            |advertisement| {
                advertisement
                    .refs
                    .iter()
                    .map(|reference| reference.id)
                    .collect()
            },
            &KnownHistory::default(),
            FetchLimits::default(),
            TransportControl::new(&cancel),
            |_| ControlFlow::Continue(()),
            |event| {
                events.push(event);
                if if delta {
                    event.deltas.is_some_and(|value| value.0 > 0)
                } else {
                    event.objects.0 > 0
                } {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(matches!(result, Err(girt::fetch::FetchError::Cancelled)));
        assert!(events.iter().all(|event| !event.complete));
    }
    let result = std::panic::catch_unwind(|| {
        receive_local_with_validation_progress(
            fixture.root.path(),
            |advertisement| {
                advertisement
                    .refs
                    .iter()
                    .map(|reference| reference.id)
                    .collect()
            },
            &KnownHistory::default(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
            |_| panic!("observer panic"),
        )
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read(&fixture.index_path).unwrap(), before);
    git(fixture.root.path(), &["fsck", "--strict"], b"");
}

fn snapshot(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        result: &mut std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.insert(path.strip_prefix(root).unwrap().to_owned(), None);
                visit(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    Some(std::fs::read(&path).unwrap()),
                );
            }
        }
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn observer_panics_leave_push_and_repository_fetch_destinations_unchanged() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 8);
    let id = std::str::from_utf8(&git(fixture.root.path(), &["rev-parse", "main"], b""))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let command = PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
        .with_force(ForcePolicy::FastForwardOnly);
    let prepared = PreparedPush::new_local(
        &fixture.repo.objects(PackLimits::default()).unwrap(),
        vec![command.clone()],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    struct Panics;
    impl girt::transfer::TransferCallbacks for Panics {
        fn validation_progress(&mut self, progress: ValidationProgress) {
            if progress.objects.0 > 0 {
                panic!("observer panic");
            }
        }
    }
    for operation in ["push", "repository-fetch", "repository-push"] {
        let root = tempfile::tempdir().unwrap();
        let destination = Repository::init(
            ObjectFormat::Sha1,
            root.path().join("destination"),
            InitKind::Bare,
        )
        .unwrap();
        git(
            destination.git_dir(),
            &[
                "remote",
                "add",
                "origin",
                fixture.root.path().to_str().unwrap(),
            ],
            b"",
        );
        let destination = Repository::open(destination.git_dir()).unwrap();
        git(
            fixture.root.path(),
            &[
                "remote",
                "add",
                "panic-target",
                destination.git_dir().to_str().unwrap(),
            ],
            b"",
        );
        let source = Repository::open(fixture.root.path()).unwrap();
        let before = snapshot(destination.git_dir());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match operation {
            "push" => {
                let inputs = girt::config::ConfigInputs::default();
                let _ = send_local_with_progress(
                    destination.git_dir(),
                    &prepared,
                    TransportControl::new(&AtomicBool::new(false)),
                    LocalPushContext::new(&inputs).with_identity(None),
                    |event| {
                        if event.objects.0 > 0 {
                            panic!("observer panic");
                        }
                    },
                );
            }
            "repository-fetch" => {
                let _ = destination.fetch(
                    "origin",
                    &girt::transfer::FetchOptions::new([
                        "refs/heads/main:refs/remotes/origin/main",
                    ]),
                    &girt::transfer::Environment::default(),
                    &mut Panics,
                );
            }
            "repository-push" => {
                let _ = source.push(
                    "panic-target",
                    vec![command.clone()],
                    &girt::transfer::PushOptions::default(),
                    &girt::transfer::Environment::default(),
                    &mut Panics,
                );
            }
            _ => unreachable!(),
        }));
        assert!(result.is_err(), "{operation} must exercise observer unwind");
        assert_eq!(
            snapshot(destination.git_dir()),
            before,
            "{operation}: refs, primary objects and temporary directories unchanged"
        );
        git(
            fixture.root.path(),
            &["remote", "remove", "panic-target"],
            b"",
        );
    }
}
