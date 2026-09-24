//! Original synthetic histories written through Git, without Git source fixtures.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{ObjectId, Repository};
fn run(directory: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Pack Author")
        .env("GIT_AUTHOR_EMAIL", "pack@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Pack Author")
        .env("GIT_COMMITTER_EMAIL", "pack@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git required for pack compatibility fixtures");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

pub fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let output = run(directory, args, input);
    assert!(
        output.status.success() || (args[0] == "merge-base" && output.status.code() == Some(1)),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub struct History {
    pub root: tempfile::TempDir,
    pub ids: Vec<ObjectId>,
}
impl History {
    pub fn new(parents: &[Vec<usize>], packed: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &["init", "--bare", "--object-format=sha1", "--template=", "."],
            b"",
        );
        let tree = git(root.path(), &["mktree"], b"");
        let tree = std::str::from_utf8(&tree).unwrap().trim();
        let mut ids = Vec::new();
        for (i, edges) in parents.iter().enumerate() {
            let mut payload = format!("tree {tree}\n");
            for &parent in edges {
                payload.push_str(&format!("parent {}\n", ids[parent]));
            }
            // Alternating times deliberately disagree with ancestry order.
            let time = if i % 2 == 0 { 1700000100 } else { 1700000000 };
            payload.push_str(&format!("author A <a@example.com> {time} +0000\ncommitter A <a@example.com> {time} +0000\n\noriginal history node {i}\n"));
            let output = git(
                root.path(),
                &["hash-object", "-t", "commit", "-w", "--stdin"],
                payload.as_bytes(),
            );
            let id: ObjectId = std::str::from_utf8(&output)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            ids.push(id);
            git(
                root.path(),
                &[
                    "update-ref",
                    &format!("refs/heads/node-{i}"),
                    &id.to_string(),
                ],
                b"",
            );
        }
        if packed {
            git(root.path(), &["repack", "-ad"], b"");
            git(root.path(), &["prune-packed"], b"");
        }
        Self { root, ids }
    }
    pub fn objects(&self) -> girt::Objects {
        Repository::open(self.root.path())
            .unwrap()
            .objects(girt::PackLimits::default())
            .unwrap()
    }
    pub fn is_ancestor(&self, left: ObjectId, right: ObjectId) -> bool {
        let output = run(
            self.root.path(),
            &[
                "merge-base",
                "--is-ancestor",
                &left.to_string(),
                &right.to_string(),
            ],
            b"",
        );
        assert!(matches!(output.status.code(), Some(0 | 1)));
        output.status.success()
    }
    pub fn query(&self, command: &str, args: &[ObjectId]) -> Vec<ObjectId> {
        let mut owned = vec![command.to_owned()];
        if command == "merge-base" {
            owned.push("--all".into());
        }
        owned.extend(args.iter().map(ToString::to_string));
        let refs: Vec<_> = owned.iter().map(String::as_str).collect();
        let output = git(self.root.path(), &refs, b"");
        let mut ids: Vec<ObjectId> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|s| s.parse().unwrap())
            .collect();
        ids.sort();
        ids
    }
}
