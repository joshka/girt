//! Git CLI fixtures generated at runtime; no upstream source or fixtures are used.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::refs::{Expected, RefName, ReferenceError, Target};
use girt::{ObjectId, Repository};
use rstest::rstest;

fn git_attempt(directory: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }

    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A. Writer")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0530")
        .env("GIT_COMMITTER_NAME", "C. Recorder")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000123 -0700")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git is required for interoperability tests");

    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let output = git_attempt(directory, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    output.stdout
}

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}
fn oid(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}
fn fixture() -> (tempfile::TempDir, Repository, ObjectId) {
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
    let tree = git(root.path(), &["mktree"], b"");
    let commit = git(
        root.path(),
        &["commit-tree", std::str::from_utf8(&tree).unwrap().trim()],
        b"First\n",
    );
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, oid(&commit))
}

#[rstest]
#[case::ordinary("refs/heads/topic", true)]
#[case::dot_component_end("refs/heads/a./b", true)]
#[case::at("refs/heads/@", true)]
#[case::dash("refs/heads/-a", true)]
#[case::lock("refs/heads/a.lock/b", false)]
#[case::traversal("refs/heads/../b", false)]
#[case::reflog("refs/heads/a@{b", false)]
#[case::trailing("refs/heads/a.", false)]
#[case::double_slash("refs//a", false)]
#[case::space("refs/heads/a b", false)]
#[case::backslash("refs/heads/a\\b", false)]
fn names_agree_with_git(#[case] value: &str, #[case] valid: bool) {
    let result = Command::new("git")
        .args(["check-ref-format", value])
        .output()
        .unwrap();
    assert_eq!(result.status.success(), valid);
    assert_eq!(RefName::new(value).is_ok(), valid);
}

#[test]
fn reads_git_loose_packed_and_symbolic_then_shadows_packed() {
    let (root, repo, first) = fixture();
    let refs = repo.references().unwrap();
    let branch = name("refs/heads/main");
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(
        root.path(),
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
        b"",
    );
    assert_eq!(refs.read(&branch).unwrap(), Some(Target::Direct(first)));
    assert_eq!(
        refs.read(&name("refs/heads/alias")).unwrap(),
        Some(Target::Symbolic(branch.clone()))
    );
    git(
        root.path(),
        &["tag", "-a", "v1", "-m", "tag", &first.to_string()],
        b"",
    );
    let tag = oid(&git(root.path(), &["rev-parse", "refs/tags/v1"], b""));
    git(root.path(), &["pack-refs", "--all"], b"");
    let packed_before = fs::read(root.path().join("packed-refs")).unwrap();
    assert!(packed_before.contains(&b'^'));
    assert_eq!(
        refs.resolve(&name("refs/tags/v1"), 0).unwrap().id,
        Some(tag)
    );
    assert_eq!(refs.resolve(&name("HEAD"), 8).unwrap().id, Some(first));
    let second = second_commit(root.path(), first);
    refs.update_without_reflog(
        &branch,
        Target::Direct(second),
        Expected::Value(Target::Direct(first)),
    )
    .unwrap();
    assert_eq!(oid(&git(root.path(), &["rev-parse", "HEAD"], b"")), second);
    assert_eq!(
        refs.resolve(&name("refs/heads/alias"), 8).unwrap().id,
        Some(second)
    );
    assert_eq!(
        fs::read(root.path().join("packed-refs")).unwrap(),
        packed_before
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/main",
            &first.to_string(),
            &second.to_string(),
        ],
        b"",
    );
    assert_eq!(refs.read(&branch).unwrap(), Some(Target::Direct(first)));
}
fn second_commit(root: &Path, first: ObjectId) -> ObjectId {
    let tree = git(root, &["mktree"], b"");
    oid(&git(
        root,
        &[
            "commit-tree",
            std::str::from_utf8(&tree).unwrap().trim(),
            "-p",
            &first.to_string(),
        ],
        b"Second\n",
    ))
}

#[test]
fn publishes_unborn_branch_and_distinguishes_symbolic_replacement() {
    let (root, repo, first) = fixture();
    let refs = repo.references().unwrap();
    assert_eq!(refs.resolve(&name("HEAD"), 8).unwrap().id, None);
    assert_eq!(
        refs.update_resolved_without_reflog(&name("HEAD"), first, Expected::Absent)
            .unwrap(),
        name("refs/heads/main")
    );
    assert_eq!(
        git(root.path(), &["symbolic-ref", "HEAD"], b""),
        b"refs/heads/main\n"
    );
    assert_eq!(oid(&git(root.path(), &["rev-parse", "HEAD"], b"")), first);
    let second = second_commit(root.path(), first);
    refs.update_without_reflog(
        &name("HEAD"),
        Target::Direct(second),
        Expected::Value(Target::Symbolic(name("refs/heads/main"))),
    )
    .unwrap();
    assert_eq!(oid(&git(root.path(), &["rev-parse", "HEAD"], b"")), second);
    assert_eq!(
        oid(&git(root.path(), &["rev-parse", "refs/heads/main"], b"")),
        first
    );
}

#[test]
fn explicitly_omits_new_and_existing_reflogs() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["config", "core.logAllRefUpdates", "true"],
        b"",
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let before = fs::read(root.path().join("logs/refs/heads/main")).unwrap();
    let head_before = fs::read(root.path().join("logs/HEAD")).unwrap();
    let second = second_commit(root.path(), first);
    let refs = repo.references().unwrap();
    refs.update_resolved_without_reflog(
        &name("HEAD"),
        second,
        Expected::Value(Target::Direct(first)),
    )
    .unwrap();
    refs.update_without_reflog(
        &name("refs/heads/new"),
        Target::Direct(second),
        Expected::Absent,
    )
    .unwrap();
    assert_eq!(
        fs::read(root.path().join("logs/refs/heads/main")).unwrap(),
        before
    );
    assert_eq!(
        fs::read(root.path().join("logs/HEAD")).unwrap(),
        head_before
    );
    assert!(!root.path().join("logs/refs/heads/new").exists());
    assert_eq!(oid(&git(root.path(), &["rev-parse", "HEAD"], b"")), second);
    assert_eq!(
        oid(&git(
            root.path(),
            &["reflog", "show", "--format=%H", "-1", "main"],
            b""
        )),
        first
    );
}

#[rstest]
#[case::loose(false, "refs/heads/topic", "refs/heads/topic/sub")]
#[case::loose_child(false, "refs/heads/topic/sub", "refs/heads/topic")]
#[case::packed(true, "refs/heads/topic", "refs/heads/topic/sub")]
#[case::packed_child(true, "refs/heads/topic/sub", "refs/heads/topic")]
fn namespace_conflicts_preserve_existing(
    #[case] pack: bool,
    #[case] existing: &str,
    #[case] new: &str,
) {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", existing, &first.to_string()],
        b"",
    );
    prepare_packed(root.path(), pack);
    let result = repo.references().unwrap().update_without_reflog(
        &name(new),
        Target::Direct(first),
        Expected::Absent,
    );
    assert!(matches!(result, Err(ReferenceError::Conflict(_))));
    assert_eq!(oid(&git(root.path(), &["rev-parse", existing], b"")), first);
    assert!(!root.path().join("packed-refs.lock").exists());
}
fn prepare_packed(root: &Path, pack: bool) {
    if pack {
        git(root, &["pack-refs", "--all"], b"");
    }
}

#[test]
fn conditional_writers_cannot_both_succeed() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let second = second_commit(root.path(), first);
    let barrier = std::sync::Barrier::new(2);
    let update = || {
        barrier.wait();
        repo.references().unwrap().update_without_reflog(
            &name("refs/heads/main"),
            Target::Direct(second),
            Expected::Value(Target::Direct(first)),
        )
    };
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(update);
        let b = scope.spawn(update);
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        oid(&git(root.path(), &["rev-parse", "refs/heads/main"], b"")),
        second
    );
    assert!(!root.path().join("refs/heads/main.lock").exists());
    assert!(!root.path().join("packed-refs.lock").exists());
}

#[rstest]
#[case::bisect("refs/bisect/test")]
#[case::rewritten("refs/rewritten/test")]
#[case::worktree("refs/worktree/test")]
fn linked_worktree_routes_shared_and_private_refs(#[case] private: &str) {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let parent = tempfile::tempdir().unwrap();
    let worktree = parent.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().unwrap(),
            &first.to_string(),
        ],
        b"",
    );
    let linked = Repository::open(&worktree).unwrap();
    let second = second_commit(root.path(), first);
    let refs = linked.references().unwrap();
    refs.update_without_reflog(&name(private), Target::Direct(second), Expected::Absent)
        .unwrap();
    refs.update_without_reflog(
        &name("refs/heads/shared"),
        Target::Direct(second),
        Expected::Absent,
    )
    .unwrap();
    refs.update_without_reflog(
        &name("HEAD"),
        Target::Direct(second),
        Expected::Value(Target::Direct(first)),
    )
    .unwrap();
    assert_eq!(oid(&git(&worktree, &["rev-parse", private], b"")), second);
    assert_eq!(
        repo.references().unwrap().read(&name(private)).unwrap(),
        None
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .resolve(&name("HEAD"), 8)
            .unwrap()
            .id,
        Some(first)
    );
    assert_eq!(
        oid(&git(root.path(), &["rev-parse", "refs/heads/shared"], b"")),
        second
    );
    assert_eq!(oid(&git(&worktree, &["rev-parse", "HEAD"], b"")), second);
    assert!(linked.git_dir().join(private).exists());
    assert!(!repo.common_dir().join(private).exists());
}

#[test]
fn non_utf8_names_round_trip_with_git() {
    use std::os::unix::ffi::OsStrExt;
    let (root, repo, first) = fixture();
    let raw = b"refs/heads/byte-\xff";
    let reference = RefName::new(raw).unwrap();
    let refs = repo.references().unwrap();
    // Packed names retain bytes even on APFS, which rejects non-UTF-8 loose filenames.
    fs::write(
        root.path().join("packed-refs"),
        [first.to_string().as_bytes(), b" ", raw, b"\n"].concat(),
    )
    .unwrap();
    let result = Command::new("git")
        .current_dir(root.path())
        .arg("check-ref-format")
        .arg(std::ffi::OsStr::from_bytes(raw))
        .output()
        .unwrap();
    assert!(result.status.success());
    let expected = [first.to_string().as_bytes(), b" ", raw, b"\n"].concat();
    assert_eq!(git(root.path(), &["show-ref"], b""), expected);
    assert_eq!(refs.read(&reference).unwrap(), Some(Target::Direct(first)));
}

#[test]
fn malformed_loose_does_not_fall_back_to_packed() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    fs::write(root.path().join("refs/heads/main"), b"broken\n").unwrap();
    let refs = repo.references().unwrap();
    assert!(matches!(
        refs.read(&name("refs/heads/main")),
        Err(ReferenceError::Malformed { .. })
    ));
    assert!(matches!(
        refs.update_without_reflog(
            &name("refs/heads/main"),
            Target::Direct(first),
            Expected::Any
        ),
        Err(ReferenceError::Malformed { .. })
    ));
    assert_eq!(
        fs::read(root.path().join("refs/heads/main")).unwrap(),
        b"broken\n"
    );
    assert!(!root.path().join("packed-refs.lock").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_loose_names_on_linux() {
    let (root, repo, first) = fixture();
    let reference = RefName::new(b"refs/heads/byte-\xff").unwrap();
    repo.references()
        .unwrap()
        .update_without_reflog(&reference, Target::Direct(first), Expected::Absent)
        .unwrap();
    assert_eq!(
        repo.references().unwrap().read(&reference).unwrap(),
        Some(Target::Direct(first))
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    assert_eq!(
        repo.references().unwrap().read(&reference).unwrap(),
        Some(Target::Direct(first))
    );
}

#[rstest]
#[case::reftable("refStorage", "reftable")]
#[case::unknown("refStorage", "unknown")]
fn unsupported_backends_fail_open(#[case] key: &str, #[case] value: &str) {
    let (root, _repo, _first) = fixture();
    fs::write(
        root.path().join("config"),
        format!("[core]\nrepositoryformatversion=1\nbare=true\n[extensions]\n{key}={value}\n"),
    )
    .unwrap();
    assert!(matches!(
        Repository::open(root.path()),
        Err(girt::OpenError::Unsupported { .. })
    ));
}

#[rstest]
#[case::unknown_header(b"# pack-refs with: future\n")]
#[case::truncated(b"1111111111111111111111111111111111111111 refs/heads/main")]
#[case::invalid_peel(b"1111111111111111111111111111111111111111 refs/tags/a\n^bad\n")]
fn malformed_packed_blocks_updates_and_preserves_data(#[case] bytes: &[u8]) {
    let (root, repo, first) = fixture();
    fs::write(root.path().join("packed-refs"), bytes).unwrap();
    let refs = repo.references().unwrap();
    assert!(refs.read(&name("refs/heads/main")).is_err());
    assert!(
        refs.update_without_reflog(
            &name("refs/heads/new"),
            Target::Direct(first),
            Expected::Absent
        )
        .is_err()
    );
    assert_eq!(fs::read(root.path().join("packed-refs")).unwrap(), bytes);
    assert!(!root.path().join("refs/heads/new").exists());
    assert!(!root.path().join("packed-refs.lock").exists());
}

#[test]
fn git_dangling_symbolic_ref_resolves_to_missing_name() {
    let (root, repo, _first) = fixture();
    git(
        root.path(),
        &["symbolic-ref", "refs/heads/dangling", "refs/heads/missing"],
        b"",
    );
    let resolved = repo
        .references()
        .unwrap()
        .resolve(&name("refs/heads/dangling"), 8)
        .unwrap();
    assert_eq!(resolved.name, name("refs/heads/missing"));
    assert_eq!(resolved.id, None);
    assert_eq!(
        git(root.path(), &["symbolic-ref", "refs/heads/dangling"], b""),
        b"refs/heads/missing\n"
    );
}

#[test]
fn dangling_object_id_is_not_object_lookup() {
    let (_root, repo, _first) = fixture();
    let missing = ObjectId::Sha1([0x55; 20]);
    let refs = repo.references().unwrap();
    refs.update_without_reflog(
        &name("refs/tags/missing"),
        Target::Direct(missing),
        Expected::Absent,
    )
    .unwrap();
    assert_eq!(
        refs.resolve(&name("refs/tags/missing"), 0).unwrap().id,
        Some(missing)
    );
    assert!(
        repo.loose_objects()
            .unwrap()
            .read_blob(missing, 100)
            .is_err()
    );
}

#[test]
fn packed_value_is_not_absent_and_failed_condition_cleans_lock() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    let refs = repo.references().unwrap();
    assert!(
        matches!(refs.update_without_reflog(&name("refs/heads/main"), Target::Direct(first), Expected::Absent), Err(ReferenceError::Mismatch { actual: Some(Target::Direct(id)) }) if id == first)
    );
    assert!(!root.path().join("refs/heads/main").exists());
    assert!(!root.path().join("refs/heads/main.lock").exists());
}

#[test]
fn git_and_girt_conditional_writers_cannot_both_succeed() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let second = second_commit(root.path(), first);
    let barrier = std::sync::Barrier::new(2);
    let (ours, theirs) = std::thread::scope(|scope| {
        let ours = scope.spawn(|| {
            barrier.wait();
            repo.references().unwrap().update_without_reflog(
                &name("refs/heads/main"),
                Target::Direct(second),
                Expected::Value(Target::Direct(first)),
            )
        });
        let theirs = scope.spawn(|| {
            barrier.wait();
            git_attempt(
                root.path(),
                &[
                    "update-ref",
                    "refs/heads/main",
                    &second.to_string(),
                    &first.to_string(),
                ],
                b"",
            )
        });
        (ours.join().unwrap(), theirs.join().unwrap())
    });
    assert_eq!(
        usize::from(ours.is_ok()) + usize::from(theirs.status.success()),
        1
    );
    assert_eq!(
        oid(&git(root.path(), &["rev-parse", "refs/heads/main"], b"")),
        second
    );
    assert!(!root.path().join("refs/heads/main.lock").exists());
    assert!(!root.path().join("packed-refs.lock").exists());
}

#[test]
fn symbolic_head_cannot_point_outside_refs() {
    let (root, repo, _first) = fixture();
    let head = name("HEAD");
    assert!(
        !git_attempt(root.path(), &["symbolic-ref", "HEAD", "HEAD"], b"")
            .status
            .success()
    );
    assert!(matches!(
        repo.references().unwrap().update_without_reflog(
            &head,
            Target::Symbolic(head.clone()),
            Expected::Any
        ),
        Err(ReferenceError::InvalidHeadTarget)
    ));
    assert_eq!(
        fs::read(root.path().join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert!(!root.path().join("packed-refs.lock").exists());
    assert!(Repository::open(root.path()).is_ok());
}

#[test]
fn enumerates_git_refs_and_deletes_shadowed_branch_and_packed_tag() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(
        root.path(),
        &["tag", "-a", "keep", "-m", "keep", &first.to_string()],
        b"",
    );
    git(
        root.path(),
        &["tag", "-a", "remove", "-m", "remove", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    let packed_tags = fs::read(root.path().join("packed-refs")).unwrap();
    let keep = oid(&git(root.path(), &["rev-parse", "refs/tags/keep"], b""));
    let second = second_commit(root.path(), first);
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &second.to_string()],
        b"",
    );
    git(
        root.path(),
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
        b"",
    );
    let refs = repo.references().unwrap();
    let entries = refs.list().unwrap();
    let names: Vec<_> = entries.iter().map(|entry| entry.name.as_bytes()).collect();
    assert_eq!(
        names,
        vec![
            b"refs/heads/alias".as_slice(),
            b"refs/heads/main",
            b"refs/tags/keep",
            b"refs/tags/remove"
        ]
    );
    assert_eq!(entries[0].target, Target::Symbolic(name("refs/heads/main")));
    assert_eq!(entries[1].target, Target::Direct(second));
    assert_eq!(
        git(root.path(), &["for-each-ref", "--format=%(refname)"], b""),
        b"refs/heads/alias\nrefs/heads/main\nrefs/tags/keep\nrefs/tags/remove\n"
    );
    let keep_peel = git(root.path(), &["rev-parse", "refs/tags/keep^{}"], b"");
    let tags = refs.list_namespace(&name("refs/tags")).unwrap();
    refs.delete_without_reflog(
        &name("refs/tags/remove"),
        Expected::Value(tags[1].target.clone()),
    )
    .unwrap();
    refs.delete_without_reflog(
        &name("refs/heads/main"),
        Expected::Value(Target::Direct(second)),
    )
    .unwrap();
    assert_eq!(refs.read(&name("refs/heads/main")).unwrap(), None);
    assert!(
        !git_attempt(
            root.path(),
            &["show-ref", "--verify", "refs/heads/main"],
            b""
        )
        .status
        .success()
    );
    assert!(
        !git_attempt(
            root.path(),
            &["show-ref", "--verify", "refs/tags/remove"],
            b""
        )
        .status
        .success()
    );
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/tags/keep^{}"], b""),
        keep_peel
    );
    assert_eq!(
        git(root.path(), &["symbolic-ref", "refs/heads/alias"], b""),
        b"refs/heads/main\n"
    );
    let remaining = fs::read(root.path().join("packed-refs")).unwrap();
    let header = packed_tags
        .split_inclusive(|byte| *byte == b'\n')
        .next()
        .unwrap();
    let keep_record = format!("{keep} refs/tags/keep\n^{first}\n");
    assert_eq!(remaining, [header, keep_record.as_bytes()].concat());
    // Git can publish and repack after girt releases its locks; deletion left no stale old tip.
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string(), ""],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    assert_eq!(
        refs.read(&name("refs/heads/main")).unwrap(),
        Some(Target::Direct(first))
    );
}

#[rstest]
#[case::bisect("refs/bisect/test")]
#[case::rewritten("refs/rewritten/test")]
#[case::worktree("refs/worktree/test")]
fn lists_and_deletes_current_worktree_refs_without_touching_other_worktree(#[case] private: &str) {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(
        root.path(),
        &["update-ref", private, &first.to_string()],
        b"",
    );
    let parent = tempfile::tempdir().unwrap();
    let worktree = parent.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().unwrap(),
            &first.to_string(),
        ],
        b"",
    );
    let second = second_commit(root.path(), first);
    git(
        &worktree,
        &["update-ref", private, &second.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    let linked = Repository::open(&worktree).unwrap();
    let refs = linked.references().unwrap();
    assert_eq!(
        refs.list_namespace(&name(private)).unwrap()[0].target,
        Target::Direct(second)
    );
    refs.delete_without_reflog(&name(private), Expected::Value(Target::Direct(second)))
        .unwrap();
    refs.delete_without_reflog(
        &name("refs/heads/main"),
        Expected::Value(Target::Direct(first)),
    )
    .unwrap();
    assert!(refs.list().unwrap().is_empty());
    assert!(
        !git_attempt(&worktree, &["rev-parse", "--verify", private], b"")
            .status
            .success()
    );
    assert_eq!(oid(&git(root.path(), &["rev-parse", private], b"")), first);
    assert_eq!(
        repo.references().unwrap().list().unwrap()[0].target,
        Target::Direct(first)
    );
    assert!(
        !git_attempt(
            root.path(),
            &["show-ref", "--verify", "refs/heads/main"],
            b""
        )
        .status
        .success()
    );
}

#[test]
fn ordinary_gitdir_file_layout_lists_and_deletes_refs() {
    let parent = tempfile::tempdir().unwrap();
    let worktree = parent.path().join("worktree");
    let metadata = parent.path().join("metadata");
    git(
        parent.path(),
        &[
            "init",
            "--template=",
            "--initial-branch=main",
            "--object-format=sha1",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
        b"",
    );
    let tree = git(&worktree, &["mktree"], b"");
    let first = oid(&git(
        &worktree,
        &["commit-tree", std::str::from_utf8(&tree).unwrap().trim()],
        b"First\n",
    ));
    git(
        &worktree,
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(&worktree, &["pack-refs", "--all"], b"");
    let repo = Repository::open(&worktree).unwrap();
    assert_eq!(
        repo.references().unwrap().list().unwrap()[0].target,
        Target::Direct(first)
    );
    repo.references()
        .unwrap()
        .delete_resolved_without_reflog(&name("HEAD"), Expected::Value(Target::Direct(first)))
        .unwrap();
    assert_eq!(
        git(&worktree, &["symbolic-ref", "HEAD"], b""),
        b"refs/heads/main\n"
    );
    assert!(
        !git_attempt(&worktree, &["rev-parse", "--verify", "HEAD"], b"")
            .status
            .success()
    );
}

#[test]
fn git_conditional_update_racing_girt_deletion_cannot_both_succeed() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    let second = second_commit(root.path(), first);
    let barrier = std::sync::Barrier::new(2);
    let (ours, theirs) = std::thread::scope(|scope| {
        let ours = scope.spawn(|| {
            barrier.wait();
            repo.references().unwrap().delete_without_reflog(
                &name("refs/heads/main"),
                Expected::Value(Target::Direct(first)),
            )
        });
        let theirs = scope.spawn(|| {
            barrier.wait();
            git_attempt(
                root.path(),
                &[
                    "update-ref",
                    "refs/heads/main",
                    &second.to_string(),
                    &first.to_string(),
                ],
                b"",
            )
        });
        (ours.join().unwrap(), theirs.join().unwrap())
    });
    assert_ne!(ours.is_ok(), theirs.status.success());
    assert_ne!(
        repo.references()
            .unwrap()
            .read(&name("refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(first))
    );
}

#[test]
fn git_packing_racing_deletion_does_not_resurrect_the_branch() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    let second = second_commit(root.path(), first);
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &second.to_string()],
        b"",
    );
    let barrier = std::sync::Barrier::new(2);
    let (deleted, packed) = std::thread::scope(|scope| {
        let deleted = scope.spawn(|| {
            barrier.wait();
            repo.references().unwrap().delete_without_reflog(
                &name("refs/heads/main"),
                Expected::Value(Target::Direct(second)),
            )
        });
        let packed = scope.spawn(|| {
            barrier.wait();
            git_attempt(root.path(), &["pack-refs", "--all"], b"")
        });
        (deleted.join().unwrap(), packed.join().unwrap())
    });
    assert!(deleted.is_ok() || packed.status.success());
    let actual = repo
        .references()
        .unwrap()
        .read(&name("refs/heads/main"))
        .unwrap();
    assert_eq!(actual.is_none(), deleted.is_ok());
    assert_ne!(actual, Some(Target::Direct(first)));
}

#[test]
fn git_packed_writer_honors_lock_retained_across_replacement() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all"], b"");
    let second = second_commit(root.path(), first);
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &second.to_string()],
        b"",
    );
    // Independently exercise the publication protocol against Git: the reservation remains
    // present after a separate file replaces packed-refs. No Git implementation is consulted.
    fs::write(root.path().join("packed-refs.lock"), b"reservation").unwrap();
    fs::write(
        root.path().join("replacement"),
        b"# pack-refs with: sorted\n",
    )
    .unwrap();
    fs::rename(
        root.path().join("replacement"),
        root.path().join("packed-refs"),
    )
    .unwrap();
    let blocked = git_attempt(
        root.path(),
        &["-c", "core.packedRefsTimeout=0", "pack-refs", "--all"],
        b"",
    );
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("packed-refs.lock"));
    assert_eq!(
        fs::read(root.path().join("packed-refs.lock")).unwrap(),
        b"reservation"
    );
    assert_eq!(
        fs::read(root.path().join("packed-refs")).unwrap(),
        b"# pack-refs with: sorted\n"
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(second))
    );
    fs::remove_file(root.path().join("packed-refs.lock")).unwrap();
    git(root.path(), &["pack-refs", "--all"], b"");
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(second))
    );
}

fn transaction_log(message: &[u8]) -> girt::refs::Reflog {
    girt::refs::Reflog::Append {
        committer: girt::Signature {
            name: b"C. Recorder".to_vec(),
            email: b"committer@example.com".to_vec(),
            seconds: 1700000123,
            offset_minutes: -420,
        },
        message: message.to_vec(),
    }
}

fn logged_edit(
    reference: &str,
    target: Option<ObjectId>,
    expected: Expected,
) -> girt::refs::RefEdit {
    girt::refs::RefEdit {
        name: name(reference),
        dereference: reference == "HEAD",
        target: target.map(Target::Direct),
        expected,
        reflog: transaction_log(b"transaction"),
    }
}

#[test]
fn git_reads_transaction_records_and_girt_reads_git_appends() {
    let (root, repo, first) = fixture();
    let refs = repo.references().unwrap();
    refs.transaction(&[
        logged_edit("HEAD", Some(first), Expected::Absent),
        logged_edit("refs/tags/batch", Some(first), Expected::Absent),
    ])
    .unwrap();
    assert_eq!(oid(&git(root.path(), &["rev-parse", "HEAD"], b"")), first);
    let bytes = fs::read(root.path().join("logs/HEAD")).unwrap();
    assert_eq!(
        bytes,
        format!(
            "{} {first} C. Recorder <committer@example.com> 1700000123 -0700\ttransaction\n",
            ObjectId::Sha1([0; 20])
        )
        .as_bytes()
    );
    let shown = git(
        root.path(),
        &["reflog", "show", "--format=%H|%gn|%ge|%gs", "HEAD"],
        b"",
    );
    assert_eq!(
        shown,
        format!("{first}|C. Recorder|committer@example.com|transaction\n").as_bytes()
    );
    let tree = git(root.path(), &["mktree"], b"");
    let second = oid(&git(
        root.path(),
        &[
            "commit-tree",
            std::str::from_utf8(&tree).unwrap().trim(),
            "-p",
            &first.to_string(),
        ],
        b"Second\n",
    ));
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "git append",
            "HEAD",
            &second.to_string(),
            &first.to_string(),
        ],
        b"",
    );
    let entries = refs.reflog(&name("HEAD")).unwrap().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].old, first);
    assert_eq!(entries[1].new, second);
    assert_eq!(entries[1].message, b"git append");
    assert_eq!(entries[1].committer.seconds, 1700000123);
    assert_eq!(entries[1].committer.offset_minutes, -420);
    assert_eq!(
        oid(&git(root.path(), &["rev-parse", "HEAD@{1}"], b"")),
        first
    );
    assert_eq!(
        refs.reflog(&name("refs/heads/main")).unwrap(),
        refs.reflog(&name("HEAD")).unwrap()
    );
}

#[test]
fn transaction_deletes_packed_and_shadowed_refs_without_resurrection() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/tags/a", &first.to_string()],
        b"",
    );
    git(
        root.path(),
        &["update-ref", "refs/tags/b", &first.to_string()],
        b"",
    );
    git(root.path(), &["pack-refs", "--all", "--prune"], b"");
    repo.references()
        .unwrap()
        .update_without_reflog(&name("refs/tags/b"), Target::Direct(first), Expected::Any)
        .unwrap();
    repo.references()
        .unwrap()
        .transaction(&[
            logged_edit("refs/tags/a", None, Expected::Value(Target::Direct(first))),
            logged_edit("refs/tags/b", None, Expected::Value(Target::Direct(first))),
        ])
        .unwrap();
    assert!(
        !git_attempt(root.path(), &["show-ref", "--verify", "refs/tags/a"], b"")
            .status
            .success()
    );
    assert!(
        !git_attempt(root.path(), &["show-ref", "--verify", "refs/tags/b"], b"")
            .status
            .success()
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .reflog(&name("refs/tags/b"))
            .unwrap()
            .unwrap()[0]
            .new,
        ObjectId::Sha1([0; 20])
    );
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "recreate",
            "refs/tags/b",
            &first.to_string(),
        ],
        b"",
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .reflog(&name("refs/tags/b"))
            .unwrap()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn git_empty_message_reflog_is_readable() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &[
            "update-ref",
            "--create-reflog",
            "refs/tags/a",
            &first.to_string(),
        ],
        b"",
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .reflog(&name("refs/tags/a"))
            .unwrap()
            .unwrap()[0]
            .message,
        b""
    );
}

#[test]
fn linked_worktree_transaction_routes_head_branch_and_private_logs() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let linked = root.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked.to_str().unwrap(),
            "main",
        ],
        b"",
    );
    let linked_repo = Repository::open(&linked).unwrap();
    let refs = linked_repo.references().unwrap();
    refs.transaction(&[
        logged_edit("HEAD", Some(first), Expected::Value(Target::Direct(first))),
        logged_edit("refs/bisect/good", Some(first), Expected::Absent),
        logged_edit("refs/rewritten/a", Some(first), Expected::Absent),
        logged_edit("refs/worktree/a", Some(first), Expected::Absent),
    ])
    .unwrap();
    assert!(linked_repo.git_dir().join("logs/HEAD").is_file());
    assert!(repo.common_dir().join("logs/refs/heads/linked").is_file());
    assert!(
        linked_repo
            .git_dir()
            .join("logs/refs/bisect/good")
            .is_file()
    );
    assert!(
        linked_repo
            .git_dir()
            .join("logs/refs/rewritten/a")
            .is_file()
    );
    assert!(linked_repo.git_dir().join("logs/refs/worktree/a").is_file());
    assert!(!repo.common_dir().join("logs/refs/bisect/good").exists());
    let shown = git(
        &linked,
        &["reflog", "show", "-1", "--format=%gs", "HEAD"],
        b"",
    );
    assert_eq!(shown, b"transaction\n");
    assert_eq!(
        oid(&git(&linked, &["rev-parse", "refs/worktree/a"], b"")),
        first
    );
}

#[test]
fn separate_git_directory_routes_transaction_logs() {
    let root = tempfile::tempdir().unwrap();
    let metadata = root.path().join("metadata");
    let worktree = root.path().join("worktree");
    git(
        root.path(),
        &[
            "init",
            "--template=",
            "--object-format=sha1",
            "--initial-branch=main",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
        b"",
    );
    let tree = git(&worktree, &["mktree"], b"");
    let first = oid(&git(
        &worktree,
        &["commit-tree", std::str::from_utf8(&tree).unwrap().trim()],
        b"First\n",
    ));
    let repo = Repository::open(&worktree).unwrap();
    repo.references()
        .unwrap()
        .transaction(&[logged_edit("HEAD", Some(first), Expected::Absent)])
        .unwrap();
    assert!(metadata.join("logs/HEAD").is_file());
    assert_eq!(
        git(&worktree, &["reflog", "show", "-1", "--format=%gs"], b""),
        b"transaction\n"
    );
}

#[test]
fn git_conditional_writer_racing_transaction_cannot_both_succeed() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let second = second_commit(root.path(), first);
    let barrier = std::sync::Barrier::new(2);
    let (batch, other) = std::thread::scope(|scope| {
        let batch = scope.spawn(|| {
            barrier.wait();
            repo.references().unwrap().transaction(&[
                logged_edit(
                    "refs/heads/main",
                    Some(second),
                    Expected::Value(Target::Direct(first)),
                ),
                logged_edit("refs/tags/batch", Some(second), Expected::Absent),
            ])
        });
        let other = scope.spawn(|| {
            barrier.wait();
            git_attempt(
                root.path(),
                &[
                    "update-ref",
                    "-m",
                    "Git race",
                    "refs/heads/main",
                    &second.to_string(),
                    &first.to_string(),
                ],
                b"",
            )
        });
        (batch.join().unwrap(), other.join().unwrap())
    });
    assert_eq!(
        usize::from(batch.is_ok()) + usize::from(other.status.success()),
        1
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/tags/batch"))
            .unwrap()
            .is_some(),
        batch.is_ok()
    );
    assert!(!root.path().join("packed-refs.lock").exists());
    assert!(!root.path().join("refs/heads/main.lock").exists());
}

#[test]
fn detached_head_transaction_records_previous_tip() {
    let (root, repo, first) = fixture();
    git(
        root.path(),
        &["update-ref", "--no-deref", "HEAD", &first.to_string()],
        b"",
    );
    let second = second_commit(root.path(), first);
    repo.references()
        .unwrap()
        .transaction(&[logged_edit(
            "HEAD",
            Some(second),
            Expected::Value(Target::Direct(first)),
        )])
        .unwrap();
    assert_eq!(
        repo.references().unwrap().read(&name("HEAD")).unwrap(),
        Some(Target::Direct(second))
    );
    let entries = repo
        .references()
        .unwrap()
        .reflog(&name("HEAD"))
        .unwrap()
        .unwrap();
    assert_eq!(entries.last().unwrap().old, first);
    assert_eq!(entries.last().unwrap().new, second);
    assert_eq!(
        git(
            root.path(),
            &["reflog", "show", "-1", "--format=%gs", "HEAD"],
            b""
        ),
        b"transaction\n"
    );
}

#[test]
fn symbolic_to_direct_head_logging_matches_git_and_preserves_branch() {
    let (root, repo, first) = fixture();
    let second = second_commit(root.path(), first);
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &first.to_string()],
        b"",
    );
    let operation = girt::refs::RefEdit {
        name: name("HEAD"),
        dereference: false,
        target: Some(Target::Direct(second)),
        expected: Expected::Value(Target::Symbolic(name("refs/heads/main"))),
        reflog: transaction_log(b"detach"),
    };
    let outcomes = repo
        .references()
        .unwrap()
        .transaction(&[operation])
        .unwrap();
    let (git_root, _git_repo, git_first) = fixture();
    let git_second = second_commit(git_root.path(), git_first);
    git(
        git_root.path(),
        &["update-ref", "refs/heads/main", &git_first.to_string()],
        b"",
    );
    git(
        git_root.path(),
        &[
            "update-ref",
            "--create-reflog",
            "--no-deref",
            "-m",
            "detach",
            "HEAD",
            &git_second.to_string(),
        ],
        b"",
    );
    assert_eq!(
        fs::read(root.path().join("logs/HEAD")).unwrap(),
        fs::read(git_root.path().join("logs/HEAD")).unwrap()
    );
    assert_eq!(
        fs::read(root.path().join("HEAD")).unwrap(),
        fs::read(git_root.path().join("HEAD")).unwrap()
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/heads/main"))
            .unwrap(),
        Some(Target::Direct(first))
    );
    assert!(!root.path().join("logs/refs/heads/main").exists());
    assert_eq!(
        outcomes[0].logs,
        vec![(name("HEAD"), girt::refs::LogOutcome::Appended)]
    );
    git(root.path(), &["fsck", "--strict"], b"");
}
