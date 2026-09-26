//! Public worktree administration behavior compared with Git's own reader.
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use girt::refs::{Backend, RefName};
use girt::{InitKind, ObjectFormat, Repository, WorktreeAdminError, WorktreeRetirement};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
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
#[case::sha1_files(ObjectFormat::Sha1, Backend::Files)]
#[case::sha256_reftable(ObjectFormat::Sha256, Backend::Reftable)]
fn repairs_moved_checkout_and_common_repository(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("before");
    fs::create_dir(&base).unwrap();
    let repo =
        Repository::init_with_backend(format, base.join("main"), InitKind::Worktree, backend)
            .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let linked = repo
        .create_orphan_worktree(base.join("topic"), &branch, 2)
        .unwrap();
    let name = linked.git_dir().file_name().unwrap().to_owned();
    let moved = root.path().join("after");
    fs::rename(&base, &moved).unwrap();
    let repo = Repository::open(moved.join("main")).unwrap();
    let registration = repo.common_dir().join("worktrees").join(name);
    let checkout = moved.join("topic");
    repo.repair_worktree(&registration, &checkout).unwrap();
    let reopened = Repository::open(&checkout).unwrap();
    assert_eq!(reopened.common_dir(), repo.common_dir());
    git(&checkout, &["symbolic-ref", "HEAD"]);
    git(&checkout, &["status", "--porcelain"]);
}

#[test]
fn prune_requires_expiry_absence_and_no_lock() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let linked = repo
        .create_orphan_worktree(root.path().join("topic"), &branch, 2)
        .unwrap();
    let registration = linked.git_dir().to_owned();
    let future = SystemTime::now() + Duration::from_secs(60);
    assert!(matches!(
        repo.prune_worktree(&registration, future, WorktreeRetirement::Confirmed),
        Err(WorktreeAdminError::Protected(_))
    ));
    repo.lock_worktree(&registration, "keep").unwrap();
    fs::rename(root.path().join("topic"), root.path().join("moved")).unwrap();
    assert!(matches!(
        repo.prune_worktree(&registration, future, WorktreeRetirement::Confirmed),
        Err(WorktreeAdminError::Protected(_))
    ));
    repo.unlock_worktree(&registration, "keep").unwrap();
    assert!(matches!(
        repo.prune_worktree(
            &registration,
            SystemTime::UNIX_EPOCH,
            WorktreeRetirement::Confirmed,
        ),
        Err(WorktreeAdminError::Protected(_))
    ));
    repo.prune_worktree(&registration, future, WorktreeRetirement::Confirmed)
        .unwrap();
    assert!(!registration.exists());
    assert!(root.path().join("moved").exists());
    let listed = git(
        &root.path().join("main"),
        &["worktree", "list", "--porcelain"],
    );
    assert!(!String::from_utf8_lossy(&listed).contains("topic"));
}

#[test]
fn moved_checkout_keeps_private_roots_until_explicit_retirement() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let checkout = root.path().join("topic");
    let linked = repo.create_orphan_worktree(&checkout, &branch, 2).unwrap();
    let registration = linked.git_dir().to_owned();
    let private_ref = registration.join("refs/worktree/keep");
    let private_log = registration.join("logs/HEAD");
    fs::create_dir_all(private_ref.parent().unwrap()).unwrap();
    fs::create_dir_all(private_log.parent().unwrap()).unwrap();
    fs::write(&private_ref, b"private ref\n").unwrap();
    fs::write(&private_log, b"private log\n").unwrap();
    let head = fs::read(registration.join("HEAD")).unwrap();
    let index = fs::read(registration.join("index")).unwrap();

    let moved = root.path().join("moved");
    fs::rename(&checkout, &moved).unwrap();
    assert!(!checkout.exists());
    assert!(registration.exists());
    assert_eq!(fs::read(registration.join("HEAD")).unwrap(), head);
    assert_eq!(fs::read(registration.join("index")).unwrap(), index);
    assert_eq!(fs::read(&private_ref).unwrap(), b"private ref\n");
    assert_eq!(fs::read(&private_log).unwrap(), b"private log\n");

    repo.repair_worktree(&registration, &moved).unwrap();
    assert_eq!(fs::read(&private_ref).unwrap(), b"private ref\n");
    assert_eq!(fs::read(&private_log).unwrap(), b"private log\n");
    git(&moved, &["symbolic-ref", "HEAD"]);
}

#[cfg(windows)]
struct DeniedAcl {
    path: std::path::PathBuf,
    sid: String,
}

#[cfg(windows)]
impl DeniedAcl {
    fn apply(path: &Path, permission: &str) -> Self {
        let output = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let identity = String::from_utf8_lossy(&output.stdout);
        let sid = identity
            .split('"')
            .find(|field| field.starts_with("S-1-"))
            .expect("whoami must report the runner's SID")
            .to_owned();
        let output = Command::new("icacls")
            .arg(path)
            .args(["/deny", &format!("*{sid}:{permission}")])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "icacls deny failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            path: path.to_owned(),
            sid,
        }
    }

    fn restore(mut self) {
        let output = self.remove().unwrap();
        assert!(
            output.status.success(),
            "icacls restore failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn remove(&mut self) -> std::io::Result<std::process::Output> {
        let output = Command::new("icacls")
            .arg(&self.path)
            .args(["/remove:d", &format!("*{}", self.sid)])
            .output()?;
        if output.status.success() {
            self.sid.clear();
        }
        Ok(output)
    }
}

#[cfg(windows)]
impl Drop for DeniedAcl {
    fn drop(&mut self) {
        if !self.sid.is_empty() {
            let _ = self.remove();
        }
    }
}

#[cfg(windows)]
fn denied_registration_refusal(result: &Result<(), WorktreeAdminError>) -> bool {
    match result {
        Err(WorktreeAdminError::Uncertain(path)) => path.ends_with("topic"),
        Err(WorktreeAdminError::Io {
            path,
            written,
            source,
        }) => {
            path.ends_with("girt-admin.lock")
                && written.is_empty()
                && source.kind() == std::io::ErrorKind::PermissionDenied
        }
        _ => false,
    }
}

#[cfg(windows)]
#[test]
fn denied_gitfile_is_uncertain_and_preserves_private_roots() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let checkout = root.path().join("topic");
    let linked = repo.create_orphan_worktree(&checkout, &branch, 2).unwrap();
    let registration = linked.git_dir().to_owned();
    let gitfile = checkout.join(".git");
    let head = fs::read(registration.join("HEAD")).unwrap();
    let index = fs::read(registration.join("index")).unwrap();
    let backlink = fs::read(registration.join("gitdir")).unwrap();
    let private_log = registration.join("logs/HEAD");
    fs::create_dir_all(private_log.parent().unwrap()).unwrap();
    fs::write(&private_log, b"retained log\n").unwrap();

    let denied = DeniedAcl::apply(&gitfile, "R");
    assert_eq!(
        fs::read(&gitfile).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "the runner must actually deny link reads"
    );
    let future = SystemTime::now() + Duration::from_secs(60);
    let prune_result = repo.prune_worktree(&registration, future, WorktreeRetirement::Confirmed);
    assert!(
        matches!(
            &prune_result,
            Err(WorktreeAdminError::Protected(path) | WorktreeAdminError::Uncertain(path))
                if path.ends_with(".git")
        ),
        "{prune_result:?}"
    );
    let repair_result = repo.repair_worktree(&registration, &checkout);
    assert!(
        matches!(
            &repair_result,
            Err(WorktreeAdminError::Uncertain(path)) if path.ends_with(".git")
        ),
        "{repair_result:?}"
    );
    assert_eq!(fs::read(registration.join("HEAD")).unwrap(), head);
    assert_eq!(fs::read(registration.join("index")).unwrap(), index);
    assert_eq!(fs::read(registration.join("gitdir")).unwrap(), backlink);
    assert_eq!(fs::read(&private_log).unwrap(), b"retained log\n");
    assert!(!registration.join("girt-admin.lock").exists());

    denied.restore();
    git(&checkout, &["symbolic-ref", "HEAD"]);
}

#[cfg(windows)]
#[test]
fn denied_registration_write_refuses_prune_and_repair() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let checkout = root.path().join("topic");
    let linked = repo.create_orphan_worktree(&checkout, &branch, 2).unwrap();
    let registration = linked.git_dir().to_owned();
    let head = fs::read(registration.join("HEAD")).unwrap();
    let index = fs::read(registration.join("index")).unwrap();
    let backlink = fs::read(registration.join("gitdir")).unwrap();

    let denied = DeniedAcl::apply(&registration, "W");
    let probe = registration.join("denial-probe");
    assert_eq!(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied,
        "the runner must actually deny registration writes"
    );
    let future = SystemTime::now() + Duration::from_secs(60);
    let prune_result = repo.prune_worktree(&registration, future, WorktreeRetirement::Confirmed);
    assert!(
        denied_registration_refusal(&prune_result),
        "{prune_result:?}"
    );
    let repair_result = repo.repair_worktree(&registration, &checkout);
    assert!(
        denied_registration_refusal(&repair_result),
        "{repair_result:?}"
    );
    assert_eq!(fs::read(registration.join("HEAD")).unwrap(), head);
    assert_eq!(fs::read(registration.join("index")).unwrap(), index);
    assert_eq!(fs::read(registration.join("gitdir")).unwrap(), backlink);
    assert!(!probe.exists());
    assert!(!registration.join("girt-admin.lock").exists());

    denied.restore();
    git(&checkout, &["symbolic-ref", "HEAD"]);
}

#[test]
fn repair_rewrites_relative_links_after_checkout_move() {
    let root = tempfile::tempdir().unwrap();
    let main = root.path().join("main");
    let repo = Repository::init(ObjectFormat::Sha256, &main, InitKind::Worktree).unwrap();
    let config = repo.common_dir().join("config");
    let mut bytes = fs::read(&config).unwrap();
    bytes.extend_from_slice(b"[extensions]\nrelativeWorktrees = true\n");
    fs::write(&config, bytes).unwrap();
    let repo = Repository::open(&main).unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let old = root.path().join("old");
    let linked = repo.create_orphan_worktree(&old, &branch, 2).unwrap();
    let registration = linked.git_dir().to_owned();
    let moved = root.path().join("moved");
    fs::rename(&old, &moved).unwrap();
    repo.repair_worktree(&registration, &moved).unwrap();
    let forward = fs::read(moved.join(".git")).unwrap();
    assert!(!forward.starts_with(b"gitdir: /"));
    git(&moved, &["symbolic-ref", "HEAD"]);
}

#[test]
fn stale_admin_lock_and_foreign_gitfile_refuse_mutation() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let checkout = root.path().join("topic");
    let linked = repo.create_orphan_worktree(&checkout, &branch, 2).unwrap();
    let registration = linked.git_dir();
    let guard = registration.join("girt-admin.lock");
    fs::write(&guard, b"leftover").unwrap();
    assert!(matches!(
        repo.prune_worktree(
            registration,
            SystemTime::now() + Duration::from_secs(60),
            WorktreeRetirement::Confirmed,
        ),
        Err(WorktreeAdminError::Busy(_))
    ));
    fs::remove_file(&guard).unwrap();
    let gitfile = checkout.join(".git");
    fs::write(&gitfile, b"gitdir: /unrelated/live\n").unwrap();
    assert!(matches!(
        repo.repair_worktree(registration, &checkout),
        Err(WorktreeAdminError::Protected(_))
    ));
    assert_eq!(fs::read(&gitfile).unwrap(), b"gitdir: /unrelated/live\n");
}

#[cfg(unix)]
#[test]
fn repair_permission_failure_preserves_existing_links() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("main"),
        InitKind::Worktree,
    )
    .unwrap();
    let branch = RefName::new(b"refs/heads/topic").unwrap();
    let checkout = root.path().join("topic");
    let linked = repo.create_orphan_worktree(&checkout, &branch, 2).unwrap();
    let registration = linked.git_dir();
    let forward = fs::read(checkout.join(".git")).unwrap();
    let backlink = fs::read(registration.join("gitdir")).unwrap();
    fs::set_permissions(registration, fs::Permissions::from_mode(0o500)).unwrap();
    let result = repo.repair_worktree(registration, &checkout);
    fs::set_permissions(registration, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(WorktreeAdminError::Io { .. })));
    assert_eq!(fs::read(checkout.join(".git")).unwrap(), forward);
    assert_eq!(fs::read(registration.join("gitdir")).unwrap(), backlink);
}
