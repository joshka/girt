//! Original CLI fixtures with isolated configuration and fixed operation identities.
use std::path::Path;
use std::process::{Command, Output};

use girt::{ObjectId, Signature};

pub fn git(root: &Path, args: &[&str]) -> Output {
    git_extra(root, args, &[])
}

pub fn git_extra(root: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Original Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Replay Writer")
        .env("GIT_COMMITTER_EMAIL", "writer@example.com")
        .env("GIT_COMMITTER_DATE", "1700000100 +0000")
        .envs(extra.iter().copied())
        .output()
        .unwrap()
}
pub fn success(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = git(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
pub fn head(root: &Path) -> ObjectId {
    std::str::from_utf8(&success(root, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
pub fn writer() -> Signature {
    Signature {
        name: b"Replay Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_100,
        offset_minutes: 0,
    }
}
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub base: ObjectId,
    pub source: ObjectId,
    pub onto: ObjectId,
}
impl Fixture {
    pub fn new(format: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        success(
            path,
            &[
                "init",
                "--template=",
                &format!("--object-format={format}"),
                ".",
            ],
        );
        std::fs::write(path.join("file"), b"one\ntwo\nthree\n").unwrap();
        success(path, &["add", "."]);
        success(path, &["commit", "-m", "base"]);
        let base = head(path);
        std::fs::write(path.join("file"), b"ONE\ntwo\nthree\n").unwrap();
        success(path, &["commit", "-am", "source"]);
        let source = head(path);
        success(path, &["checkout", "--detach", &base.to_string()]);
        std::fs::write(path.join("other"), b"destination\n").unwrap();
        success(path, &["add", "."]);
        success(path, &["commit", "-m", "destination"]);
        let onto = head(path);
        Self {
            root,
            base,
            source,
            onto,
        }
    }
}

pub fn conflict_destination(fixture: &Fixture) -> ObjectId {
    std::fs::write(
        fixture.root.path().join("file"),
        b"CONFLICTING\ntwo\nthree\n",
    )
    .unwrap();
    success(
        fixture.root.path(),
        &["commit", "-am", "conflicting destination"],
    );
    head(fixture.root.path())
}
