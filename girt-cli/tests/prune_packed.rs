//! CLI isolation assertion and Git-output behavior for verified loose-duplicate pruning.
use std::process::Command;

use girt::{InitKind, ObjectFormat, Repository};

#[path = "../../tests/support/maintenance_git.rs"]
mod oracle;

#[test]
fn requires_isolation_assertion_and_supports_silent_git_output() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare).unwrap();
    let id = repo.loose_objects().write_blob(b"packed survivor").unwrap();
    oracle::git(
        repo.git_dir(),
        &["pack-objects", "objects/pack/pack"],
        format!("{id}\n").as_bytes(),
    );
    let hex = id.to_string();
    let loose = repo.object_dir().join(&hex[..2]).join(&hex[2..]);
    let command = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_girt"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(repo.git_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
            .args(args)
            .output()
            .unwrap()
    };
    let refused = command(&["prune-packed"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(loose.exists());
    let limited = command(&["prune-packed", "--yes", "--max-entries", "0"]);
    assert!(!limited.status.success());
    assert!(
        String::from_utf8_lossy(&limited.stderr).contains("deleted 0; uncertain loose object none")
    );
    assert!(loose.exists());
    let output = command(&["--git", "prune-packed", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(!loose.exists());
    oracle::git(repo.git_dir(), &["fsck", "--full"], b"");
}
