//! No-op retirement over protected primary packs, generated and checked with Git.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::retention::{MaintenanceIsolation, RepackLimits, RetentionPolicy};

struct Disposable;
impl MaintenanceIsolation for Disposable {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn retirement_with_only_protected_packed_objects_is_read_only(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
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
    let blob = git(root.path(), &["hash-object", "-w", "--stdin"], b"survivor");
    git(root.path(), &["update-ref", "refs/tags/blob", &blob], b"");
    git(root.path(), &["repack", "-ad"], b"");
    git(root.path(), &["prune-packed"], b"");
    let directory = root.path().join("objects/pack");
    let pack = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .unwrap();
    let original = fs::read(&pack).unwrap();
    let original_index = fs::read(pack.with_extension("idx")).unwrap();
    let original_count = fs::read_dir(&directory).unwrap().count();
    let repo = Repository::open(root.path()).unwrap();
    let report = repo
        .retire_old_packs(
            &mut Disposable,
            &RetentionPolicy::default(),
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.published.is_none());
    assert!(!report.durable);
    assert!(report.removed.is_empty());
    assert_eq!(fs::read_dir(directory).unwrap().count(), original_count);
    assert_eq!(
        fs::read(pack.with_extension("idx")).unwrap(),
        original_index
    );
    assert_eq!(fs::read(pack).unwrap(), original);
    assert_eq!(
        git(root.path(), &["cat-file", "-p", &blob], b""),
        "survivor"
    );
    git(root.path(), &["fsck", "--strict"], b"");
}
