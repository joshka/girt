//! Original Git-generated storage fixtures; cleanup never edits their published bytes.
#![cfg(unix)]
#[path = "support/stand_in.rs"]
mod stand_in;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
static CLEANUP_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .arg("-C")
        .arg(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"));
    command
}

fn git(root: &std::path::Path, args: &[&str]) {
    let output = command(root).args(args).output().unwrap();
    assert!(output.status.success(), "{:?}", output);
}

#[cfg(unix)]
#[test]
fn cleanup_invalidates_owned_edits_and_preserves_git_targets_and_replacements() {
    let _serial = CLEANUP_TEST.lock().unwrap();
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
    assert_eq!(
        std::fs::read(repo.git_dir().join("index.lock")).unwrap(),
        b"replacement"
    );
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
    use std::os::unix::process::CommandExt as _;

    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "-q"]);
    std::fs::write(root.path().join("file"), b"git interrupt fixture").unwrap();
    git(root.path(), &["add", "file"]);
    let index = root.path().join(".git/index");
    let before = std::fs::read(&index).unwrap();
    let filter = root.path().join(".git/cleanup-filter");
    stand_in::shell(&filter, "printf ready >.git/filter-ready\nsleep 2\ncat\n");
    std::fs::write(root.path().join(".gitattributes"), b"file filter=cleanup\n").unwrap();
    std::fs::write(root.path().join("file"), b"changed fixture").unwrap();
    struct Child {
        child: std::process::Child,
        reaped: bool,
    }
    impl Child {
        fn pid(&self) -> Pid {
            Pid::from_raw(self.child.id() as i32).unwrap()
        }
    }
    impl Drop for Child {
        fn drop(&mut self) {
            if !self.reaped {
                let _ = kill_process_group(self.pid(), Signal::KILL);
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
    let filter_command = format!("'{}'", filter.to_str().unwrap().replace('\'', "'\\''"));
    let mut child = Child {
        child: command(root.path())
            .args([
                "-c",
                &format!("filter.cleanup.clean={filter_command}"),
                "add",
                "file",
            ])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        reaped: false,
    };
    // The filter starts only after Git has acquired the index lock. Its marker replaces the
    // previous assumption that a fixed sleep was enough to finish lock registration.
    let end = Instant::now() + Duration::from_secs(5);
    while !root.path().join(".git/filter-ready").exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(root.path().join(".git/filter-ready").exists());
    assert!(root.path().join(".git/index.lock").exists());
    kill_process_group(child.pid(), Signal::INT).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let exited = waitid(
            WaitId::Pid(child.pid()),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .unwrap();
        if exited.is_some() {
            break;
        }
        assert!(Instant::now() < end, "Git did not exit after interruption");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Reserve the leader PID until its owned group is cleaned up, then reap it.
    let _ = kill_process_group(child.pid(), Signal::KILL);
    assert!(!child.child.wait().unwrap().success());
    child.reaped = true;
    assert!(!root.path().join(".git/index.lock").exists());
    assert_eq!(std::fs::read(index).unwrap(), before);
}

#[test]
fn cleanup_racing_index_publication_or_drop_preserves_git_readability() {
    let _serial = CLEANUP_TEST.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "-q"]);
    std::fs::write(root.path().join("file"), b"publication race fixture").unwrap();
    git(root.path(), &["add", "file"]);
    let repo = girt::Repository::open(root.path()).unwrap();
    for publish in [true, false] {
        for _ in 0..20 {
            let edit = repo.edit_index(girt::index::Limits::default()).unwrap();
            let start = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    start.wait();
                    if publish {
                        let _ = edit.commit();
                    } else {
                        drop(edit);
                    }
                });
                start.wait();
                assert!(girt::locks::cleanup().unwrap().failures.is_empty());
            });
            assert!(!repo.git_dir().join("index.lock").exists());
            git(root.path(), &["ls-files", "--stage"]);
        }
    }
}

#[test]
fn cleanup_cancels_a_pending_packed_ref_wait_without_stealing_the_lock() {
    let _serial = CLEANUP_TEST.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--template=", "-q"]);
    let repo = girt::Repository::open(root.path()).unwrap();
    let lock = repo.git_dir().join("packed-refs.lock");
    std::fs::write(&lock, b"foreign owner").unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let references = repo.references().unwrap();
            let objects = repo.objects(girt::PackLimits::default()).unwrap();
            started.send(()).unwrap();
            references.pack(
                &objects,
                // Bounded independently if cancellation regresses.
                girt::refs::PackRefsOptions::ALL
                    .with_lock_wait(girt::refs::LockWait::For(Duration::from_secs(5))),
            )
        });
        ready.recv().unwrap();
        // Keep requesting cleanup until the worker acknowledges cancellation. This does not
        // assume it loaded the generation before the main thread was scheduled.
        let end = Instant::now() + Duration::from_secs(10);
        while !worker.is_finished() {
            assert!(girt::locks::cleanup().unwrap().failures.is_empty());
            assert!(Instant::now() < end, "packing did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        let error = worker.join().unwrap().unwrap_err();
        assert!(matches!(
            error,
            girt::refs::PackRefsError::Reference(girt::refs::ReferenceError::Cancelled)
        ));
    });
    assert_eq!(std::fs::read(lock).unwrap(), b"foreign owner");
}
