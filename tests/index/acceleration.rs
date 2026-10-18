//! Original fixtures generated through an isolated Git executable.
#[path = "../support/layout_git.rs"]
mod oracle;

use girt::index::Limits;
use girt::{InitKind, ObjectFormat, Repository};
use rstest::rstest;

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn reads_git_untracked_cache_and_retains_its_bytes(#[case] format: ObjectFormat) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("tracked"), b"tracked\n").unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/new"), b"new\n").unwrap();
    std::fs::write(root.join(".gitignore"), b"*.ignored\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    oracle::git(root, &["update-index", "--untracked-cache"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");

    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let cache = index.untracked_cache(Limits::default()).unwrap().unwrap();
    assert!(cache.ident().ends_with(b"\0"));
    assert_eq!(cache.exclude_name(), b".gitignore");
    assert_eq!(cache.directories()[0].name(), b"");
    assert!(cache.directories()[0].valid());
    assert!(cache.directories()[0].stat().is_some());
    assert_eq!(
        cache.directories()[0].exclude(),
        // Git hashes its ignore buffer with an additional newline, even when the file ends in one.
        Some(girt::ObjectId::for_blob(format, b"*.ignored\n\n"))
    );
    assert!(
        cache
            .directories()
            .iter()
            .any(|dir| dir.name() == b"nested")
    );
    let extension = index
        .extensions()
        .iter()
        .find(|extension| extension.signature() == *b"UNTR")
        .unwrap();
    assert_eq!(cache.encode(Limits::default()).unwrap(), extension.data());
    for end in 0..extension.data().len() {
        assert!(
            girt::index::UntrackedCache::parse(format, &extension.data()[..end], Limits::default())
                .is_err(),
            "accepted truncated length {end}"
        );
    }
}

#[cfg(unix)]
#[path = "../support/stand_in.rs"]
mod stand_in;

#[cfg(unix)]
#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn reads_git_hook_monitor_checkpoint(#[case] format: ObjectFormat) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("tracked"), b"tracked\n").unwrap();
    oracle::git(root, &["add", "tracked"], b"");
    let hook = temporary.path().join("monitor");
    stand_in::shell(&hook, "printf 'checkpoint\\000tracked\\000'\n");
    oracle::git(
        root,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
        b"",
    );
    oracle::git(root, &["config", "core.fsmonitorHookVersion", "2"], b"");
    oracle::git(root, &["status", "--porcelain"], b"");

    let index = repo.read_index(Limits::default()).unwrap().unwrap();
    let cache = index.fsmonitor_cache(Limits::default()).unwrap().unwrap();
    assert_eq!(
        cache.token(),
        &girt::index::MonitorToken::Opaque(b"checkpoint".to_vec())
    );
    assert!(cache.dirty().is_empty());
    oracle::git(root, &["update-index", "--split-index"], b"");
    oracle::git(
        root,
        &["update-index", "--no-fsmonitor-valid", "tracked"],
        b"",
    );
    let resolved = repo.read_index(Limits::default()).unwrap().unwrap();
    assert_eq!(
        resolved
            .fsmonitor_cache(Limits::default())
            .unwrap()
            .unwrap()
            .dirty(),
        [0]
    );
    let extension = index
        .extensions()
        .iter()
        .find(|extension| extension.signature() == *b"FSMN")
        .unwrap();
    assert_eq!(cache.encode(Limits::default()).unwrap(), extension.data());
}
