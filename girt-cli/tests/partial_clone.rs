//! Object reads in a Git-made partial clone fetch missing objects, as Git's lazy fetch does.
#![cfg(unix)]
#[path = "../../tests/support/apply_git.rs"]
mod support;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn run(program: &str, dir: &Path, args: &[&str], env: &[(&str, &str)], input: &[u8]) -> Output {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-config"))
        .envs(env.iter().copied())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input).unwrap();
    child.wait_with_output().unwrap()
}

fn girt(dir: &Path, args: &[&str], env: &[(&str, &str)], input: &[u8]) -> Output {
    run(env!("CARGO_BIN_EXE_girt"), dir, args, env, input)
}

/// A source with two files and two blob:none clones of it, one for Git and one for girt.
fn clones() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    support::git(&source, &["init", "-q", "--template=", "-b", "main"], b"");
    support::git(&source, &["config", "uploadpack.allowFilter", "true"], b"");
    std::fs::write(source.join("a"), "a\n").unwrap();
    std::fs::write(source.join("b"), "b\n").unwrap();
    support::git(&source, &["add", "."], b"");
    support::git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "one"],
        b"",
    );
    let url = format!("file://{}", source.display());
    let mut paths = ["git", "girt"].map(|name| root.path().join(name));
    for path in &mut paths {
        support::git(
            root.path(),
            &[
                "clone",
                "-q",
                "--template=",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                path.to_str().unwrap(),
            ],
            b"",
        );
    }
    let [git, girt] = paths;
    (root, git, girt)
}

fn promisor_packs(clone: &Path) -> usize {
    std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "promisor")
        })
        .count()
}

#[test]
fn reads_fetch_missing_objects_like_git() {
    let (_root, git_clone, girt_clone) = clones();
    let offline = [("GIT_NO_LAZY_FETCH", "1")];

    // With lazy fetching off, both report the blob as missing and fetch nothing.
    let git = run(
        "git",
        &git_clone,
        &["cat-file", "-e", "HEAD:a"],
        &offline,
        b"",
    );
    let girt_exists = girt(&girt_clone, &["object", "exists", "HEAD:a"], &offline, b"");
    assert!(!git.status.success());
    assert!(!girt_exists.status.success());
    assert!(
        !girt(&girt_clone, &["object", "cat", "HEAD:a"], &offline, b"")
            .status
            .success()
    );
    assert_eq!(promisor_packs(&girt_clone), 1);

    // Otherwise the read fetches the blob, stores it in a promisor pack and prints it.
    let git = run("git", &git_clone, &["cat-file", "-p", "HEAD:a"], &[], b"");
    let shown = girt(&girt_clone, &["object", "show", "HEAD:a"], &[], b"");
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(shown.stdout, git.stdout);
    assert_eq!(promisor_packs(&girt_clone), promisor_packs(&git_clone));
    let stored = run(
        "git",
        &girt_clone,
        &["cat-file", "-e", "HEAD:a"],
        &offline,
        b"",
    );
    assert!(stored.status.success());

    // Batch reads fetch each missing object too.
    let git = run(
        "git",
        &git_clone,
        &["cat-file", "--batch-check"],
        &[],
        b"HEAD:b\n",
    );
    let batch = girt(
        &girt_clone,
        &["object", "batch", "--check"],
        &[],
        b"HEAD:b\n",
    );
    assert_eq!(batch.stdout, git.stdout);
    let fsck = run("git", &girt_clone, &["fsck", "--strict"], &offline, b"");
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

#[test]
fn invalid_no_lazy_fetch_value_is_an_error() {
    let (_root, _, girt_clone) = clones();
    let bad = [("GIT_NO_LAZY_FETCH", "banana")];
    let output = girt(&girt_clone, &["object", "cat", "HEAD:a"], &bad, b"");
    assert!(!output.status.success());
    // Leading whitespace is allowed, as with `strtol` in Git.
    let spaced = [("GIT_NO_LAZY_FETCH", " 1")];
    let output = girt(&girt_clone, &["object", "exists", "HEAD:a"], &spaced, b"");
    assert_eq!(output.status.code(), Some(1));
    let out_of_range = [("GIT_NO_LAZY_FETCH", "2147483648")];
    let output = girt(
        &girt_clone,
        &["object", "exists", "HEAD:a"],
        &out_of_range,
        b"",
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("bad boolean"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("GIT_NO_LAZY_FETCH"));
    assert_eq!(promisor_packs(&girt_clone), 1);
}

/// A replace reference whose replacement is promised but absent fetches the replacement, as
/// Git's lazy fetch does, and prints its content.
#[test]
fn missing_replacement_is_fetched() {
    let (_root, git_clone, girt_clone) = clones();
    let offline = [("GIT_NO_LAZY_FETCH", "1")];
    for clone in [&git_clone, &girt_clone] {
        let rev = |path: &str| {
            let output = run("git", clone, &["rev-parse", path], &offline, b"");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let (original, replacement) = (rev("HEAD:a"), rev("HEAD:b"));
        // `git update-ref` refuses a missing target, so write the loose reference directly.
        let replace = clone.join(".git/refs/replace");
        std::fs::create_dir_all(&replace).unwrap();
        std::fs::write(replace.join(&original), format!("{replacement}\n")).unwrap();
    }

    let offline_read = girt(&girt_clone, &["object", "cat", "HEAD:a"], &offline, b"");
    assert!(!offline_read.status.success());
    let git = run("git", &git_clone, &["cat-file", "-p", "HEAD:a"], &[], b"");
    assert_eq!(git.stdout, b"b\n");
    let shown = girt(&girt_clone, &["object", "cat", "HEAD:a"], &[], b"");
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert_eq!(shown.stdout, git.stdout);
    assert_eq!(promisor_packs(&girt_clone), 2);
}
