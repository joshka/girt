//! Git merge oracle with no inherited Git environment or global/system configuration.
use std::path::Path;
use std::process::{Command, Output};

pub fn merge_tree(root: &Path, ours: &str, theirs: &str) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("LC_ALL", "C")
        .args(["merge-tree", "--write-tree", ours, theirs])
        .output()
        .unwrap()
}

pub fn commit(root: &Path, tree: girt::ObjectId, parent: Option<girt::ObjectId>) -> girt::ObjectId {
    commit_parents(root, tree, &parent.into_iter().collect::<Vec<_>>())
}

pub fn commit_parents(
    root: &Path,
    tree: girt::ObjectId,
    parents: &[girt::ObjectId],
) -> girt::ObjectId {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .args([
            "-c",
            "user.name=Merge Author",
            "-c",
            "user.email=merge@example.com",
            "commit-tree",
            &tree.to_string(),
            "-m",
            "fixture",
        ]);
    for parent in parents {
        command.args(["-p", &parent.to_string()]);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
