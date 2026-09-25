//! Original raw trees and blobs created only through Git's public commands.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{ObjectId, Objects, PackLimits, Repository};

pub fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub fn id(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

pub struct Fixture {
    pub root: tempfile::TempDir,
}

impl Fixture {
    pub fn new(format: &str) -> Self {
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
        Self { root }
    }

    pub fn blob(&self, bytes: &[u8]) -> ObjectId {
        id(&git(
            self.root.path(),
            &["hash-object", "-w", "--stdin"],
            bytes,
        ))
    }

    pub fn tree(&self, entries: &[(&str, ObjectId, &[u8])]) -> ObjectId {
        let mut rows = Vec::new();
        for (mode, id, path) in entries {
            let kind = if *mode == "040000" { "tree" } else { "blob" };
            rows.extend_from_slice(format!("{mode} {kind} {id}\t").as_bytes());
            rows.extend_from_slice(path);
            rows.push(0);
        }
        id(&git(
            self.root.path(),
            &["mktree", "-z", "--missing"],
            &rows,
        ))
    }

    pub fn objects(&self) -> Objects {
        Repository::open(self.root.path())
            .unwrap()
            .objects(PackLimits::default())
            .unwrap()
    }
}
