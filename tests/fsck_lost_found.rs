//! Recovery files compared with isolated Git on original dangling object fixtures.
#[path = "support/apply_git.rs"]
mod support;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use girt::{ObjectFormat, Repository};

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for category in ["commit", "other"] {
        if let Ok(entries) = std::fs::read_dir(root.join(category)) {
            for entry in entries {
                let path = entry.unwrap().path();
                result.insert(
                    PathBuf::from(category).join(path.file_name().unwrap()),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    result
}

#[test]
fn recovery_files_match_git_and_repeat_without_overwriting() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let directory = tempfile::tempdir().unwrap();
        support::git(
            directory.path(),
            &[
                "init",
                "--bare",
                "--template=",
                &format!("--object-format={format}"),
                ".",
            ],
            b"",
        );
        support::git(
            directory.path(),
            &["hash-object", "-w", "--stdin"],
            b"recover this blob\0exactly\n",
        );
        let tree = support::git(directory.path(), &["mktree"], b"");
        support::git(
            directory.path(),
            &["commit-tree", std::str::from_utf8(&tree).unwrap().trim()],
            b"dangling commit\n",
        );
        let repo = Repository::open(directory.path()).unwrap();
        let report = repo.fsck().unwrap();

        let written = repo.write_lost_found(&report).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(repo.write_lost_found(&report).unwrap(), written);
        let root = repo.common_dir().join("lost-found");
        let expected = files(&root);
        std::fs::remove_dir_all(&root).unwrap();
        support::git(directory.path(), &["fsck", "--lost-found"], b"");

        assert_eq!(files(&root), expected);
    }
}

#[test]
fn conflicting_recovery_files_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let id = repo.loose_objects().write_blob(b"recover").unwrap();
    let report = repo.fsck().unwrap();
    let root = repo.common_dir().join("lost-found/other");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(id.to_string());
    std::fs::write(&path, b"unrelated").unwrap();

    let error = repo.write_lost_found(&report).unwrap_err();

    assert!(matches!(error.cause, girt::fsck::LostFoundCause::Path(_)));
    assert!(error.written.is_empty());
    assert_eq!(std::fs::read(path).unwrap(), b"unrelated");
}

#[cfg(unix)]
#[test]
fn symlinked_recovery_directories_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo"),
        girt::InitKind::Bare,
    )
    .unwrap();
    repo.loose_objects().write_blob(b"recover").unwrap();
    let report = repo.fsck().unwrap();
    let outside = directory.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.common_dir().join("lost-found")).unwrap();

    assert!(repo.write_lost_found(&report).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn partial_failures_report_completed_paths_and_limits_cancel_before_output() {
    use std::sync::atomic::AtomicBool;

    use girt::fsck::{LostFoundCause, LostFoundLimits};
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let first = repo
        .loose_objects()
        .write_blob(b"first recovery payload")
        .unwrap();
    let second = repo
        .loose_objects()
        .write_blob(b"second recovery payload")
        .unwrap();
    let report = repo.fsck().unwrap();
    let mut limited = LostFoundLimits::default();
    limited.max_objects = 0;
    assert!(matches!(
        repo.write_lost_found_with(&report, limited, &AtomicBool::new(false))
            .unwrap_err()
            .cause,
        LostFoundCause::Limit
    ));
    assert!(matches!(
        repo.write_lost_found_with(&report, Default::default(), &AtomicBool::new(true))
            .unwrap_err()
            .cause,
        LostFoundCause::Cancelled
    ));
    assert!(!repo.common_dir().join("lost-found").exists());
    let (low, high) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    let root = repo.common_dir().join("lost-found/other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(high.to_string()), b"conflict").unwrap();

    let error = repo.write_lost_found(&report).unwrap_err();

    assert!(matches!(error.cause, LostFoundCause::Path(_)));
    assert_eq!(error.written, [root.join(low.to_string())]);
    assert_eq!(
        std::fs::read(root.join(high.to_string())).unwrap(),
        b"conflict"
    );
}
