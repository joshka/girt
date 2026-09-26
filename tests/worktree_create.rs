//! Original linked-worktree fixtures checked with Git's public commands.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::refs::{Backend, RefName};
use girt::{CreateWorktreeError, InitKind, ObjectFormat, Repository, WorktreeState};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[rstest]
#[case::sha1_files(ObjectFormat::Sha1, Backend::Files, false)]
#[case::sha256_files(ObjectFormat::Sha256, Backend::Files, false)]
#[case::sha1_reftable(ObjectFormat::Sha1, Backend::Reftable, false)]
#[case::sha256_reftable(ObjectFormat::Sha256, Backend::Reftable, false)]
#[case::relative_files(ObjectFormat::Sha1, Backend::Files, true)]
#[case::relative_reftable(ObjectFormat::Sha256, Backend::Reftable, true)]
fn creates_git_usable_orphan(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
    #[case] relative: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let main = root.path().join("main");
    let mut repo =
        Repository::init_with_backend(format, &main, InitKind::Worktree, backend).unwrap();
    if relative {
        let config = repo.common_dir().join("config");
        let mut bytes = fs::read(&config).unwrap();
        if format == ObjectFormat::Sha1 && backend == Backend::Files {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replace("repositoryformatversion = 0", "repositoryformatversion = 1")
                .into_bytes();
        }
        bytes.extend_from_slice(b"[extensions]\n\trelativeWorktrees = true\n");
        fs::write(config, bytes).unwrap();
        repo = Repository::open(&main).unwrap();
    }
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let destination = root.path().join("topic");
    let linked = repo
        .create_orphan_worktree(&destination, &branch, 2)
        .unwrap();
    assert_eq!(linked.common_dir(), repo.common_dir());
    assert_ne!(linked.git_dir(), repo.git_dir());
    assert!(linked.git_dir().join("index").is_file());
    assert_eq!(
        git(&destination, &["symbolic-ref", "HEAD"]),
        b"refs/heads/topic\n"
    );
    assert!(git(&destination, &["ls-files"]).is_empty());
    git(&destination, &["status", "--porcelain"]);
    git(
        &destination,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    assert!(!git(&destination, &["rev-parse", "HEAD"]).is_empty());
    assert!(matches!(
        repo.create_orphan_worktree(root.path().join("later"), &branch, 2),
        Err(CreateWorktreeError::ExistingBranch)
    ));
    git(&destination, &["fsck", "--no-reflogs"]);
    assert_eq!(git(&main, &["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
    git(&main, &["worktree", "list", "--porcelain"]);
    let entries = repo.worktrees(2, &AtomicBool::new(false)).unwrap();
    assert!(matches!(entries[0].state, WorktreeState::Available));
    let forward = fs::read(destination.join(".git")).unwrap();
    if relative {
        assert!(!forward.starts_with(b"gitdir: /"));
    } else {
        #[cfg(unix)]
        assert!(forward.starts_with(b"gitdir: /"));
    }
}

#[test]
fn preflight_refusals_leave_destination_absent() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let destination = root.path().join("topic");
    let head = RefName::new(b"HEAD").unwrap();
    assert!(matches!(
        repo.create_orphan_worktree(&destination, &head, 2),
        Err(CreateWorktreeError::Branch)
    ));
    assert!(!destination.exists());
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    assert!(matches!(
        repo.create_orphan_worktree(&destination, &branch, 0),
        Err(CreateWorktreeError::Limit)
    ));
    assert!(!destination.exists());
}

#[test]
fn unborn_branch_can_be_shared_and_registration_names_can_collide() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    fs::create_dir(root.path().join("other")).unwrap();
    let first = root.path().join("topic");
    let first_branch = RefName::new(b"refs/heads/topic").unwrap();
    repo.create_orphan_worktree(&first, &first_branch, 4)
        .unwrap();
    let second = root.path().join("other/topic");
    let linked = repo
        .create_orphan_worktree(&second, &first_branch, 4)
        .unwrap();
    assert_eq!(linked.git_dir().file_name().unwrap(), "topic1");
    assert_eq!(
        git(&second, &["symbolic-ref", "HEAD"]),
        b"refs/heads/topic\n"
    );
}

#[cfg(unix)]
#[test]
fn destination_creation_failure_retains_registration_for_inspection() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let blocked = root.path().join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let result = repo.create_orphan_worktree(blocked.join("topic"), &branch, 2);
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    let registration = match result {
        Err(CreateWorktreeError::Io {
            registration: Some(path),
            ..
        }) => path,
        other => panic!("expected partial registration: {other:?}"),
    };
    assert!(registration.join("HEAD").is_file());
    assert!(!blocked.join("topic").exists());
    assert!(matches!(
        repo.worktrees(2, &AtomicBool::new(false)).unwrap()[0].state,
        WorktreeState::Missing
    ));
}

#[cfg(unix)]
#[test]
fn line_break_path_is_refused_before_registration() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let destination = root.path().join(OsStr::from_bytes(b"bad\nname"));
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    assert!(matches!(
        repo.create_orphan_worktree(&destination, &branch, 2),
        Err(CreateWorktreeError::Path(_))
    ));
    assert!(!destination.exists());
    assert!(!repo.common_dir().join("worktrees").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn native_byte_path_is_preserved_in_links() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let destination = root.path().join(OsStr::from_bytes(b"topic-\xff"));
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let linked = repo
        .create_orphan_worktree(&destination, &branch, 2)
        .unwrap();
    assert_eq!(linked.worktree(), Some(destination.as_path()));
    assert_eq!(
        git(&destination, &["symbolic-ref", "HEAD"]),
        b"refs/heads/topic\n"
    );
}
