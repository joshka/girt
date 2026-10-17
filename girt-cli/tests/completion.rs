//! Dynamic completion (`COMPLETE=<shell> girt`) reads refs, remotes and config from the repository.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Asks girt for fish-format candidates (`value[\thelp]` per line) for the last word of `line`,
/// leaving out options.
fn complete(dir: &Path, line: &[&str]) -> Vec<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_girt"))
        .arg("--")
        .args(line)
        .current_dir(dir)
        .env("COMPLETE", "fish")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with('-'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn completes_refs_remotes_and_config_keys() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(dir, &["branch", "feature"]);
    git(dir, &["tag", "v1"]);
    git(
        dir,
        &["remote", "add", "upstream", "https://example.com/r.git"],
    );
    git(dir, &["update-ref", "refs/remotes/upstream/main", "HEAD"]);

    assert_eq!(
        complete(dir, &["girt", "log", ""]),
        [
            "HEAD",
            "feature\tbranch",
            "main\tbranch",
            "upstream/main\tremote branch",
            "v1\ttag"
        ]
    );
    assert_eq!(complete(dir, &["girt", "diff", "fe"]), ["feature\tbranch"]);
    assert_eq!(
        complete(dir, &["girt", "ref", "show", "refs/t"]),
        ["refs/tags/v1"]
    );
    assert_eq!(complete(dir, &["girt", "fetch", ""]), ["upstream"]);
    assert!(
        complete(dir, &["girt", "config", "get", "remote."])
            .contains(&"remote.upstream.url".to_owned())
    );
}

#[test]
fn completes_nothing_outside_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        complete(dir.path(), &["girt", "fetch", ""]),
        Vec::<String>::new()
    );
}
