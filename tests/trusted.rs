//! Original disposable fixtures; Git's CLI is the interoperability oracle.
use std::path::Path;
use std::process::Command;

use girt::trusted::Handle;
use girt::workflow::Environment;
use girt::{Commit, InitKind, ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn git(path: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let output = command
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn environment() -> Environment {
    Environment::default()
        .with("GIT_AUTHOR_NAME", "Writer")
        .with("GIT_AUTHOR_EMAIL", "a@example.com")
        .with("GIT_COMMITTER_NAME", "Recorder")
        .with("GIT_COMMITTER_EMAIL", "c@example.com")
        .with("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .with("GIT_COMMITTER_DATE", "@1700000001 +0000")
}
fn handle(format: ObjectFormat) -> (tempfile::TempDir, Handle) {
    handle_with_backend(format, girt::refs::Backend::Files)
}
fn handle_with_backend(
    format: ObjectFormat,
    backend: girt::refs::Backend,
) -> (tempfile::TempDir, Handle) {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(format, directory.path(), InitKind::Worktree, backend)
        .unwrap();
    (directory, Handle::new(repo).with_environment(environment()))
}

fn tree(git: &Handle, bytes: &[u8]) -> ObjectId {
    let blob = git.repository().loose_objects().write_blob(bytes).unwrap();
    let mut editor = git.tree_editor();
    editor.set(b"dir/file", blob).unwrap();
    editor.write().unwrap()
}

#[rstest]
#[case::sha1_files(ObjectFormat::Sha1, girt::refs::Backend::Files)]
#[case::sha256_files(ObjectFormat::Sha256, girt::refs::Backend::Files)]
#[case::sha1_reftable(ObjectFormat::Sha1, girt::refs::Backend::Reftable)]
#[case::sha256_reftable(ObjectFormat::Sha256, girt::refs::Backend::Reftable)]
fn commit_reads_and_reflogs_match_git(
    #[case] format: ObjectFormat,
    #[case] backend: girt::refs::Backend,
) {
    let (dir, mut handle) = handle_with_backend(format, backend);
    let tree = tree(&handle, b"first\n");
    let first = handle.commit(tree, "Root\n").unwrap();
    let second = handle.commit(tree, "Child\n").unwrap();

    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), second.to_string());
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD~1"]), first.to_string());
    assert_eq!(git(dir.path(), &["show", "HEAD:dir/file"]), "first");
    assert_eq!(handle.blob_at("HEAD", b"dir/file").unwrap(), b"first\n");
    assert_eq!(handle.rev_parse("HEAD~1").unwrap(), first);
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%an/%cn"]),
        "Writer/Recorder"
    );
    assert_eq!(
        git(dir.path(), &["reflog", "show", "--format=%gs", "HEAD"]),
        "commit: Child\ncommit (initial): Root"
    );
    let branch = git(dir.path(), &["symbolic-ref", "HEAD"]);
    assert_eq!(
        git(dir.path(), &["reflog", "show", "--format=%gs", &branch]),
        "commit: Child\ncommit (initial): Root"
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cached_reader_refreshes_after_git_packs_new_history(#[case] format: ObjectFormat) {
    let (dir, mut handle) = handle(format);
    let tree = tree(&handle, b"one");
    let first = handle.commit(tree, "root").unwrap();
    assert_eq!(handle.rev_parse("HEAD").unwrap(), first);
    let tree = tree_for_new_handle(dir.path(), b"new pack");
    let mut writer =
        Handle::new(Repository::open(dir.path()).unwrap()).with_environment(environment());
    let next = writer.commit(tree, "new").unwrap();
    git(dir.path(), &["repack", "-ad"]);
    git(dir.path(), &["prune-packed"]);

    assert_eq!(handle.blob_at("HEAD", b"dir/file").unwrap(), b"new pack");
    assert_eq!(handle.rev_parse(&next.to_string()[..12]).unwrap(), next);
}
fn tree_for_new_handle(path: &Path, bytes: &[u8]) -> ObjectId {
    tree(&Handle::new(Repository::open(path).unwrap()), bytes)
}

#[test]
fn log_is_lazy_and_stops_after_a_missing_parent() {
    let (_dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    let missing = ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    let signature = Signature::now(b"Writer", b"a@example.com").unwrap();
    let commit = Commit::builder(tree, signature, b"broken")
        .parents([missing])
        .build()
        .unwrap();
    let id = handle
        .repository()
        .loose_objects()
        .write_commit(&commit)
        .unwrap();
    let mut log = handle.log(&id.to_string()).unwrap();

    assert_eq!(log.next().unwrap().unwrap().0, id);
    assert!(
        log.next()
            .unwrap()
            .unwrap_err()
            .is::<girt::ReadCommitError>()
    );
    assert!(log.next().is_none());
}

#[test]
fn date_priority_walk_matches_git_on_monotonic_merge_history() {
    let (dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    let root = handle.commit(tree, "root").unwrap();
    let loose = handle.repository().loose_objects();
    let signature = Signature {
        name: b"A".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1700000002,
        offset_minutes: 0,
    };
    let left = loose
        .write_commit(
            &Commit::builder(tree, signature.clone(), b"left")
                .parents([root])
                .build()
                .unwrap(),
        )
        .unwrap();
    let right_signature = Signature {
        seconds: 1700000003,
        ..signature.clone()
    };
    let right = loose
        .write_commit(
            &Commit::builder(tree, right_signature, b"right")
                .parents([root])
                .build()
                .unwrap(),
        )
        .unwrap();
    let merge_signature = Signature {
        seconds: 1700000004,
        ..signature
    };
    let merge = loose
        .write_commit(
            &Commit::builder(tree, merge_signature, b"merge")
                .parents([left, right])
                .build()
                .unwrap(),
        )
        .unwrap();

    let actual: Vec<_> = handle
        .log(&merge.to_string())
        .unwrap()
        .map(|entry| entry.unwrap().0.to_string())
        .collect();
    assert_eq!(actual, [merge, right, left, root].map(|id| id.to_string()));
    assert_eq!(
        actual.join("\n"),
        git(
            dir.path(),
            &["rev-list", "--date-order", &merge.to_string()]
        )
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn switch_updates_files_index_head_and_refuses_dirty_state(#[case] format: ObjectFormat) {
    let (dir, mut handle) = handle(format);
    let first_tree = tree(&handle, b"one\n");
    let first = handle.commit(first_tree, "first").unwrap();
    git(dir.path(), &["branch", "topic"]);
    let second_tree = tree(&handle, b"two\n");
    let second = handle.commit(second_tree, "second").unwrap();
    handle
        .repository()
        .checkout_tree(None, Some(second_tree))
        .unwrap();

    handle.switch("topic").unwrap();

    assert_eq!(
        git(dir.path(), &["symbolic-ref", "HEAD"]),
        "refs/heads/topic"
    );
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), first.to_string());
    assert_eq!(
        std::fs::read(dir.path().join("dir/file")).unwrap(),
        b"one\n"
    );
    assert!(handle.status().unwrap().staged.is_empty());
    assert_eq!(
        git(dir.path(), &["reflog", "-1", "--format=%gs", "HEAD"]),
        "checkout: moving from main to topic"
    );
    assert_eq!(git(dir.path(), &["rev-parse", "@{-1}"]), second.to_string());
    let index_before = std::fs::read(handle.repository().git_dir().join("index")).unwrap();
    std::fs::write(dir.path().join("dir/file"), b"dirty").unwrap();
    let branch = git(
        dir.path(),
        &[
            "branch",
            "--contains",
            &second.to_string(),
            "--format=%(refname:short)",
        ],
    );
    let failure = handle
        .switch(&branch)
        .unwrap_err()
        .downcast::<girt::checkout::Failure>()
        .unwrap();
    assert!(failure.report.applied.is_empty());
    assert!(!failure.report.index_published);
    assert!(failure.cleanup.is_empty());
    assert_eq!(
        std::fs::read(handle.repository().git_dir().join("index")).unwrap(),
        index_before
    );
    assert_eq!(
        git(dir.path(), &["symbolic-ref", "HEAD"]),
        "refs/heads/topic"
    );
    assert_eq!(
        std::fs::read(dir.path().join("dir/file")).unwrap(),
        b"dirty"
    );
    assert!(!dir.path().join(".git/HEAD.lock").exists());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn named_remote_fetch_and_leased_push_compose_workflow(#[case] format: ObjectFormat) {
    let (source_dir, mut source) = handle(format);
    let root_tree = tree(&source, b"remote");
    let root = source.commit(root_tree, "remote root").unwrap();
    let (local_dir, _local) = handle(format);
    git(
        local_dir.path(),
        &[
            "remote",
            "add",
            "origin",
            source_dir.path().to_str().unwrap(),
        ],
    );
    let mut local =
        Handle::new(Repository::open(local_dir.path()).unwrap()).with_environment(environment());

    local.fetch("origin").unwrap();
    let branch = git(source_dir.path(), &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(
        local
            .rev_parse(&format!("refs/remotes/origin/{branch}"))
            .unwrap(),
        root
    );
    let command = girt::push::PushCommand::new(
        girt::refs::RefName::branch("published").unwrap(),
        None,
        root,
    );
    assert!(local.push("origin", vec![command]).unwrap().all_succeeded());
    assert_eq!(
        git(source_dir.path(), &["rev-parse", "published"]),
        root.to_string()
    );
    let identity = Signature::now(b"A", b"a@example.com").unwrap();
    let child = local
        .repository()
        .loose_objects()
        .write_commit(
            &Commit::builder(root_tree, identity, b"child")
                .parents([root])
                .build()
                .unwrap(),
        )
        .unwrap();
    let stale = girt::push::PushCommand::new(
        girt::refs::RefName::branch("published").unwrap(),
        None,
        child,
    );
    let rejected = local.push("origin", vec![stale]).unwrap();
    assert!(!rejected.all_succeeded());
    assert_eq!(
        git(source_dir.path(), &["rev-parse", "published"]),
        root.to_string()
    );
}

#[test]
fn commit_and_switch_refuse_in_progress_operations_without_mutation() {
    let (dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    let root = handle.commit(tree, "root").unwrap();
    std::fs::write(
        handle.repository().git_dir().join("MERGE_HEAD"),
        format!("{root}\n"),
    )
    .unwrap();

    assert!(
        handle
            .commit(tree, "must refuse")
            .unwrap_err()
            .is::<girt::trusted::Error>()
    );
    assert!(
        handle
            .switch("topic")
            .unwrap_err()
            .is::<girt::trusted::Error>()
    );
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), root.to_string());
    assert_eq!(
        std::fs::read(handle.repository().git_dir().join("MERGE_HEAD")).unwrap(),
        format!("{root}\n").as_bytes()
    );
}

#[rstest]
#[case::empty(b"")]
#[case::dotdot(b"dir/../file")]
#[case::empty_component(b"dir//file")]
fn blob_paths_reject_invalid_components(#[case] path: &[u8]) {
    let (_dir, mut handle) = handle(ObjectFormat::Sha1);
    assert!(
        matches!(handle.blob_at("HEAD", path).unwrap_err().downcast::<girt::trusted::Error>().unwrap(), girt::trusted::Error::InvalidPath { path: actual } if actual == path)
    );
}

#[rstest]
#[case::missing(b"missing", false)]
#[case::directory(b"dir", true)]
#[case::file_as_directory(b"dir/file/sub", true)]
fn blob_paths_preserve_absence_and_kind_context(#[case] path: &[u8], #[case] wrong_kind: bool) {
    let (_dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    handle.commit(tree, "root").unwrap();
    let error = handle
        .blob_at("HEAD", path)
        .unwrap_err()
        .downcast::<girt::trusted::Error>()
        .unwrap();
    assert_eq!(
        matches!(error, girt::trusted::Error::PathKind { .. }),
        wrong_kind
    );
}

#[test]
fn fetch_refuses_missing_remote_and_url_only_remote() {
    let (dir, handle) = handle(ObjectFormat::Sha1);
    assert!(
        matches!(handle.fetch("absent").unwrap_err().downcast::<girt::trusted::Error>().unwrap(), girt::trusted::Error::RemoteNotConfigured { name } if name == "absent")
    );
    git(
        dir.path(),
        &["config", "remote.urlonly.url", "not-contacted"],
    );
    let handle = Handle::new(Repository::open(dir.path()).unwrap()).with_environment(environment());
    assert!(
        matches!(handle.fetch("urlonly").unwrap_err().downcast::<girt::trusted::Error>().unwrap(), girt::trusted::Error::NoFetchRefspecs { name } if name == "urlonly")
    );
}

#[test]
fn detached_commit_keeps_the_previous_branch_unchanged() {
    let (dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    let root = handle.commit(tree, "root").unwrap();
    git(
        dir.path(),
        &["update-ref", "--no-deref", "HEAD", &root.to_string()],
    );
    let child = handle.commit(tree, "detached").unwrap();
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), child.to_string());
    assert_eq!(git(dir.path(), &["rev-parse", "main"]), root.to_string());
    assert_eq!(
        git(dir.path(), &["reflog", "-1", "--format=%gs", "HEAD"]),
        "commit: detached"
    );
}

#[test]
fn log_honors_shallow_boundaries_and_skewed_dates() {
    let (dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    let root = handle.commit(tree, "newer parent").unwrap();
    let signature = Signature {
        name: b"A".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1_600_000_000,
        offset_minutes: 0,
    };
    let child = handle
        .repository()
        .loose_objects()
        .write_commit(
            &Commit::builder(tree, signature, b"older child")
                .parents([root])
                .build()
                .unwrap(),
        )
        .unwrap();
    let actual: Vec<_> = handle
        .log(&child.to_string())
        .unwrap()
        .map(|entry| entry.unwrap().0)
        .collect();
    assert_eq!(actual, [child, root]);
    std::fs::write(
        handle.repository().git_dir().join("shallow"),
        format!("{child}\n"),
    )
    .unwrap();
    let mut reopened = Handle::new(Repository::open(dir.path()).unwrap());
    let shallow: Vec<_> = reopened
        .log(&child.to_string())
        .unwrap()
        .map(|entry| entry.unwrap().0)
        .collect();
    assert_eq!(shallow, [child]);
}

#[test]
fn blob_reads_symlink_bytes_and_refuses_gitlinks() {
    let (_dir, mut handle) = handle(ObjectFormat::Sha1);
    let target = handle
        .repository()
        .loose_objects()
        .write_blob(b"../target")
        .unwrap();
    let mut editor = handle.tree_editor();
    editor
        .set_entry(
            b"link",
            girt::TreeValue {
                mode: girt::EntryMode::Symlink,
                id: target,
            },
        )
        .unwrap();
    editor
        .set_entry(
            b"submodule",
            girt::TreeValue {
                mode: girt::EntryMode::Gitlink,
                id: ObjectId::for_blob(ObjectFormat::Sha1, b"external"),
            },
        )
        .unwrap();
    let tree = editor.write().unwrap();
    handle.commit(tree, "modes").unwrap();
    assert_eq!(handle.blob_at("HEAD", b"link").unwrap(), b"../target");
    assert!(matches!(
        handle
            .blob_at("HEAD", b"submodule")
            .unwrap_err()
            .downcast::<girt::trusted::Error>()
            .unwrap(),
        girt::trusted::Error::PathKind {
            found: girt::ObjectKind::Commit,
            ..
        }
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn switch_from_unborn_head_materializes_target_and_previous_detached_checkout() {
    let (dir, mut handle) = handle(ObjectFormat::Sha1);
    let tree = tree(&handle, b"x");
    let signature = Signature::now(b"A", b"a@example.com").unwrap();
    let root = handle
        .repository()
        .loose_objects()
        .write_commit(&Commit::builder(tree, signature, b"root").build().unwrap())
        .unwrap();
    git(
        dir.path(),
        &["update-ref", "refs/heads/topic", &root.to_string()],
    );
    handle.switch("topic").unwrap();
    assert_eq!(std::fs::read(dir.path().join("dir/file")).unwrap(), b"x");
    git(
        dir.path(),
        &["update-ref", "--no-deref", "HEAD", &root.to_string()],
    );
    handle.switch("topic").unwrap();
    assert_eq!(git(dir.path(), &["rev-parse", "@{-1}"]), root.to_string());
    assert_eq!(
        git(dir.path(), &["reflog", "-1", "--format=%gs", "HEAD"]),
        format!("checkout: moving from {root} to topic")
    );
}
