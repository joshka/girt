//! Disposable receive-pack interoperability. No real remotes, network or caller checkout writes.
#[path = "support/pack_git.rs"]
mod pack_git;

#[cfg(unix)]
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::push::{
    ForcePolicy, PreparedPush, PushCommand, PushError, PushFailure, PushLimits, PushReport, Status,
};
use girt::refs::RefName;
use girt::{
    EntryMode, ObjectId, ObjectKind, PackLimits, ReadLimits, Repository, Tag, TagFields, Tree,
    TreeEntry,
};
use pack_git::{Fixture, git};
use rstest::rstest;

// Default-policy sends share setup so cases focus on receiver behavior.
fn send_local(
    destination: impl AsRef<std::path::Path>,
    prepared: &PreparedPush,
    cancel: &AtomicBool,
) -> Result<PushReport, PushError> {
    girt::push::send_local(
        destination,
        girt::push::LocalPushRequest::new(prepared),
        girt::transport::TransportControl::new(cancel),
        |_| {},
    )
}

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
    PushCommand::new(RefName::new(name).unwrap(), expected, new)
        .with_force(ForcePolicy::FastForwardOnly)
}
fn prepare(repo: &Repository, commands: Vec<PushCommand>) -> PreparedPush {
    let roots: Vec<_> = commands.iter().filter_map(|c| c.expected).collect();
    PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
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
    let objects = dest.objects_with(PackLimits::default()).unwrap();
    assert!(
        objects
            .read_with(f.ordinary, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert!(
        objects
            .read_with(f.delta, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    for (id, kind, data) in &f.records {
        let object = objects
            .read_with(*id, ReadLimits::default())
            .unwrap()
            .unwrap();
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
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn prepares_commit_with_unstored_canonical_empty_tree(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            &format!("--object-format={format}"),
            "--template=",
            ".",
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let empty_tree = repo.object_format().hash_object(ObjectKind::Tree, b"");
    // Git stores no object for the canonical empty tree, but reads it in every repository.
    let hex = empty_tree.to_string();
    assert!(
        !root
            .path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
            .exists()
    );
    assert!(
        repo.objects_with(PackLimits::default())
            .unwrap()
            .read_with(empty_tree, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    let content =
        format!("tree {empty_tree}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nempty\n");
    let commit = git(
        root.path(),
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        content.as_bytes(),
    );
    let commit = std::str::from_utf8(&commit)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let prepared = PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/main", None, commit)],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let (_dest_root, dest) = destination(true);
    if format == "sha1" {
        let report = send_local(dest.git_dir(), &prepared, &AtomicBool::new(false)).unwrap();
        assert!(report.all_succeeded());
    }
}
#[rstest]
#[case::ofs(true)]
#[case::ref_delta(false)]
fn publishes_branches_tags_and_complete_mixed_object_graph(#[case] ofs: bool) {
    let f = Fixture::new(girt::ObjectFormat::Sha1, ofs, 16);
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
fn stale_expected_value_rejects_only_its_ref() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, old)]).unwrap();
    let result = push(
        &f.repo,
        &dest,
        vec![
            command("refs/heads/other", None, old),
            command("refs/heads/main", None, next(&f)),
        ],
    );
    let report = result.unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert!(matches!(report.refs[1].status, Some(Status::Rejected(_))));
    assert_eq!(tip(&dest, "refs/heads/other"), Some(old));
    assert_eq!(tip(&dest, "refs/heads/main"), Some(old));
}
/// Observed with `git push --force-with-lease=refs/heads/main:`: a ref that already has the pushed
/// value is reported up to date, even though the lease expected it to be absent.
#[test]
fn unchanged_ref_is_up_to_date_despite_stale_expected_value() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, old)]).unwrap();
    let report = push(&f.repo, &dest, vec![command("refs/heads/main", None, old)]).unwrap();
    assert!(report.all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), Some(old));
}
#[test]
fn local_delete_policy_rejects_one_ref_without_stopping_another() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let id = main(&f);
    push(&f.repo, &dest, vec![command("refs/tags/old", None, id)]).unwrap();
    git(
        dest.git_dir(),
        &["config", "receive.denyDeletes", "true"],
        b"",
    );
    let report = push(
        &f.repo,
        &dest,
        vec![
            command(
                "refs/tags/old",
                Some(id),
                ObjectId::null(girt::ObjectFormat::Sha1),
            ),
            command("refs/tags/new", None, id),
        ],
    )
    .unwrap();
    assert!(matches!(report.refs[0].status, Some(Status::Rejected(_))));
    assert_eq!(report.refs[1].status, Some(Status::Ok));
    assert_eq!(tip(&dest, "refs/tags/old"), Some(id));
    assert_eq!(tip(&dest, "refs/tags/new"), Some(id));
}
#[test]
fn native_push_rejects_hidden_ref_without_publishing_it() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    git(
        dest.git_dir(),
        &["config", "receive.hideRefs", "refs/secret"],
        b"",
    );
    let id = main(&f);
    let result = push(&f.repo, &dest, vec![command("refs/secret/main", None, id)]);
    let report = result.unwrap();
    assert!(matches!(report.refs[0].status, Some(Status::Rejected(_))));
    assert_eq!(tip(&dest, "refs/secret/main"), None);
    assert!(
        dest.objects_with(PackLimits::default())
            .unwrap()
            .read_with(id, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    let git_result = std::process::Command::new("git")
        .current_dir(f.root.path())
        .args([
            "push",
            dest.git_dir().to_str().unwrap(),
            "refs/heads/main:refs/secret/main",
        ])
        .output()
        .unwrap();
    assert!(!git_result.status.success());
    assert_eq!(tip(&dest, "refs/secret/main"), None);
}
#[test]
fn explicit_force_rewinds_when_server_allows_it() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
fn forced_fast_forward_is_allowed_by_receive_policy() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let old = main(&f);
    let new = next(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, old)]).unwrap();
    git(
        dest.git_dir(),
        &["config", "receive.denyNonFastForwards", "true"],
        b"",
    );
    let mut advance = command("refs/heads/main", Some(old), new);
    advance.force = ForcePolicy::Allow;
    assert!(push(&f.repo, &dest, vec![advance]).unwrap().all_succeeded());
    assert_eq!(tip(&dest, "refs/heads/main"), Some(new));
}
#[test]
fn checked_out_branch_is_rejected_by_server() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
fn native_push_update_hook_rejects_only_its_ref() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
    );
    let report = result.unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(
        report.refs[1].status,
        Some(Status::Rejected(b"hook declined".to_vec()))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
    assert_eq!(tip(&dest, "refs/heads/reject"), None);
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}
#[cfg(unix)]
#[test]
fn native_push_pre_receive_hook_rejects_every_ref() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(&dest, "pre-receive", "#!/bin/sh\ncat >/dev/null\nexit 1\n");
    let result = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    );
    assert_eq!(
        result.unwrap().refs[0].status,
        Some(Status::Rejected(b"pre-receive hook declined".to_vec()))
    );
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}
#[test]
fn publishes_nested_tags_trees_binary_blobs_symlinks_and_external_gitlinks() {
    let (_source_root, source) = destination(true);
    let (_root, dest) = destination(true);
    let objects = source.loose_objects();
    let blob = objects.write_blob(b"binary\0\xff").unwrap();
    let link = objects.write_blob(b"target").unwrap();
    let external = ObjectId::for_blob(
        girt::ObjectFormat::Sha1,
        b"external missing submodule commit",
    );
    let tree = objects
        .write_tree(
            &Tree::new(
                girt::ObjectFormat::Sha1,
                vec![
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
                ],
            )
            .unwrap(),
        )
        .unwrap();
    let subtree = objects
        .write_tree(
            &Tree::new(
                girt::ObjectFormat::Sha1,
                vec![TreeEntry {
                    name: b"dir".to_vec(),
                    mode: EntryMode::Tree,
                    id: tree,
                }],
            )
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
    let reader = dest.objects_with(PackLimits::default()).unwrap();
    assert_eq!(
        reader.read_with(external, ReadLimits::default()).unwrap(),
        None
    );
    assert_eq!(
        reader
            .read_with(nested, ReadLimits::default())
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
fn native_push_update_hook_racing_the_ref_fails_its_lease() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let root = main(&f);
    let newer = next(&f);
    push(&f.repo, &dest, vec![command("refs/heads/main", None, root)]).unwrap();
    // The update hook runs after the lease was checked and deletes the ref before the conditional
    // reference transaction publishes the push.
    hook(&dest, "update", "#!/bin/sh\ngit update-ref -d \"$1\"\n");
    let result = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", Some(root), newer)],
    );
    assert!(!matches!(
        result.as_ref().map(|report| &report.refs[0].status),
        Ok(Some(Status::Ok))
    ));
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}

#[test]
fn tag_replacement_requires_explicit_force_but_creation_does_not() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
    let objects = f.repo.objects_with(PackLimits::default()).unwrap();
    assert!(matches!(
        PreparedPush::new(
            &objects,
            vec![c.clone()],
            &[],
            PushLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(PushFailure::WouldForce(_))
    ));
    let forced = (c).with_force(ForcePolicy::Allow);
    assert!(push(&f.repo, &dest, vec![forced]).unwrap().all_succeeded());
    assert_eq!(tip(&dest, "refs/tags/published"), Some(new));
}

#[cfg(unix)]
#[rstest]
#[case::pre("pre-receive")]
#[case::post("post-receive")]
fn native_push_passes_updates_to_receive_hooks(#[case] hook_name: &str) {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(&dest, hook_name, "#!/bin/sh\ncat >hook-input\n");
    let report = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap();
    assert!(report.all_succeeded());
    let null = ObjectId::null(girt::ObjectFormat::Sha1);
    assert_eq!(
        fs::read(dest.git_dir().join("hook-input")).unwrap(),
        format!("{null} {} refs/heads/main\n", main(&f)).into_bytes()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[rstest]
#[case::cancelled(true)]
#[case::expired(false)]
fn interrupted_before_spawn_changes_no_destination_storage(#[case] cancelled: bool) {
    use std::time::Instant;

    use girt::transport::TransportControl;
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let prepared = prepare(&f.repo, vec![command("refs/heads/main", None, main(&f))]);
    let cancel = AtomicBool::new(cancelled);
    let result = girt::push::send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        TransportControl::new(&cancel).with_deadline(Some(Instant::now())),
        |_| {},
    );
    assert!(matches!(
        result,
        Err(PushError::NotSent(
            PushFailure::Cancelled | PushFailure::Deadline
        ))
    ));
    assert_eq!(tip(&dest, "refs/heads/main"), None);
    assert!(
        dest.objects_with(PackLimits::default())
            .unwrap()
            .read_with(main(&f), ReadLimits::default())
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 16);
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
        &f.repo.objects_with(PackLimits::default()).unwrap(),
        commands.clone(),
        &[],
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let prepared = PreparedPush::new(
        &f.repo.objects_with(PackLimits::default()).unwrap(),
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    let foreign = dest.loose_objects().write_blob(b"foreign root").unwrap();
    git(
        dest.git_dir(),
        &["update-ref", "refs/tags/foreign", &foreign.to_string()],
        b"",
    );
    let commands = vec![
        (command("refs/tags/foreign", Some(foreign), main(&f))).with_force(ForcePolicy::Allow),
    ];
    let prepared = PreparedPush::new(
        &f.repo.objects_with(PackLimits::default()).unwrap(),
        commands.clone(),
        &[foreign],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let full = PreparedPush::new(
        &f.repo.objects_with(PackLimits::default()).unwrap(),
        commands,
        &[],
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
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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

/// A rewritten receiver tip still proves its ancestry, so only the divergent commit is sent.
#[test]
fn divergent_receiver_tip_excludes_shared_history() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
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
    let update = (command("refs/heads/main", Some(right), left)).with_force(ForcePolicy::Allow);
    let prepared = prepare(&f.repo, vec![update]);
    assert_eq!(prepared.object_count(), 1);
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(left));
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}

/// Imports commits described by space-separated `parents@seconds` entries, such as `@100 0@200
/// 1,0@300`, with Git's fast-import. Parents are comma-separated indexes of earlier entries.
/// Commit `i` is `refs/heads/c<i>` and adds file `f<i>`, so each has a new tree and blob.
fn import(spec: &str) -> (tempfile::TempDir, Repository, Vec<ObjectId>) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--quiet",
            "--object-format=sha1",
            "--template=",
            ".",
        ],
        b"",
    );
    let mut stream = String::new();
    let entries: Vec<_> = spec.split_whitespace().collect();
    for (index, entry) in entries.iter().enumerate() {
        let (parents, seconds) = entry.split_once('@').unwrap();
        let message = format!("c{index}\n");
        let content = format!("content {index}\n");
        stream += &format!(
            "commit refs/heads/c{index}\nmark :{}\ncommitter C <c@example.com> {seconds} +0000\n\
             data {}\n{message}",
            index + 1,
            message.len()
        );
        for (position, parent) in parents.split(',').filter(|p| !p.is_empty()).enumerate() {
            let parent: usize = parent.parse().unwrap();
            let verb = ["from", "merge"][usize::from(position > 0)];
            stream += &format!("{verb} :{}\n", parent + 1);
        }
        stream += &format!(
            "M 100644 inline f{index}\ndata {}\n{content}\n",
            content.len()
        );
    }
    git(root.path(), &["fast-import", "--quiet"], stream.as_bytes());
    let names: Vec<_> = (0..entries.len()).map(|i| format!("c{i}")).collect();
    let mut args = vec!["rev-parse"];
    args.extend(names.iter().map(String::as_str));
    let ids = String::from_utf8(git(root.path(), &args, b""))
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, ids)
}

/// A receiver holding only `known`'s history, pushed there by Git.
fn receiver_with(source: &tempfile::TempDir, known: ObjectId) -> (tempfile::TempDir, Repository) {
    let (root, dest) = destination(true);
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            dest.git_dir().to_str().unwrap(),
            &format!("{known}:refs/heads/main"),
        ],
        b"",
    );
    (root, dest)
}

/// With skewed committer dates, a fast-forward over the receiver's only tip sends exactly the new
/// commits, and Git accepts the result as complete.
#[rstest]
#[case::new_commit_older_than_parent("@100 0@200 1@300 2@50", 2, 3, 1)]
#[case::known_parent_newer_than_child("@500 0@100 1@600 2@700 3@50 4@800", 3, 5, 2)]
#[case::merge_with_new_side_older_than_base("@500 0@600 0@10 1,2@700", 1, 3, 2)]
#[case::new_side_forks_deep_with_old_dates("@100 0@200 1@300 2@400 0@50 3,4@500", 3, 5, 2)]
#[case::known_second_parent_skewed("@100 0@900 0@200 2,1@300", 1, 3, 2)]
fn skewed_fast_forward_sends_complete_new_history(
    #[case] spec: &str,
    #[case] known: usize,
    #[case] new: usize,
    #[case] new_commits: u32,
) {
    let (source, repo, ids) = import(spec);
    let (_root, dest) = receiver_with(&source, ids[known]);
    let prepared = prepare(
        &repo,
        vec![command("refs/heads/main", Some(ids[known]), ids[new])],
    );
    assert_eq!(prepared.object_count(), 3 * new_commits);
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/main"), Some(ids[new]));
    git(dest.git_dir(), &["fsck", "--strict", "--no-reflogs"], b"");
}

/// Annotated tags of new and known commits send only the tags and new history.
#[test]
fn tags_over_known_history_send_only_new_objects() {
    let (source, repo, ids) = import("@500 0@100 1@600 2@50");
    let (_root, dest) = receiver_with(&source, ids[2]);
    git(
        source.path(),
        &["tag", "-a", "-m", "new\n", "new", "c3"],
        b"",
    );
    git(
        source.path(),
        &["tag", "-a", "-m", "old\n", "old", "c1"],
        b"",
    );
    git(
        source.path(),
        &["tag", "-a", "-m", "nested\n", "nested", "old"],
        b"",
    );
    let commands = vec![
        command("refs/heads/main", Some(ids[2]), ids[3]),
        command("refs/tags/new", None, tip(&repo, "refs/tags/new").unwrap()),
        command(
            "refs/tags/nested",
            None,
            tip(&repo, "refs/tags/nested").unwrap(),
        ),
    ];
    let prepared = prepare(&repo, commands);
    assert_eq!(prepared.object_count(), 3 + 3);
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    git(dest.git_dir(), &["fsck", "--strict", "--no-reflogs"], b"");
}

#[test]
fn delta_push_then_incremental_update_preserves_payloads() {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, false, 12);
    let (_root, dest) = destination(true);
    let cancel = AtomicBool::new(false);
    let objects = fixture.repo.objects_with(PackLimits::default()).unwrap();
    let commands = vec![
        command("refs/heads/main", None, main(&fixture)),
        command("refs/tags/packed", None, fixture.records.last().unwrap().0),
    ];
    let ordinary_limits = PushLimits::default().with_compression(girt::PackCompression::Ordinary);
    let ordinary =
        PreparedPush::new(&objects, commands.clone(), &[], ordinary_limits, &cancel).unwrap();
    let limits = PushLimits::default()
        .with_compression(girt::PackCompression::Delta(girt::DeltaOptions::default()));
    let prepared = PreparedPush::new(&objects, commands, &[], limits, &cancel).unwrap();
    // Git has already deltified every packed input; both policies preserve those streams.
    // The incremental loose-object transfer below still exercises fresh delta search.
    assert_eq!(prepared.pack_bytes(), ordinary.pack_bytes());
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
    let loose = fixture.repo.loose_objects();
    let first = loose.write_blob(&first_payload).unwrap();
    let second = loose.write_blob(&second_payload).unwrap();
    let updates = vec![
        command("refs/heads/main", Some(main(&fixture)), new),
        command("refs/tags/new-a", None, first),
        command("refs/tags/new-b", None, second),
    ];
    let refreshed = fixture.repo.objects_with(PackLimits::default()).unwrap();
    let update = PreparedPush::new(
        &refreshed,
        updates.clone(),
        &[main(&fixture)],
        limits,
        &cancel,
    )
    .unwrap();
    let ordinary_update = PreparedPush::new(
        &refreshed,
        updates,
        &[main(&fixture)],
        ordinary_limits,
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

/// Git trusts referenced history, including non-tip ancestors, rather than fscking old trees.
#[test]
fn non_tip_receiver_boundary_does_not_scan_old_trees() {
    let (source, repo, ids) = import("@100 0@200 1@300 1@400");
    let (_root, dest) = receiver_with(&source, ids[2]);
    let old_tree = String::from_utf8(git(source.path(), &["rev-parse", "c0^{tree}"], b"")).unwrap();
    let old_tree = old_tree.trim();
    std::fs::remove_file(dest.object_dir().join(&old_tree[..2]).join(&old_tree[2..])).unwrap();
    // The new commit forks from c1, which is not an advertised tip.
    let prepared = PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/new", None, ids[3])],
        &[ids[2]],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/new"), Some(ids[3]));
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            dest.git_dir().to_str().unwrap(),
            "c3:refs/heads/git-new",
        ],
        b"",
    );
}

#[cfg(unix)]
#[test]
fn pre_receive_rejection_discards_quarantined_objects() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "pre-receive",
        &format!(
            "#!/bin/sh\ntest -n \"$GIT_QUARANTINE_PATH\" || exit 9\ntest \"$GIT_OBJECT_DIRECTORY\" = \"$GIT_QUARANTINE_PATH\" || exit 9\ngit cat-file -e {} || exit 9\nprintf quarantine >hook-observed\nexit 1\n",
            main(&f)
        ),
    );
    let report = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap();
    assert_eq!(
        report.refs[0].status,
        Some(Status::Rejected(b"pre-receive hook declined".to_vec()))
    );
    assert_eq!(
        fs::read(dest.git_dir().join("hook-observed")).unwrap(),
        b"quarantine"
    );
    assert_eq!(tip(&dest, "refs/heads/main"), None);
    assert!(
        dest.objects_with(PackLimits::default())
            .unwrap()
            .read_with(main(&f), ReadLimits::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_dir(dest.object_dir()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn update_hook_observes_published_objects_without_quarantine() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "pre-receive",
        "#!/bin/sh\ntest -n \"$GIT_QUARANTINE_PATH\" || exit 1\nprintf '%s' \"$GIT_QUARANTINE_PATH\" >quarantine-path\ncat >/dev/null\n",
    );
    hook(
        &dest,
        "update",
        "#!/bin/sh\ntest -z \"$GIT_QUARANTINE_PATH\" || exit 1\ntest -z \"$GIT_OBJECT_DIRECTORY\" || exit 1\ntest ! -e \"$(cat quarantine-path)\" || exit 1\ngit cat-file -e \"$3\" || exit 1\nprintf primary >hook-observed\nexit 1\n",
    );
    let report = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap();
    assert_eq!(
        report.refs[0].status,
        Some(Status::Rejected(b"hook declined".to_vec()))
    );
    assert_eq!(
        fs::read(dest.git_dir().join("hook-observed")).unwrap(),
        b"primary"
    );
    assert_eq!(tip(&dest, "refs/heads/main"), None);
    assert!(
        dest.objects_with(PackLimits::default())
            .unwrap()
            .read_with(main(&f), ReadLimits::default())
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn cancellation_during_pre_receive_leaves_primary_storage_unchanged() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    // A finite hook keeps the reproducing test bounded even before hook cancellation exists.
    hook(
        &dest,
        "pre-receive",
        "#!/bin/sh\nprintf started >hook-starting\nmv hook-starting hook-observed\nsleep 1\ncat >/dev/null\n",
    );
    let prepared = prepare(&f.repo, vec![command("refs/heads/main", None, main(&f))]);
    let cancel = AtomicBool::new(false);
    let observed = dest.git_dir().join("hook-observed");
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !observed.exists() {
                assert!(std::time::Instant::now() < end, "hook did not start");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        girt::push::send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&prepared),
            girt::transport::TransportControl::new(&cancel),
            |_| {},
        )
        .unwrap_err()
    });
    assert!(matches!(error, PushError::NotSent(PushFailure::Cancelled)));
    assert_eq!(
        fs::read(dest.git_dir().join("hook-observed")).unwrap(),
        b"started"
    );
    assert_eq!(tip(&dest, "refs/heads/main"), None);
    assert!(
        dest.objects_with(PackLimits::default())
            .unwrap()
            .read_with(main(&f), ReadLimits::default())
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn hook_output_limit_truncates_without_aborting_publication() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "pre-receive",
        "#!/bin/sh\nprintf 01234567890123456789\nprintf 01234567890123456789 >&2\ncat >/dev/null\n",
    );
    let prepared = PreparedPush::new(
        &f.repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/main", None, main(&f))],
        &[],
        PushLimits::default().with_max_status_bytes(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = send_local(dest.git_dir(), &prepared, &AtomicBool::new(false)).unwrap();
    assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
    assert_eq!(report.progress.iter().map(Vec::len).sum::<usize>(), 30);
}

#[cfg(unix)]
#[test]
fn pre_receive_creating_objects_is_explicitly_unsupported() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "pre-receive",
        "#!/bin/sh\nprintf hook-created | git hash-object -w --stdin >/dev/null\ncat >/dev/null\n",
    );
    let error = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PushError::NotSent(PushFailure::Unsupported(
            "pre-receive modified quarantined object storage"
        ))
    ));
    assert_eq!(tip(&dest, "refs/heads/main"), None);
    assert!(
        dest.objects_with(PackLimits::default())
            .unwrap()
            .read_with(main(&f), ReadLimits::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_dir(dest.object_dir()).unwrap().count(), 2);
}

#[cfg(unix)]
#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn accepted_quarantine_can_read_primary_alternates_with_special_path_bytes(
    #[case] format: girt::ObjectFormat,
) {
    let f = Fixture::new(format, true, 4);
    let root = tempfile::Builder::new()
        .prefix("girt:quote\"\n")
        .tempdir()
        .unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let dest = Repository::open(root.path()).unwrap();
    let old = main(&f);
    let seed = PreparedPush::new(
        &f.repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/main", None, old)],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    send_local(dest.git_dir(), &seed, &AtomicBool::new(false)).unwrap();
    let new = next(&f);
    hook(
        &dest,
        "pre-receive",
        &format!(
            "#!/bin/sh\ngit cat-file -e {old} || exit 1\ngit cat-file -e {new} || exit 1\ncat >/dev/null\n"
        ),
    );
    let prepared = PreparedPush::new(
        &f.repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/main", Some(old), new)],
        &[old],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = send_local(dest.git_dir(), &prepared, &AtomicBool::new(false)).unwrap();
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(tip(&dest, "refs/heads/main"), Some(new));
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}

#[cfg(unix)]
#[test]
fn cancellation_in_post_receive_preserves_published_ref_evidence() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "post-receive",
        "#!/bin/sh\nprintf started >hook-starting\nmv hook-starting hook-observed\nsleep 1\ncat >/dev/null\n",
    );
    let prepared = prepare(&f.repo, vec![command("refs/heads/main", None, main(&f))]);
    let cancel = AtomicBool::new(false);
    let observed = dest.git_dir().join("hook-observed");
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !observed.exists() {
                assert!(std::time::Instant::now() < end, "hook did not start");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        send_local(dest.git_dir(), &prepared, &cancel).unwrap_err()
    });
    let PushError::Uncertain { cause, report } = error else {
        panic!("missing published outcome")
    };
    assert!(matches!(cause, PushFailure::Cancelled));
    assert_eq!(report.refs[0].status, Some(Status::Ok));
    assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
    git(dest.git_dir(), &["fsck", "--strict"], b"");
}

#[cfg(unix)]
#[test]
fn replacement_quarantine_is_preserved_and_cleanup_failure_is_structured() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let (_root, dest) = destination(true);
    hook(
        &dest,
        "pre-receive",
        "#!/bin/sh\nprintf '%s' \"$GIT_QUARANTINE_PATH\" >quarantine-path\nmv \"$GIT_QUARANTINE_PATH\" \"$GIT_DIR/saved-quarantine\"\nmkdir \"$GIT_QUARANTINE_PATH\"\nprintf replacement >\"$GIT_QUARANTINE_PATH/marker\"\ncat >/dev/null\n",
    );
    let error = push(
        &f.repo,
        &dest,
        vec![command("refs/heads/main", None, main(&f))],
    )
    .unwrap_err();
    let PushError::Uncertain {
        cause: PushFailure::QuarantineCleanup { failure, .. },
        ..
    } = error
    else {
        panic!("missing quarantine cleanup failure")
    };
    assert!(matches!(
        failure.as_deref(),
        Some(PushError::NotSent(PushFailure::Io(_)))
    ));
    let path = fs::read_to_string(dest.git_dir().join("quarantine-path")).unwrap();
    assert_eq!(
        fs::read(std::path::Path::new(&path).join("marker")).unwrap(),
        b"replacement"
    );
    assert!(dest.git_dir().join("saved-quarantine/pack").is_dir());
    assert_eq!(tip(&dest, "refs/heads/main"), None);
}

#[cfg(unix)]
#[test]
fn post_hooks_drain_excess_output_and_preserve_background_work_like_git() {
    let f = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    for native in [false, true] {
        let (_root, dest) = destination(true);
        hook(
            &dest,
            "post-receive",
            "#!/bin/sh\nhead -c 5242880 /dev/zero\n(sleep 0.1; printf completed >background-result) &\nexit 0\n",
        );
        hook(
            &dest,
            "post-update",
            "#!/bin/sh\nprintf completed >post-update-result\n",
        );
        if native {
            let prepared = PreparedPush::new(
                &f.repo.objects_with(PackLimits::default()).unwrap(),
                vec![command("refs/heads/main", None, main(&f))],
                &[],
                PushLimits::default().with_max_status_bytes(30),
                &AtomicBool::new(false),
            )
            .unwrap();
            let report = girt::push::send_local(
                dest.git_dir(),
                girt::push::LocalPushRequest::new(&prepared),
                girt::transport::TransportControl::new(&AtomicBool::new(false)).with_deadline(
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(10)),
                ),
                |_| {},
            )
            .unwrap();
            assert_eq!(report.progress.iter().map(Vec::len).sum::<usize>(), 30);
        } else {
            git(
                f.root.path(),
                &[
                    "push",
                    dest.git_dir().to_str().unwrap(),
                    "main:refs/heads/main",
                ],
                b"",
            );
        }
        assert_eq!(tip(&dest, "refs/heads/main"), Some(main(&f)));
        assert_eq!(
            fs::read(dest.git_dir().join("background-result")).unwrap(),
            b"completed"
        );
        assert_eq!(
            fs::read(dest.git_dir().join("post-update-result")).unwrap(),
            b"completed"
        );
    }
}

/// A stale bookmark must stop at its proven base, even beyond the former discovery cap.
#[rstest]
#[case::intact(false)]
#[case::old_tree_absent(true)]
fn deep_receiver_boundary_matches_git(#[case] remove_old_tree: bool) {
    let (source, repo, ids) = deep_push_history();
    let (_root, dest) = receiver_with(&source, ids[1001]);
    let (_git_root, git_dest) = receiver_with(&source, ids[1001]);
    unpack_receiver(&dest);
    unpack_receiver(&git_dest);
    remove_deep_old_tree(&source, &dest, remove_old_tree);
    remove_deep_old_tree(&source, &git_dest, remove_old_tree);
    let prepared = PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/new", None, ids[1002])],
        &[ids[1001]],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            git_dest.git_dir().to_str().unwrap(),
            "c1002:refs/heads/git-new",
        ],
        b"",
    );
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/new"), Some(ids[1002]));
    verify_deep_receiver(&dest, remove_old_tree);
}

fn deep_push_history() -> (tempfile::TempDir, Repository, Vec<ObjectId>) {
    let mut spec = String::from("@100");
    for index in 1..=1001 {
        spec += &format!(" {}@{}", index - 1, 100 + index);
    }
    // c1001 is master; c1 is master~1000, and c1002 forks from it.
    spec += " 1@1200";
    let fixture = import(&spec);
    git(
        fixture.0.path(),
        &[
            "update-ref",
            "refs/heads/master",
            &fixture.2[1001].to_string(),
        ],
        b"",
    );
    assert_eq!(
        git(fixture.0.path(), &["rev-parse", "master~1000"], b""),
        format!("{}\n", fixture.2[1]).as_bytes()
    );
    fixture
}

fn unpack_receiver(dest: &Repository) {
    let pack_dir = dest.object_dir().join("pack");
    for entry in std::fs::read_dir(&pack_dir).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "pack")
        {
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::fs::remove_file(path.with_extension("idx")).unwrap();
            git(dest.git_dir(), &["unpack-objects", "-q"], &bytes);
        }
    }
}

fn remove_deep_old_tree(source: &tempfile::TempDir, dest: &Repository, remove: bool) {
    if remove {
        let tree = String::from_utf8(git(source.path(), &["rev-parse", "c0^{tree}"], b"")).unwrap();
        let tree = tree.trim();
        std::fs::remove_file(dest.object_dir().join(&tree[..2]).join(&tree[2..])).unwrap();
    }
}

fn verify_deep_receiver(dest: &Repository, old_tree_absent: bool) {
    if !old_tree_absent {
        git(dest.git_dir(), &["fsck", "--strict", "--no-reflogs"], b"");
    }
}

#[test]
fn non_tip_receiver_boundary_in_alternate() {
    let (source, repo, ids) = import("@100 0@200 1@300 1@400");
    let (_root, dest) = receiver_with(&source, ids[2]);
    let alternate = tempfile::tempdir().unwrap();
    let alternate_objects = alternate.path().join("objects");
    std::fs::rename(dest.object_dir(), &alternate_objects).unwrap();
    std::fs::create_dir_all(dest.object_dir().join("info")).unwrap();
    std::fs::write(
        dest.object_dir().join("info/alternates"),
        format!("{}\n", alternate_objects.display()),
    )
    .unwrap();
    let prepared = PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/new", None, ids[3])],
        &[ids[2]],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/new"), Some(ids[3]));
    git(dest.git_dir(), &["fsck", "--strict", "--no-reflogs"], b"");
}

/// Every valid receiver root participates, including the last root in object-ID order.
#[test]
fn receiver_boundary_after_4096_roots_matches_git() {
    let (source, repo, roots, base, new) = many_ref_history();
    let (_destination_root, dest) = destination(true);
    git(
        source.path(),
        &["push", "--quiet", dest.git_dir().to_str().unwrap(), "--all"],
        b"",
    );
    let (_git_root, git_dest) = destination(true);
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            git_dest.git_dir().to_str().unwrap(),
            "--all",
        ],
        b"",
    );
    unpack_receiver(&dest);
    unpack_receiver(&git_dest);
    let tree = String::from_utf8(git(
        source.path(),
        &["rev-parse", &format!("{base}^{{tree}}")],
        b"",
    ))
    .unwrap();
    let tree = tree.trim();
    std::fs::remove_file(dest.object_dir().join(&tree[..2]).join(&tree[2..])).unwrap();
    std::fs::remove_file(git_dest.object_dir().join(&tree[..2]).join(&tree[2..])).unwrap();
    let prepared = PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/new", None, new)],
        &roots,
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    git(
        source.path(),
        &[
            "push",
            "--quiet",
            git_dest.git_dir().to_str().unwrap(),
            &format!("{new}:refs/heads/git-new"),
        ],
        b"",
    );
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/new"), Some(new));
}

fn many_ref_history() -> (
    tempfile::TempDir,
    Repository,
    Vec<ObjectId>,
    ObjectId,
    ObjectId,
) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "--template=", "."], b"");
    let mut stream = String::new();
    for index in 0..4100 {
        let contents = format!("root {index}\n");
        stream += &format!(
            "commit refs/heads/r{index}\nmark :{}\ncommitter A <a@example.com> 100 +0000\ndata {}\n{contents}deleteall\nM 100644 inline f\ndata {}\n{contents}",
            index + 1,
            contents.len(),
            contents.len()
        );
        stream += &format!(
            "commit refs/heads/r{index}\ncommitter A <a@example.com> 200 +0000\ndata 4\ntip\nfrom :{}\n",
            index + 1
        );
    }
    git(root.path(), &["fast-import", "--quiet"], stream.as_bytes());
    let mut roots: Vec<ObjectId> = String::from_utf8(git(
        root.path(),
        &["for-each-ref", "--format=%(objectname)", "refs/heads"],
        b"",
    ))
    .unwrap()
    .lines()
    .map(|line| line.parse().unwrap())
    .collect();
    roots.sort_unstable();
    let last = roots.last().unwrap();
    let base: ObjectId =
        String::from_utf8(git(root.path(), &["rev-parse", &format!("{last}~1")], b""))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
    let empty = String::from_utf8(git(root.path(), &["mktree"], b"")).unwrap();
    let new = String::from_utf8(git(
        root.path(),
        &["commit-tree", empty.trim(), "-p", &base.to_string()],
        b"new\n",
    ))
    .unwrap()
    .trim()
    .parse()
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, roots, base, new)
}

#[cfg(unix)]
#[test]
fn deep_receiver_boundary_with_quarantine() {
    let (source, repo, ids) = deep_push_history();
    let (_root, dest) = receiver_with(&source, ids[1001]);
    hook(
        &dest,
        "pre-receive",
        &format!(
            "#!/bin/sh\ntest -n \"$GIT_QUARANTINE_PATH\" || exit 9\ngit cat-file -e {} || exit 9\nprintf quarantine >hook-observed\ncat >/dev/null\n",
            ids[1002]
        ),
    );
    let prepared = PreparedPush::new(
        &repo.objects_with(PackLimits::default()).unwrap(),
        vec![command("refs/heads/new", None, ids[1002])],
        &[ids[1001]],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        send_local(dest.git_dir(), &prepared, &AtomicBool::new(false))
            .unwrap()
            .all_succeeded()
    );
    assert_eq!(tip(&dest, "refs/heads/new"), Some(ids[1002]));
    assert_eq!(
        fs::read(dest.git_dir().join("hook-observed")).unwrap(),
        b"quarantine"
    );
    git(dest.git_dir(), &["fsck", "--strict", "--no-reflogs"], b"");
}
