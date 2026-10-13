//! Original Git-generated storage fixtures; cleanup never edits their published bytes.
#![cfg(unix)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn git(root: &std::path::Path, args: &[&str]) {
    let output = Command::new("git").arg("-C").arg(root).args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .output().unwrap();
    assert!(output.status.success(), "{:?}", output);
}

#[cfg(unix)]
#[test]
fn cleanup_invalidates_owned_edits_and_preserves_git_targets_and_replacements() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "-q"]);
    std::fs::write(root.path().join("file"), b"original fixture").unwrap();
    git(root.path(), &["add", "file"]);
    let repo = girt::Repository::open(root.path()).unwrap();
    let index_path = repo.git_dir().join("index");
    let config_path = repo.git_dir().join("config");
    let index = std::fs::read(&index_path).unwrap();
    let config = std::fs::read(&config_path).unwrap();
    let edit = repo.edit_index(girt::index::Limits::default()).unwrap();
    let config_edit = repo.edit_config(1024 * 1024).unwrap();
    let foreign = repo.git_dir().join("foreign.lock");
    std::fs::write(&foreign, b"foreign").unwrap();
    let report = girt::locks::cleanup().unwrap();
    assert!(report.failures.is_empty());
    assert_eq!(report.removed.len(), 2);
    assert!(!repo.git_dir().join("index.lock").exists());
    assert!(!repo.git_dir().join("config.lock").exists());
    assert!(girt::locks::cleanup().unwrap().removed.is_empty());
    std::fs::write(repo.git_dir().join("index.lock"), b"replacement").unwrap();
    assert!(edit.commit().is_err());
    assert!(config_edit.commit().is_err());
    assert_eq!(std::fs::read(repo.git_dir().join("index.lock")).unwrap(), b"replacement");
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(std::fs::read(index_path).unwrap(), index);
    assert_eq!(std::fs::read(config_path).unwrap(), config);
    git(root.path(), &["ls-files", "--stage"]);
    git(root.path(), &["fsck", "--strict"]);

    std::fs::remove_file(repo.git_dir().join("index.lock")).unwrap();
    let edit = repo.edit_index(girt::index::Limits::default()).unwrap();
    let lock = repo.git_dir().join("index.lock");
    std::fs::rename(&lock, repo.git_dir().join("saved-owned-lock")).unwrap();
    std::fs::write(&lock, b"foreign replacement").unwrap();
    let report = girt::locks::cleanup().unwrap();
    assert_eq!(report.preserved, vec![lock.clone()]);
    drop(edit);
    assert_eq!(std::fs::read(lock).unwrap(), b"foreign replacement");
}

#[cfg(unix)]
#[test]
fn git_interrupt_releases_index_lock_without_changing_published_index() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "-q"]);
    std::fs::write(root.path().join("file"), b"git interrupt fixture").unwrap();
    git(root.path(), &["add", "file"]);
    let index = root.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    let mut child = Child(Command::new("git").arg("-C").arg(root.path())
        .args(["update-index", "--index-info"]).stdin(Stdio::piped())
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let end = Instant::now() + Duration::from_secs(5);
    while !root.path().join(".git/index.lock").exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(root.path().join(".git/index.lock").exists());
    let pid = rustix::process::Pid::from_raw(child.0.id() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    assert!(!child.0.wait().unwrap().success());
    assert!(!root.path().join(".git/index.lock").exists());
    assert_eq!(std::fs::read(index).unwrap(), before);
}
