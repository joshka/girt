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
    let missing = ObjectId::from_bytes([0x55; 20]);
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
