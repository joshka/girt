//! Git invocations for rerere compatibility tests.
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Runs Git with isolated configuration, fixed identities and dates.
pub fn run(directory: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Rerere Author")
        .env("GIT_AUTHOR_EMAIL", "rerere@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Rerere Author")
        .env("GIT_COMMITTER_EMAIL", "rerere@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("Git required for rerere compatibility tests")
}

/// Runs Git and returns trimmed standard output, failing on a nonzero exit.
pub fn git(directory: &Path, args: &[&str]) -> String {
    let output = run(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
