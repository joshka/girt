//! Disposable receive-pack interoperability. No real remotes, network or caller checkout writes.
#[path = "support/pack_git.rs"]
mod pack_git;

#[cfg(unix)]
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::push::{
    ForcePolicy, PreparedPush, PushCommand, PushError, PushFailure, PushLimits, PushReport, Status,
    send_local,
};
use girt::refs::RefName;
use girt::{
    EntryMode, ObjectId, ObjectKind, PackLimits, ReadLimits, Repository, Tag, TagFields, Tree,
    TreeEntry,
};
use pack_git::{Fixture, git};
use rstest::rstest;

fn destination(bare: bool) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let mut args = vec![
        "init",
        "--object-format=sha1",
        "--template=",
        "--initial-branch=main",
    ];
    if bare {
        args.push("--bare");
    }
    args.push(".");
    git(root.path(), &args, b"");
    let repo = Repository::open(root.path()).unwrap();
    (root, repo)
}
fn command(name: &str, expected: Option<ObjectId>, new: ObjectId) -> PushCommand {
    PushCommand {
        name: RefName::new(name).unwrap(),
        expected,
        new,
        force: ForcePolicy::FastForwardOnly,
    }
}
fn prepare(repo: &Repository, commands: Vec<PushCommand>) -> PreparedPush {
    let roots: Vec<_> = commands.iter().filter_map(|c| c.expected).collect();
    PreparedPush::new_excluding(
        &repo.objects(PackLimits::default()).unwrap(),
        commands,
        &roots,
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap()
}
fn push(
    source: &Repository,
    dest: &Repository,
    commands: Vec<PushCommand>,
) -> Result<PushReport, PushError> {
    send_local(
        dest.git_dir(),
        &prepare(source, commands),
        &AtomicBool::new(false),
    )
}
fn tip(repo: &Repository, name: &str) -> Option<ObjectId> {
    repo.references()
        .unwrap()
        .resolve(&RefName::new(name).unwrap(), 8)
        .unwrap()
        .id
}
fn main(f: &Fixture) -> ObjectId {
    tip(&f.repo, "refs/heads/main").unwrap()
}
fn next(f: &Fixture) -> ObjectId {
    let tree = git(f.root.path(), &["rev-parse", "main^{tree}"], b"");
    let output = git(
        f.root.path(),
        &[
            "commit-tree",
            std::str::from_utf8(&tree).unwrap().trim(),
            "-p",
            &main(f).to_string(),
        ],
        b"Next push fixture\n",
    );
    std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn verify(f: &Fixture, dest: &Repository) {
    assert!(f.index_path.exists());
    let objects = dest.objects(PackLimits::default()).unwrap();
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
    for (id, kind, data) in &f.records {
        let object = objects.read(*id, ReadLimits::default()).unwrap().unwrap();
        assert_eq!(object.kind(), *kind);
        assert_eq!(object.data(), data);
        assert_eq!(
            git(
                dest.git_dir(),
                &["cat-file", kind.as_str(), &id.to_string()],
                b""
            ),
            *data
        );
    }
    git(dest.git_dir(), &["fsck", "--strict", "--no-reflogs"], b"");
}
#[rstest]
#[case::ofs(true)]
#[case::ref_delta(false)]
fn publishes_branches_tags_and_complete_mixed_object_graph(#[case] ofs: bool) {
    let f = Fixture::new(ofs, 16);
    let (_root, dest) = destination(true);
    let tag = f.records.last().unwrap().0;
    let result = push(
        &f.repo,
        &dest,
        vec![
            command("refs/heads/main", None, main(&f)),
            command("refs/tags/packed", None, tag),
        ],
    )
    .unwrap();
    assert!(result.all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
    assert_eq!(tip(&dest, "refs/tags/packed"), Some(tag));
    assert_eq!(tip(&f.repo, "refs/remotes/origin/main"), None);
    verify(&f, &dest);
}
#[test]
fn advances_branch_and_repeats_without_implicit_local_ref_updates() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    let new = next(&f);
    assert!(
        push(&f.repo, &dest, vec![command("refs/heads/main", None, old)])
            .unwrap()
            .all_succeeded()
    );
    assert!(
        push(
            &f.repo,
            &dest,
            vec![command("refs/heads/main", Some(old), new)]
        )
        .unwrap()
        .all_succeeded()
    );
    assert!(
        push(
            &f.repo,
            &dest,
            vec![command("refs/heads/main", Some(new), new)]
        )
        .unwrap()
        .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(new));
    assert_eq!(main(&f), old);
    assert_eq!(tip(&f.repo, "refs/remotes/origin/main"), None);
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}
#[test]
fn stale_expected_value_rejects_entire_request_before_any_updates() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, old)]).unwrap();
    let result = push(
        &f.repo,
        &dest,
        vec![
            command("refs/heads/other", None, old),
            command("refs/heads/main", None, old),
        ],
    );
    assert!(matches!(
        result,
        Err(PushError::NotSent(PushFailure::Stale { .. }))
    ));
    assert_eq!(tip(&dest, "refs/heads/other"), None);
    assert_eq!(tip(&dest, "refs/heads/main"), Some(old));
}
#[test]
fn explicit_force_rewinds_when_server_allows_it() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    let new = next(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, new)]).unwrap();
    let mut rewind = command("refs/heads/main", Some(new), old);
    rewind.force = ForcePolicy::Allow;
    assert!(push(&f.repo, &dest, vec![rewind]).unwrap().all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), Some(old));
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}
#[test]
fn server_non_fast_forward_policy_overrides_explicit_force() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    let new = next(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, new)]).unwrap();
    git(
        dest.git_dir(),
        &["config", "receive.denyNonFastForwards", "true"],
        b"",
    );
    let mut rewind = command("refs/heads/main", Some(new), old);
    rewind.force = ForcePolicy::Allow;
    let result = push(&f.repo, &dest, vec![rewind]).unwrap();
    assert_eq!(
        result.refs[0].status,
        Some(Status::Rejected(b"non-fast-forward".to_vec()))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(new));
}
#[test]
fn checked_out_branch_is_rejected_by_server() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(false);
    let result = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap();
    assert_eq!(
        result.refs[0].status,
        Some(Status::Rejected(
            b"branch is currently checked out".to_vec()
        ))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}
#[cfg(unix)]
fn hook(repo: &Repository, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = repo.git_dir().join("hooks").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
#[cfg(unix)]
#[test]
fn update_hook_rejection_preserves_partial_success() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "update",
        "#!/bin/sh\n[ \"$1\" != refs/heads/reject ]\n",
    );
    let result = push(
        &f.repo,
        &dest,
        vec![
            command("refs/heads/main", None, main(&f)),
            command("refs/heads/reject", None, main(&f)),
        ],
    )
    .unwrap();
    assert!(!result.all_succeeded());
    assert_eq!(result.refs[0].status, Some(Status::Ok));
    assert_eq!(
        result.refs[1].status,
        Some(Status::Rejected(b"hook declined".to_vec()))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
    assert_eq!(tip(&dest, "refs/heads/reject"), None);
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}
#[cfg(unix)]
#[test]
fn pre_receive_hook_rejects_all_commands() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    hook(&dest, "pre-receive", "#!/bin/sh\ncat >/dev/null\nexit 1\n");
    let result = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap();
    assert_eq!(result.unpack, Some(Status::Ok));
    assert_eq!(
        result.refs[0].status,
        Some(Status::Rejected(b"pre-receive hook declined".to_vec()))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}
#[test]
fn publishes_nested_tags_trees_binary_blobs_symlinks_and_external_gitlinks() {
    let (_source_root, source) = destination(true);
    let (_root, dest) = destination(true);
    let objects = source.loose_objects().unwrap();
    let blob = objects.write_blob(b"binary\0\xff").unwrap();
    let link = objects.write_blob(b"target").unwrap();
    let external = ObjectId::for_blob(b"external missing submodule commit");
    let tree = objects
        .write_tree(
            &Tree::new(vec![
                TreeEntry {
                    name: b"data".to_vec(),
                    mode: EntryMode::Blob,
                    id: blob,
                },
                TreeEntry {
                    name: b"link".to_vec(),
                    mode: EntryMode::Symlink,
                    id: link,
                },
                TreeEntry {
                    name: b"submodule".to_vec(),
                    mode: EntryMode::Gitlink,
                    id: external,
                },
            ])
            .unwrap(),
        )
        .unwrap();
    let subtree = objects
        .write_tree(
            &Tree::new(vec![TreeEntry {
                name: b"dir".to_vec(),
                mode: EntryMode::Tree,
                id: tree,
            }])
            .unwrap(),
        )
        .unwrap();
    let tag = objects
        .write_tag(
            &Tag::new(TagFields {
                target: subtree,
                target_kind: ObjectKind::Tree,
                name: b"tree".to_vec(),
                tagger: None,
                extra_headers: vec![],
                message: vec![],
            })
            .unwrap(),
        )
        .unwrap();
    let nested = objects
        .write_tag(
            &Tag::new(TagFields {
                target: tag,
                target_kind: ObjectKind::Tag,
                name: b"nested".to_vec(),
                tagger: None,
                extra_headers: vec![],
                message: vec![],
            })
            .unwrap(),
        )
        .unwrap();
    let p = prepare(
        &source,
        vec![
            command("refs/tags/nested", None, nested),
            command("refs/tags/blob", None, blob),
        ],
    );
    assert_eq!(p.object_count(), 6);
    assert!(
        send_local(dest.git_dir(), &p, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(
        git(
            dest.git_dir(),
            &["cat-file", "blob", &blob.to_string()],
            b""
        ),
        b"binary\0\xff"
    );
    let reader = dest.objects(PackLimits::default()).unwrap();
    assert_eq!(reader.read(external, ReadLimits::default()).unwrap(), None);
    assert_eq!(
        reader
            .read(nested, ReadLimits::default())
            .unwrap()
            .unwrap()
            .kind(),
        ObjectKind::Tag
    );
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}
#[test]
fn empty_push_to_empty_repository_succeeds_without_pack() {
    let (_source_root, source) = destination(true);
    let (_root, dest) = destination(true);
    assert!(push(&source, &dest, vec![]).unwrap().all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}

#[cfg(unix)]
#[test]
fn remote_race_after_advertisement_is_reported_as_rejection() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let root = main(&f);
    let newer = next(&f);
    push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, newer)],
    )
    .unwrap();
    // The update hook runs after discovery and changes the ref before receive-pack's conditional
    // ref transaction. Both IDs already exist in the destination; no quarantine write is needed.
    hook(
        &dest,
        "update",
        &format!("#!/bin/sh\ngit update-ref \"$1\" {root}\n"),
    );
    let result = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", Some(newer), newer)],
    )
    .unwrap();
    assert_eq!(
        result.refs[0].status,
        Some(Status::Rejected(b"incorrect old value provided".to_vec()))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(root));
}

#[test]
fn tag_replacement_requires_explicit_force_but_creation_does_not() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let old = f.records.last().unwrap().0;
    let new = next(&f);
    push(
        &f.repo,
        &dest,
        vec![command("refs/tags/published", None, old)],
    )
    .unwrap();
    let c = command("refs/tags/published", Some(old), new);
    let objects = f.repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        PreparedPush::new(
            &objects,
            vec![c.clone()],
            PushLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(PushFailure::WouldForce(_))
    ));
    let forced = PushCommand {
        force: ForcePolicy::Allow,
        ..c
    };
    assert!(push(&f.repo, &dest, vec![forced]).unwrap().all_succeeded());
    assert_eq!(tip(&dest, "refs/tags/published"), Some(new));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[rstest]
#[case::before_commit("pre-receive", None)]
#[case::after_commit("post-receive", Some(true))]
fn deadline_interrupts_stalled_hook_with_uncertain_outcome(
    #[case] hook_name: &str,
    #[case] committed: Option<bool>,
) {
    use std::time::{Duration, Instant};

    use girt::push::send_local_with_control;
    use girt::transport::TransportControl;
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    // A finite fallback prevents a broken cancellation path from leaving a hanging hook.
    hook(
        &dest,
        hook_name,
        "#!/bin/sh\ncat >/dev/null\nprintf ready >hook-ready\nsleep 5\n",
    );
    let prepared = prepare(&f.repo, vec![command("refs/heads/main", None, main(&f))]);
    let cancel = AtomicBool::new(false);
    let result = send_local_with_control(
        dest.git_dir(),
        &prepared,
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(2)),
        },
    );
    let PushError::Uncertain { cause, .. } = result.unwrap_err() else {
        panic!("expected uncertain push")
    };
    assert!(matches!(cause, PushFailure::Deadline));
    assert!(dest.git_dir().join("hook-ready").exists());
    assert_eq!(tip(&dest, "refs/heads/main"), committed.map(|_| main(&f)));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn cancellation_interrupts_stalled_hook() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use girt::push::send_local_with_control;
    use girt::transport::TransportControl;
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "pre-receive",
        "#!/bin/sh\ncat >/dev/null\nprintf ready >hook-ready\nsleep 5\n",
    );
    let prepared = prepare(&f.repo, vec![command("refs/heads/main", None, main(&f))]);
    let cancel = AtomicBool::new(false);
    let marker = dest.git_dir().join("hook-ready");
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            let until = Instant::now() + Duration::from_secs(3);
            while !marker.exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel.store(true, Ordering::Relaxed);
        });
        send_local_with_control(
            dest.git_dir(),
            &prepared,
            TransportControl {
                cancel: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(4)),
            },
        )
    });
    let PushError::Uncertain { cause, report } = result.unwrap_err() else {
        panic!("expected uncertain push")
    };
    assert!(matches!(cause, PushFailure::Cancelled));
    assert!(marker.exists());
    assert_eq!(report.refs[0].status, None);
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[rstest]
#[case::cancelled(true)]
#[case::expired(false)]
fn interrupted_before_spawn_changes_no_destination_storage(#[case] cancelled: bool) {
    use std::time::Instant;

    use girt::push::send_local_with_control;
    use girt::transport::TransportControl;
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let prepared = prepare(&f.repo, vec![command("refs/heads/main", None, main(&f))]);
    let cancel = AtomicBool::new(cancelled);
    let result = send_local_with_control(
        dest.git_dir(),
        &prepared,
        TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now()),
        },
    );
    assert!(matches!(
        result,
        Err(PushError::NotSent(
            PushFailure::Cancelled | PushFailure::Deadline
        ))
    ));
    assert_eq!(tip(&dest, "refs/heads/main"), None);
    assert!(
        dest.objects(PackLimits::default())
            .unwrap()
            .read(main(&f), ReadLimits::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::read_dir(dest.git_dir().join("objects/pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn exclusion_reduces_incremental_and_noop_packs() {
    let f = Fixture::new(true, 16);
    let (_root, dest) = destination(true);
    let old = main(&f);
    assert!(
        push(&f.repo, &dest, vec![command("refs/heads/main", None, old)])
            .unwrap()
            .all_succeeded()
    );
    let new = next(&f);
    let commands = vec![command("refs/heads/main", Some(old), new)];
    let full = PreparedPush::new(
        &f.repo.objects(PackLimits::default()).unwrap(),
        commands.clone(),
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let reduced = prepare(&f.repo, commands);
    assert_eq!(reduced.object_count(), 1);
    assert!(reduced.pack_bytes() < full.pack_bytes());
    assert!(
        send_local(dest.git_dir(), &reduced, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    let noop = prepare(&f.repo, vec![command("refs/heads/main", Some(new), new)]);
    assert_eq!(noop.object_count(), 0);
    assert_eq!(noop.pack_bytes(), 32);
    assert!(
        send_local(dest.git_dir(), &noop, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    git(dest.git_dir(), &["fsck", "--strict"], b"");
    eprintln!(
        "push comparison incremental={}/{} full={}/{} noop={}/{}",
        reduced.object_count(),
        reduced.pack_bytes(),
        full.object_count(),
        full.pack_bytes(),
        noop.object_count(),
        noop.pack_bytes()
    );
}

#[test]
fn arbitrary_local_possession_is_not_receiver_knowledge() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let prepared = PreparedPush::new_excluding(
        &f.repo.objects(PackLimits::default()).unwrap(),
        vec![command("refs/heads/main", None, main(&f))],
        &[main(&f)],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(prepared.object_count(), 0);
    let result = send_local(dest.git_dir(), &prepared, &AtomicBool::new(false));
    assert!(matches!(
        result,
        Err(PushError::NotSent(PushFailure::KnowledgeChanged(_)))
    ));
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}

#[test]
fn receiver_root_missing_locally_preserves_full_forced_transfer() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let foreign = dest
        .loose_objects()
        .unwrap()
        .write_blob(b"foreign root")
        .unwrap();
    git(
        dest.git_dir(),
        &["update-ref", "refs/tags/foreign", &foreign.to_string()],
        b"",
    );
    let commands = vec![PushCommand {
        force: ForcePolicy::Allow,
        ..command("refs/tags/foreign", Some(foreign), main(&f))
    }];
    let prepared = PreparedPush::new_excluding(
        &f.repo.objects(PackLimits::default()).unwrap(),
        commands.clone(),
        &[foreign],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let full = PreparedPush::new(
        &f.repo.objects(PackLimits::default()).unwrap(),
        commands,
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(prepared.object_count(), full.object_count());
    assert_eq!(prepared.pack_bytes(), full.pack_bytes());
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    git(
        dest.git_dir(),
        &["fsck", "--strict", &main(&f).to_string()],
        b"",
    );
}

fn commit_with_parents(f: &Fixture, parents: &[ObjectId], message: &[u8]) -> ObjectId {
    let tree = String::from_utf8(git(f.root.path(), &["rev-parse", "main^{tree}"], b"")).unwrap();
    let mut args = vec!["commit-tree".to_owned(), tree.trim().to_owned()];
    for parent in parents {
        args.extend(["-p".to_owned(), parent.to_string()]);
    }
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    String::from_utf8(git(f.root.path(), &args, message))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn merge_push_excludes_receiver_second_parent_and_preserves_other_side() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let left = commit_with_parents(&f, &[main(&f)], b"left\n");
    let right = commit_with_parents(&f, &[main(&f)], b"right\n");
    let merge = commit_with_parents(&f, &[left, right], b"merge\n");
    assert!(
        push(
            &f.repo,
            &dest,
            vec![command("refs/heads/main", None, right)]
        )
        .unwrap()
        .all_succeeded()
    );
    let prepared = prepare(
        &f.repo,
        vec![command("refs/heads/main", Some(right), merge)],
    );
    assert_eq!(prepared.object_count(), 2);
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(merge));
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}

#[test]
fn divergent_receiver_tip_outside_selected_graph_falls_back_despite_local_possession() {
    let f = Fixture::new(true, 4);
    let (_root, dest) = destination(true);
    let left = commit_with_parents(&f, &[main(&f)], b"left\n");
    let right = commit_with_parents(&f, &[main(&f)], b"right\n");
    assert!(
        push(
            &f.repo,
            &dest,
            vec![command("refs/heads/main", None, right)]
        )
        .unwrap()
        .all_succeeded()
    );
    let update = PushCommand {
        force: ForcePolicy::Allow,
        ..command("refs/heads/main", Some(right), left)
    };
    let full = PreparedPush::new(
        &f.repo.objects(PackLimits::default()).unwrap(),
        vec![update.clone()],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let prepared = prepare(&f.repo, vec![update]);
    assert_eq!(prepared.object_count(), full.object_count());
    assert_eq!(prepared.pack_bytes(), full.pack_bytes());
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}

#[test]
fn delta_push_then_incremental_update_preserves_payloads() {
    let fixture = Fixture::new(false, 12);
    let (_root, dest) = destination(true);
    let cancel = AtomicBool::new(false);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let commands = vec![
        command("refs/heads/main", None, main(&fixture)),
        command("refs/tags/packed", None, fixture.records.last().unwrap().0),
    ];
    let ordinary =
        PreparedPush::new(&objects, commands.clone(), PushLimits::default(), &cancel).unwrap();
    let limits = PushLimits {
        compression: girt::PackCompression::Delta(girt::DeltaOptions::default()),
        ..PushLimits::default()
    };
    let prepared = PreparedPush::new(&objects, commands, limits, &cancel).unwrap();
    assert!(prepared.pack_bytes() < ordinary.pack_bytes());
    assert!(
        send_local(dest.git_dir(), &prepared, &cancel)
            .unwrap()
            .all_succeeded()
    );
    verify(&fixture, &dest);
    let new = next(&fixture);
    let mut first_payload = fixture.records[0].2.clone();
    first_payload[5000] ^= 1;
    let mut second_payload = first_payload.clone();
    second_payload[7000] ^= 1;
    let loose = fixture.repo.loose_objects().unwrap();
    let first = loose.write_blob(&first_payload).unwrap();
    let second = loose.write_blob(&second_payload).unwrap();
    let updates = vec![
        command("refs/heads/main", Some(main(&fixture)), new),
        command("refs/tags/new-a", None, first),
        command("refs/tags/new-b", None, second),
    ];
    let refreshed = fixture.repo.objects(PackLimits::default()).unwrap();
    let update = PreparedPush::new_excluding(
        &refreshed,
        updates.clone(),
        &[main(&fixture)],
        limits,
        &cancel,
    )
    .unwrap();
    let ordinary_update = PreparedPush::new_excluding(
        &refreshed,
        updates,
        &[main(&fixture)],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(update.object_count(), 3);
    assert!(update.pack_bytes() < ordinary_update.pack_bytes());
    assert!(
        send_local(dest.git_dir(), &update, &cancel)
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(new));
    assert_eq!(
        git(
            dest.git_dir(),
            &["cat-file", "blob", &first.to_string()],
            b""
        ),
        first_payload
    );
    assert_eq!(
        git(
            dest.git_dir(),
            &["cat-file", "blob", &second.to_string()],
            b""
        ),
        second_payload
    );
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}
