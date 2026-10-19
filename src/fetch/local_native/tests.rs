use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rstest::rstest;

use super::*;
use crate::{EntryMode, InitKind, ObjectFormat, Tree, TreeEntry};

fn source(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("source"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"shared contents").unwrap();
    let tree = Tree::new(
        format,
        vec![
            TreeEntry {
                mode: EntryMode::Blob,
                name: b"a".to_vec(),
                id: blob,
            },
            TreeEntry {
                mode: EntryMode::Blob,
                name: b"b".to_vec(),
                id: blob,
            },
        ],
    )
    .unwrap();
    let id = loose.write_tree(&tree).unwrap();
    std::fs::write(repo.git_dir().join("refs/heads/main"), format!("{id}\n")).unwrap();
    (root, repo, id)
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn local_progress_counts_unique_reads_and_pack_entries(#[case] format: ObjectFormat) {
    let (_root, repo, id) = source(format);
    let mut events = Vec::new();
    let received = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(received.is_ok());
    assert_eq!(
        events,
        [
            LocalFetchProgress::Reading { objects: 0 },
            LocalFetchProgress::Reading { objects: 1 },
            LocalFetchProgress::Reading { objects: 2 },
            LocalFetchProgress::Packing { objects: (0, 2) },
            LocalFetchProgress::Packing { objects: (1, 2) },
            LocalFetchProgress::Packing { objects: (2, 2) },
            LocalFetchProgress::Complete,
        ]
    );
}

#[test]
fn local_progress_no_wants_has_no_pack() {
    let (_root, repo, _id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    receive(
        &repo,
        |_| vec![],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(
        events,
        [
            LocalFetchProgress::Reading { objects: 0 },
            LocalFetchProgress::Complete
        ]
    );
}

#[test]
fn local_progress_known_objects_are_not_read_or_packed() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let objects = repo.objects_with(Default::default()).unwrap();
    let known = KnownHistory::new(&objects, &[id], FetchLimits::default(), &cancel).unwrap();
    let mut events = Vec::new();
    receive(
        &repo,
        |_| vec![id],
        &known,
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(
        events,
        [
            LocalFetchProgress::Reading { objects: 0 },
            LocalFetchProgress::Complete
        ]
    );
}

#[rstest]
#[case::before_reads(LocalFetchProgress::Reading { objects: 0 })]
#[case::during_reads(LocalFetchProgress::Reading { objects: 1 })]
#[case::before_pack(LocalFetchProgress::Packing { objects: (0, 2) })]
#[case::during_pack(LocalFetchProgress::Packing { objects: (1, 2) })]
#[case::after_entries(LocalFetchProgress::Packing { objects: (2, 2) })]
fn local_progress_callback_can_cancel_before_completion(#[case] stop: LocalFetchProgress) {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |event| {
            events.push(event);
            if event == stop {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
    assert!(!events.contains(&LocalFetchProgress::Complete));
    assert_eq!(events.last(), Some(&stop));
}

#[test]
fn local_progress_terminal_callback_cannot_undo_completion() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |event| {
            if event == LocalFetchProgress::Complete {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    assert!(cancel.load(Ordering::Relaxed));
}

#[test]
fn local_progress_preserves_original_callback_cancellation() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Break(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
    assert!(!events.contains(&LocalFetchProgress::Complete));
}

#[test]
fn local_progress_expired_deadline_has_no_notifications() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl {
            cancel: &AtomicBool::new(false),
            deadline: Some(Instant::now() - Duration::from_secs(1)),
        },
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::Deadline)));
    assert!(events.is_empty());
}

#[test]
fn local_progress_pack_failure_has_no_completion() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits {
            max_pack_bytes: 12,
            ..Default::default()
        },
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::PackWrite(_))));
    assert!(!events.contains(&LocalFetchProgress::Complete));
}

#[test]
fn local_progress_missing_source_has_no_completion() {
    let (_root, repo, _id) = source(ObjectFormat::Sha1);
    let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    std::fs::write(
        repo.git_dir().join("refs/heads/main"),
        format!("{missing}\n"),
    )
    .unwrap();
    let mut events = Vec::new();
    let result = receive(
        &repo,
        |_| vec![missing],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(result, Err(FetchError::Missing(_))));
    assert!(!events.contains(&LocalFetchProgress::Complete));
}

#[test]
fn local_clone_pack_is_spooled_and_removed_on_drop() {
    let (_root, repo, id) = source(ObjectFormat::Sha1);
    let received = receive(
        &repo,
        |_| vec![id],
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |_| {},
    )
    .unwrap();
    let path = received
        .temporary_pack_path()
        .expect("local transfer must spool")
        .to_owned();
    assert_eq!(
        std::fs::metadata(&path).unwrap().len() as usize,
        received.pack_bytes()
    );
    drop(received);
    assert!(!path.exists());
}

/// A Git-written history of nested trees across many commits, so the walk reads ahead.
fn git_history(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q", &format!("--object-format={format}"), "."]);
    // A background `gc --auto` would repack and delete loose objects while the test reads them.
    git(&["config", "gc.auto", "0"]);
    git(&["config", "maintenance.auto", "false"]);
    for round in 0..12 {
        for file in 0..40 {
            let dir = path.join(format!("d{}/e{}", file % 4, file % 3));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("f{file}")),
                format!("{file} {}\n", round * (file % 5)),
            )
            .unwrap();
        }
        git(&["add", "-A"]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            &format!("r{round}"),
        ]);
    }
    let head = git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    let repo = Repository::open(path.join(".git")).unwrap();
    (root, repo, head)
}

/// Every worker count, including none, reads the same objects in the same order and writes the
/// same pack. A small decode allowance is still refused while workers read ahead.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn worker_counts_select_identical_packs_and_keep_limits(#[case] format: ObjectFormat) {
    let (_root, repo, head) = git_history(format);
    let run = |max_threads, max_decode_bytes| {
        let mut events = Vec::new();
        let received = receive(
            &repo,
            |_| vec![head],
            &KnownHistory::default(),
            FetchLimits {
                max_threads,
                max_decode_bytes,
                ..FetchLimits::default()
            },
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
            |event| events.push(event),
        );
        (received, events)
    };
    let (inline, inline_events) = run(1, FetchLimits::default().max_decode_bytes);
    let inline = inline.unwrap();
    let inline_pack = std::fs::read(inline.temporary_pack_path().unwrap()).unwrap();
    for threads in [0, 2, 8] {
        let (received, events) = run(threads, FetchLimits::default().max_decode_bytes);
        let received = received.unwrap();
        assert_eq!(events, inline_events, "{threads} threads");
        assert_eq!(
            std::fs::read(received.temporary_pack_path().unwrap()).unwrap(),
            inline_pack,
            "{threads} threads"
        );
    }
    for threads in [1, 8] {
        assert!(matches!(
            run(threads, 4096).0,
            Err(FetchError::Limit("local bytes"))
        ));
    }
}

/// Dependencies a walk to the tip of `commits` new commits records over a Git-written base whose
/// unchanged `keep` subtree holds `files` blobs. Only the base is known complete, so only the
/// oldest new commit has a complete parent.
fn chain_dependencies(format: ObjectFormat, files: usize, commits: usize) -> usize {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q", &format!("--object-format={format}"), "."]);
    git(&["config", "gc.auto", "0"]);
    git(&["config", "maintenance.auto", "false"]);
    for file in 0..files {
        let dir = path.join(format!("keep/d{}", file % 8));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("f{file}")), format!("keep {file}\n")).unwrap();
    }
    let commit = |message: &str| {
        std::fs::write(path.join("edit"), format!("{message}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["-c", "commit.gpgsign=false", "commit", "-qm", message]);
        git(&["rev-parse", "HEAD"])
            .trim()
            .parse::<ObjectId>()
            .unwrap()
    };
    let base = commit("base");
    for n in 0..commits {
        commit(&format!("edit {n}"));
    }
    let head = git(&["rev-parse", "HEAD"]).trim().parse().unwrap();
    let repo = Repository::open(path.join(".git")).unwrap();
    let cancel = AtomicBool::new(false);
    let known = KnownHistory::from_store_roots(
        repo.objects_with(crate::PackLimits::default()).unwrap(),
        &[base],
        &cancel,
    )
    .unwrap();
    let received = receive(
        &repo,
        |_| vec![head],
        &known,
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |_| {},
    )
    .unwrap();
    received.dependencies().len()
}

/// The oldest new commit's paired tree marks the unchanged subtree complete before any later
/// commit's tree is walked, so a chain records that subtree once instead of walking into it.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn commit_chain_does_not_walk_unchanged_subtrees(#[case] format: ObjectFormat) {
    let small = chain_dependencies(format, 16, 10);
    let large = chain_dependencies(format, 400, 10);
    assert_eq!(small, large, "{large} dependencies for 400 unchanged blobs");
}

#[derive(Clone, Copy, Debug)]
enum TransferSelection {
    Empty,
    Full,
    KnownOnly,
    StoreKnown,
}

#[rstest]
#[case::sha1_empty(ObjectFormat::Sha1, TransferSelection::Empty)]
#[case::sha1_full(ObjectFormat::Sha1, TransferSelection::Full)]
#[case::sha1_known(ObjectFormat::Sha1, TransferSelection::KnownOnly)]
#[case::sha256_empty(ObjectFormat::Sha256, TransferSelection::Empty)]
#[case::sha256_full(ObjectFormat::Sha256, TransferSelection::Full)]
#[case::sha256_known(ObjectFormat::Sha256, TransferSelection::KnownOnly)]
#[case::sha1_store(ObjectFormat::Sha1, TransferSelection::StoreKnown)]
#[case::sha256_store(ObjectFormat::Sha256, TransferSelection::StoreKnown)]
fn admitted_transfer_matches_legacy_bytes_and_notifications(
    #[case] format: ObjectFormat,
    #[case] selection: TransferSelection,
) {
    use std::io::Read;

    let (_root, repo, id) = source(format);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default();
    let known = if matches!(selection, TransferSelection::KnownOnly) {
        KnownHistory::new(
            &repo.objects_with(limits.source_packs).unwrap(),
            &[id],
            limits,
            &cancel,
        )
        .unwrap()
    } else if matches!(selection, TransferSelection::StoreKnown) {
        KnownHistory::from_store_roots_controlled(
            repo.objects_controlled(limits.source_packs, Default::default(), &cancel)
                .unwrap(),
            &[id],
            limits,
            crate::PeelLimits::default(),
            &cancel,
        )
        .unwrap()
    } else {
        KnownHistory::default()
    };
    let wants = if matches!(selection, TransferSelection::Empty) {
        Vec::new()
    } else {
        vec![id, id]
    };
    let mut legacy_messages = Vec::new();
    let mut legacy_construction = Vec::new();
    let mut legacy_validation = Vec::new();
    let legacy = receive_depth_observed(
        &repo,
        |_| wants.clone(),
        &known,
        None,
        false,
        limits,
        TransportControl::new(&cancel),
        |message| {
            legacy_messages.push(message.to_vec());
            ControlFlow::Continue(())
        },
        |event| legacy_construction.push(event),
        |event| legacy_validation.push(event),
    )
    .unwrap();
    let store = repo.objects_with(limits.source_packs).unwrap();
    let advertisement =
        advertise_store(&repo, &store, limits, TransportControl::new(&cancel)).unwrap();
    let mut messages = Vec::new();
    let mut construction = Vec::new();
    let mut validation = Vec::new();
    let admitted = receive_advertised_observed(
        &repo,
        store,
        advertisement,
        |_| wants.clone(),
        &known,
        None,
        false,
        limits,
        TransportControl::new(&cancel),
        |message| {
            messages.push(message.to_vec());
            ControlFlow::Continue(())
        },
        |event| construction.push(event),
        |event| validation.push(event),
    )
    .unwrap();
    let mut legacy_bytes = Vec::new();
    legacy
        .pack
        .reader()
        .unwrap()
        .read_to_end(&mut legacy_bytes)
        .unwrap();
    let mut admitted_bytes = Vec::new();
    admitted
        .pack
        .reader()
        .unwrap()
        .read_to_end(&mut admitted_bytes)
        .unwrap();
    assert_eq!(admitted_bytes, legacy_bytes);
    assert_eq!(admitted.index, legacy.index);
    assert_eq!(admitted.wants(), legacy.wants());
    assert_eq!(admitted.dependencies(), legacy.dependencies());
    assert_eq!(
        admitted.complete_dependencies(),
        legacy.complete_dependencies()
    );
    assert_eq!(admitted.shallow_roots(), legacy.shallow_roots());
    assert_eq!(admitted.object_count(), legacy.object_count());
    assert_eq!(messages, legacy_messages);
    assert_eq!(construction, legacy_construction);
    assert_eq!(validation, legacy_validation);

    let store = repo
        .objects_controlled(limits.source_packs, Default::default(), &cancel)
        .unwrap();
    let advertisement =
        advertise_store(&repo, &store, limits, TransportControl::new(&cancel)).unwrap();
    let mut controlled_messages = Vec::new();
    let mut controlled_construction = Vec::new();
    let mut controlled_validation = Vec::new();
    let controlled = receive_advertised_controlled(
        &repo,
        store,
        advertisement,
        |_| wants.clone(),
        &known,
        limits,
        TransportControl::new(&cancel),
        |message| {
            controlled_messages.push(message.to_vec());
            ControlFlow::Continue(())
        },
        |event| controlled_construction.push(event),
        |event| controlled_validation.push(event),
    )
    .unwrap();
    let mut controlled_bytes = Vec::new();
    controlled
        .pack
        .reader()
        .unwrap()
        .read_to_end(&mut controlled_bytes)
        .unwrap();
    assert_eq!(controlled_bytes, legacy_bytes);
    assert_eq!(controlled.index, legacy.index);
    assert_eq!(controlled.wants(), legacy.wants());
    assert_eq!(controlled.dependencies(), legacy.dependencies());
    assert_eq!(
        controlled.complete_dependencies(),
        legacy.complete_dependencies()
    );
    assert_eq!(controlled.object_count(), legacy.object_count());
    assert_eq!(controlled_messages, legacy_messages);
    assert_eq!(controlled_construction, legacy_construction);
    assert_eq!(controlled_validation, legacy_validation);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn controlled_native_receive_refuses_exhausted_known_reads(#[case] format: ObjectFormat) {
    let (_root, repo, id) = source(format);
    let cancel = AtomicBool::new(false);
    let limits = FetchLimits::default();
    let store = repo
        .objects_controlled(limits.source_packs, Default::default(), &cancel)
        .unwrap();
    let known = KnownHistory::from_store_roots_controlled(
        store.clone(),
        &[],
        limits,
        crate::PeelLimits::default(),
        &cancel,
    )
    .unwrap();
    let advertisement =
        advertise_store(&repo, &store, limits, TransportControl::new(&cancel)).unwrap();
    let result = receive_advertised_controlled(
        &repo,
        store,
        advertisement,
        |_| vec![id],
        &known,
        limits.with_max_known_objects(0),
        TransportControl::new(&cancel),
        |_| ControlFlow::Continue(()),
        |_| {},
        |_| {},
    );
    assert!(matches!(
        result,
        Err(FetchError::Limit("known read attempts"))
    ));
}
