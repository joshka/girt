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

/// A source served by `git upload-pack` through an SSH command that runs it locally.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct SshSource {
    root: tempfile::TempDir,
    url: String,
    ssh: String,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl SshSource {
    /// `main` has files `a` and `b`; an older commit had a different `a`.
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        support::git(&source, &["init", "-q", "--template=", "-b", "main"], b"");
        support::git(&source, &["config", "uploadpack.allowFilter", "true"], b"");
        for (a, message) in [("old\n", "one"), ("a\n", "two")] {
            std::fs::write(source.join("a"), a).unwrap();
            std::fs::write(source.join("b"), "b\n").unwrap();
            support::git(&source, &["add", "."], b"");
            support::git(
                &source,
                &["commit", "-q", "--no-gpg-sign", "-m", message],
                b"",
            );
        }
        // The last argument is the remote command, such as `git-upload-pack '/path'`.
        let ssh = root.path().join("ssh");
        std::fs::write(
            &ssh,
            "#!/bin/sh\nfor arg; do last=$arg; done\nexec sh -c \"$last\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            url: format!("localhost:{}", source.display()),
            ssh: ssh.to_str().unwrap().to_owned(),
            root,
        }
    }

    fn source(&self) -> PathBuf {
        self.root.path().join("source")
    }

    fn girt(&self, dir: &Path, args: &[&str]) -> Output {
        let output = girt(dir, args, &[("GIT_SSH_COMMAND", &self.ssh)], b"");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// `git rev-list --missing=print` IDs, without lazy fetching.
    fn missing(&self, clone: &Path) -> Vec<String> {
        let output = run(
            "git",
            clone,
            &["rev-list", "--objects", "--all", "--missing=print"],
            &[("GIT_NO_LAZY_FETCH", "1")],
            b"",
        );
        let mut missing: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix('?').map(str::to_owned))
            .collect();
        missing.sort();
        missing
    }

    fn rev(&self, revision: &str) -> String {
        let output = run("git", &self.source(), &["rev-parse", revision], &[], b"");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}

/// `girt clone --filter=blob:none` leaves the same objects missing as `git clone --filter`, with
/// the checked-out files present and a clean status and fsck.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn blobless_clone_matches_git() {
    let source = SshSource::new();
    let root = source.root.path();
    support::git(
        root,
        &[
            "clone",
            "-q",
            "--template=",
            "--filter=blob:none",
            &format!("file://{}", source.source().display()),
            "git",
        ],
        b"",
    );
    source.girt(root, &["clone", "--filter=blob:none", &source.url, "girt"]);
    let (git_clone, girt_clone) = (root.join("git"), root.join("girt"));
    let old = source.rev("HEAD~:a");
    assert_eq!(source.missing(&girt_clone), std::slice::from_ref(&old));
    assert_eq!(source.missing(&girt_clone), source.missing(&git_clone));
    assert_eq!(promisor_packs(&girt_clone), promisor_packs(&git_clone));
    let offline = [("GIT_NO_LAZY_FETCH", "1")];
    for args in [&["status", "--porcelain"][..], &["fsck", "--strict"]] {
        let output = run("git", &girt_clone, args, &offline, b"");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    for name in [
        "remote.origin.promisor",
        "remote.origin.partialclonefilter",
        "core.repositoryformatversion",
    ] {
        let read = |clone: &Path| run("git", clone, &["config", "get", name], &[], b"").stdout;
        assert_eq!(read(&girt_clone), read(&git_clone), "{name}");
    }

    // `fetch --no-filter` fetches whole objects but keeps the promisor configuration.
    std::fs::write(source.source().join("c"), "c\n").unwrap();
    support::git(&source.source(), &["add", "c"], b"");
    support::git(
        &source.source(),
        &["commit", "-q", "--no-gpg-sign", "-m", "three"],
        b"",
    );
    source.girt(&girt_clone, &["fetch", "--no-filter"]);
    assert_eq!(source.missing(&girt_clone), [old]);
    let filter = run(
        "git",
        &girt_clone,
        &["config", "get", "remote.origin.partialclonefilter"],
        &[],
        b"",
    );
    assert_eq!(filter.stdout, b"blob:none\n");
}

/// `--no-checkout` fetches no blobs; a server without filter support sends everything with
/// Git's warning.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn blobless_clone_without_checkout_or_server_support() {
    let source = SshSource::new();
    let root = source.root.path();
    source.girt(
        root,
        &[
            "clone",
            "--no-checkout",
            "--filter=blob:none",
            &source.url,
            "bare",
        ],
    );
    let mut blobs = vec![
        source.rev("HEAD~:a"),
        source.rev("HEAD:a"),
        source.rev("HEAD:b"),
    ];
    blobs.sort();
    assert_eq!(source.missing(&root.join("bare")), blobs);
    assert_eq!(promisor_packs(&root.join("bare")), 1);

    support::git(
        &source.source(),
        &["config", "uploadpack.allowFilter", "false"],
        b"",
    );
    let output = source.girt(root, &["clone", "--filter=blob:none", &source.url, "full"]);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("warning: filtering not recognized by server, ignoring")
    );
    assert!(source.missing(&root.join("full")).is_empty());
}

/// Plain paths ignore `--filter` with Git's warning; `--no-filter` cancels an earlier `--filter`.
#[test]
fn local_clone_ignores_filter() {
    let (root, _, _) = clones();
    let source = root.path().join("source");
    let output = girt(
        root.path(),
        &[
            "clone",
            "--filter=blob:none",
            source.to_str().unwrap(),
            "plain",
        ],
        &[],
        b"",
    );
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("warning: --filter is ignored in local clones; use file:// instead.")
    );
    let output = girt(
        root.path(),
        &[
            "clone",
            "--filter=blob:none",
            "--no-filter",
            source.to_str().unwrap(),
            "unfiltered",
        ],
        &[],
        b"",
    );
    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let promisor = run(
        "git",
        &root.path().join("unfiltered"),
        &["config", "get", "remote.origin.promisor"],
        &[],
        b"",
    );
    assert!(!promisor.status.success());
}

/// Repeated `--filter` values combine as Git's do, and `--no-filter` discards earlier ones.
#[test]
fn repeated_filters_combine_like_git() {
    let (root, _, _) = clones();
    let source = root.path().join("source");
    let source = source.to_str().unwrap();
    for (case, (args, expected)) in [
        (
            &["--filter=blob:none", "--filter=tree:0"][..],
            Some("combine:blob:none+tree:0"),
        ),
        (
            &[
                "--filter=blob:none",
                "--filter=tree:0",
                "--no-filter",
                "--filter=blob:limit=1k",
            ],
            Some("blob:limit=1024"),
        ),
        (&["--filter=blob:none", "--no-filter"], None),
    ]
    .into_iter()
    .enumerate()
    {
        let destination = root.path().join(format!("clone-{case}"));
        let mut command = vec!["clone"];
        command.extend_from_slice(args);
        command.extend([source, destination.to_str().unwrap()]);
        let output = girt(root.path(), &command, &[], b"");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let filter = run(
            "git",
            &destination,
            &["config", "get", "remote.origin.partialclonefilter"],
            &[],
            b"",
        );
        match expected {
            Some(expected) => assert_eq!(filter.stdout, format!("{expected}\n").as_bytes()),
            None => assert!(!filter.status.success()),
        }
    }
}

/// A source with a modified file and an edited rename across two commits, and two blob:none
/// no-checkout clones of it, one for Git and one for girt.
fn history_clones() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| support::git(&source, args, b"");
    git(&["init", "-q", "--template=", "-b", "main"]);
    git(&["config", "uploadpack.allowFilter", "true"]);
    let moved = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
    std::fs::write(source.join("a"), "a\nkept\n").unwrap();
    std::fs::write(source.join("moved"), moved).unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "--no-gpg-sign", "-m", "one"]);
    std::fs::write(source.join("a"), "c\nkept\n").unwrap();
    std::fs::remove_file(source.join("moved")).unwrap();
    std::fs::write(source.join("renamed"), moved.replace("five", "FIVE")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "--no-gpg-sign", "-m", "two"]);
    let url = format!("file://{}", source.display());
    let paths = ["git", "girt"].map(|name| root.path().join(name));
    for path in &paths {
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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Under `GIT_NO_LAZY_FETCH`, a content-reading command fails naming the missing objects and
/// fetches nothing, as Git refuses.
fn assert_offline_failure(git_clone: &Path, girt_clone: &Path, git: &[&str], girt_args: &[&str]) {
    let offline = [("GIT_NO_LAZY_FETCH", "1")];
    assert!(!run("git", git_clone, git, &offline, b"").status.success());
    let output = girt(girt_clone, girt_args, &offline, b"");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing from this partial clone"),
        "{stderr}"
    );
    assert_eq!(promisor_packs(girt_clone), 1);
}

#[test]
fn diff_fetches_missing_blobs_in_one_batch() {
    let (_root, git_clone, girt_clone) = history_clones();
    let git_args = ["diff", "--no-renames", "HEAD~1", "HEAD"];
    let girt_args = ["diff", "--patch", "HEAD~1", "HEAD"];
    assert_offline_failure(&git_clone, &girt_clone, &git_args, &girt_args);

    let expected = run("git", &git_clone, &git_args, &[], b"");
    assert_success(&expected);
    let output = girt(&girt_clone, &girt_args, &[], b"");
    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&expected.stdout)
    );
    // Both sides of both changed files arrive in one promisor pack.
    assert_eq!(promisor_packs(&girt_clone), 2);
}

#[test]
fn rename_detection_fetches_missing_candidates() {
    let (_root, git_clone, girt_clone) = history_clones();
    let git_args = ["diff-tree", "-r", "-M", "--name-status", "HEAD~1", "HEAD"];
    let girt_args = ["--git", "diff", "--renames", "HEAD~1", "HEAD"];
    assert_offline_failure(&git_clone, &girt_clone, &git_args, &girt_args);

    let expected = run("git", &git_clone, &git_args, &[], b"");
    assert_success(&expected);
    let output = girt(&girt_clone, &girt_args, &[], b"");
    assert_success(&output);
    assert_eq!(output.stdout, expected.stdout);
}

#[test]
fn blame_fetches_each_missing_version() {
    let (_root, git_clone, girt_clone) = history_clones();
    let git_args = ["blame", "--porcelain", "HEAD", "--", "a"];
    assert_offline_failure(&git_clone, &girt_clone, &git_args, &["blame", "a"]);

    let expected = run("git", &git_clone, &git_args, &[], b"");
    assert_success(&expected);
    // Porcelain starts each group with `<commit> <original> <final> <count>`.
    let expected: Vec<String> = String::from_utf8(expected.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            (fields.len() == 4 && fields[0].len() == 40)
                .then(|| format!("{} {} {} {} a", fields[0], fields[2], fields[3], fields[1]))
        })
        .collect();
    let output = girt(&girt_clone, &["blame", "a"], &[], b"");
    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn checkout_fetches_missing_blobs_like_git() {
    let (_root, git_clone, girt_clone) = history_clones();
    let girt_args = ["checkout", "HEAD"];
    assert_offline_failure(
        &git_clone,
        &girt_clone,
        &["checkout", "-q", "HEAD"],
        &girt_args,
    );
    assert!(!girt_clone.join(".git/index").exists());

    let index = |clone: &Path| {
        let output = run(
            "git",
            clone,
            &["ls-files", "-s"],
            &[("GIT_NO_LAZY_FETCH", "1")],
            b"",
        );
        assert_success(&output);
        output.stdout
    };
    assert_success(&run(
        "git",
        &git_clone,
        &["checkout", "-q", "HEAD"],
        &[],
        b"",
    ));
    assert_success(&girt(&girt_clone, &girt_args, &[], b""));
    assert_eq!(index(&girt_clone), index(&git_clone));
    assert_eq!(promisor_packs(&girt_clone), 2);

    // Another commit's blobs that differ arrive the same way.
    assert_success(&run(
        "git",
        &git_clone,
        &["checkout", "-q", "HEAD~1"],
        &[],
        b"",
    ));
    assert_success(&girt(&girt_clone, &["checkout", "HEAD~1"], &[], b""));
    assert_eq!(index(&girt_clone), index(&git_clone));
    for name in ["a", "moved"] {
        assert_eq!(
            std::fs::read(girt_clone.join(name)).unwrap(),
            std::fs::read(git_clone.join(name)).unwrap()
        );
    }
    assert!(!girt_clone.join("renamed").exists());
}
