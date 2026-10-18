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
