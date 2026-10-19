//! Partial clones made by Git or girt, served by Git or girt, are usable by the other tool.
//!
//! Each case clones with a filter through an SSH command that runs the chosen server locally, then
//! checks the clone against a Git client cloning from Git's server, and that the server spoke the
//! requested protocol version. The clone must match Git's: the same objects are missing,
//! with the same promisor settings and `.promisor` files. The other tool then reads the clone,
//! including a lazy fetch of a missing blob from the same server.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rstest::rstest;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tool {
    Git,
    Girt,
}

use Tool::{Girt, Git};

/// Runs `program` in `dir` with isolated configuration, fixed identities and dates, and `env`.
fn run(program: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
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
        .env("GIT_AUTHOR_NAME", "Interop Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Interop Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .envs(env.iter().copied())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn succeed(output: Output, what: &str) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    succeed(run("git", dir, args, &[]), &format!("git {args:?}"))
}

/// A source repository served over a local SSH command by Git's `upload-pack` or girt's.
struct Remote {
    root: tempfile::TempDir,
    url: String,
    /// The environment selecting the SSH command for the server under test, and the protocol.
    env: Vec<(&'static str, String)>,
    /// The same for Git's server, which the reference clone and its fetches use.
    reference_env: Vec<(&'static str, String)>,
}

impl Remote {
    /// `main` has files `a` and `b`, and an older commit had a different `a`.
    fn new(server: Tool, format: &str, protocol: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        git(
            root.path(),
            &[
                "init",
                "-q",
                "--template=",
                "-b",
                "main",
                &format!("--object-format={format}"),
                "source",
            ],
        );
        git(&source, &["config", "uploadpack.allowFilter", "true"]);
        // Protocol v0 lazy fetches want unadvertised blobs, which Git's server refuses otherwise.
        git(
            &source,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        for (a, message) in [("old\n", "one"), ("a\n", "two")] {
            std::fs::write(source.join("a"), a).unwrap();
            std::fs::write(source.join("b"), "b\n").unwrap();
            git(&source, &["add", "."]);
            git(&source, &["commit", "-q", "--no-gpg-sign", "-m", message]);
        }

        // The SSH command runs its last argument, `git-upload-pack '<path>'`, with a shim first on
        // `PATH` that runs the chosen server. The server under test copies everything it sends to
        // one file per connection, which shows the protocol version it actually spoke.
        let responses = root.path().join("responses");
        std::fs::create_dir(&responses).unwrap();
        let girt_server = format!("'{}' upload-pack \"$1\"", env!("CARGO_BIN_EXE_girt"));
        let tested = match server {
            Git => "git upload-pack \"$1\"",
            Girt => girt_server.as_str(),
        };
        let tested = format!("{tested} | tee \"{}/$$\"", responses.display());
        let env = |name: &str, serve: &str| {
            let shims = root.path().join(format!("{name}-shims"));
            std::fs::create_dir(&shims).unwrap();
            executable(
                &shims.join("git-upload-pack"),
                &format!("#!/bin/sh\n{serve}\n"),
            );
            // Git passes `GIT_PROTOCOL` only to an SSH command it recognises by name as OpenSSH,
            // and otherwise falls back to protocol v0.
            let ssh = shims.join("ssh");
            executable(
                &ssh,
                &format!(
                    "#!/bin/sh\nfor arg; do last=$arg; done\nPATH='{}':$PATH exec sh -c \"$last\"\n",
                    shims.display()
                ),
            );
            vec![
                ("GIT_SSH_COMMAND", ssh.to_str().unwrap().to_owned()),
                ("GIT_CONFIG_COUNT", "1".to_owned()),
                ("GIT_CONFIG_KEY_0", "protocol.version".to_owned()),
                ("GIT_CONFIG_VALUE_0", protocol.to_owned()),
            ]
        };
        Self {
            url: format!("localhost:{}", source.display()),
            env: env("tested", &tested),
            reference_env: env("reference", "exec git upload-pack \"$1\""),
            root,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn source(&self) -> PathBuf {
        self.path("source")
    }

    /// Runs `tool` in `dir` against this remote, with lazy fetching off when `offline` is set.
    fn output(&self, tool: Tool, dir: &Path, args: &[&str], offline: bool) -> Output {
        self.output_with(&self.env, tool, dir, args, offline)
    }

    fn output_with(
        &self,
        env: &[(&'static str, String)],
        tool: Tool,
        dir: &Path,
        args: &[&str],
        offline: bool,
    ) -> Output {
        let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        if offline {
            env.push(("GIT_NO_LAZY_FETCH", "1"));
        }
        let program = match tool {
            Git => "git",
            Girt => env!("CARGO_BIN_EXE_girt"),
        };
        run(program, dir, args, &env)
    }

    fn ok(&self, tool: Tool, dir: &Path, args: &[&str], offline: bool) -> Vec<u8> {
        succeed(
            self.output(tool, dir, args, offline),
            &format!("{tool:?} {args:?}"),
        )
    }

    /// Clones with `filter` into `name` using `client`.
    fn clone(&self, client: Tool, filter: &str, name: &str) -> PathBuf {
        let filter = format!("--filter={filter}");
        let mut args = vec!["clone"];
        if client == Git {
            args.extend(["-q", "--template="]);
        }
        args.extend([filter.as_str(), &self.url, name]);
        self.ok(client, self.root.path(), &args, false);
        self.path(name)
    }

    /// Clones with `filter` into `name` as Git does from Git's server, for comparison.
    fn clone_reference(&self, filter: &str, name: &str) -> PathBuf {
        let filter = format!("--filter={filter}");
        let args = ["clone", "-q", "--template=", &filter, &self.url, name];
        let output = self.output_with(&self.reference_env, Git, self.root.path(), &args, false);
        succeed(output, "reference clone");
        self.path(name)
    }

    /// Fetches into the reference clone from Git's server.
    fn fetch_reference(&self, reference: &Path) {
        let output = self.output_with(&self.reference_env, Git, reference, &["fetch", "-q"], false);
        succeed(output, "reference fetch");
    }

    /// Checks that the server under test answered at least once since the last call, and that
    /// every answer was in protocol `version`: v2 starts with `version 2`, and v0 with a
    /// reference advertisement whose first line carries capabilities after a NUL.
    fn assert_spoke(&self, version: &str) {
        let responses = self.path("responses");
        let mut count = 0;
        for entry in std::fs::read_dir(&responses).unwrap() {
            let path = entry.unwrap().path();
            let sent = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            count += 1;
            let first = first_packet(&sent);
            match version {
                "2" => assert_eq!(first, b"version 2\n", "{}", String::from_utf8_lossy(&sent)),
                _ => assert!(
                    !first.starts_with(b"version ") && first.contains(&0),
                    "{}",
                    String::from_utf8_lossy(&sent)
                ),
            }
        }
        assert!(count > 0, "the server under test was never used");
    }

    fn rev(&self, revision: &str) -> String {
        let id = git(&self.source(), &["rev-parse", revision]);
        String::from_utf8(id).unwrap().trim().to_owned()
    }

    fn commit_c(&self) {
        let source = self.source();
        std::fs::write(source.join("c"), "c\n").unwrap();
        git(&source, &["add", "c"]);
        git(&source, &["commit", "-q", "--no-gpg-sign", "-m", "three"]);
    }
}

/// The payload of the first pkt-line in `stream`.
fn first_packet(stream: &[u8]) -> &[u8] {
    let length = std::str::from_utf8(&stream[..4]).unwrap();
    let length = usize::from_str_radix(length, 16).unwrap();
    &stream[4..length]
}

fn executable(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `git rev-list --missing=print` IDs, without lazy fetching.
fn missing(clone: &Path) -> Vec<String> {
    let output = run(
        "git",
        clone,
        &["rev-list", "--objects", "--all", "--missing=print"],
        &[("GIT_NO_LAZY_FETCH", "1")],
    );
    let mut missing: Vec<String> = String::from_utf8(succeed(output, "rev-list"))
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix('?').map(str::to_owned))
        .collect();
    missing.sort();
    missing
}

/// The contents of every `.promisor` file, sorted.
fn promisor_files(clone: &Path) -> Vec<String> {
    let mut contents: Vec<String> = std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "promisor"))
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect();
    contents.sort();
    contents
}

/// The partial clone settings Git reads.
fn settings(clone: &Path) -> Vec<String> {
    [
        "core.repositoryformatversion",
        "extensions.objectformat",
        "remote.origin.promisor",
        "remote.origin.partialclonefilter",
    ]
    .iter()
    .map(|name| {
        let output = run("git", clone, &["config", "get", name], &[]);
        format!("{name}={}", String::from_utf8_lossy(&output.stdout).trim())
    })
    .collect()
}

/// Checks a clone against Git's own clone of the same remote with the same filter.
fn assert_matches_git(clone: &Path, reference: &Path) {
    assert_eq!(missing(clone), missing(reference), "missing objects");
    assert_eq!(settings(clone), settings(reference));
    assert_eq!(promisor_files(clone), promisor_files(reference));
}

/// Git reads, lazily fetches into, collects and checks a partial clone girt made.
fn git_uses_girt_clone(remote: &Remote, clone: &Path, old: &str) {
    for args in [&["fsck", "--strict"][..], &["status", "--porcelain"]] {
        let output = remote.ok(Git, clone, args, true);
        assert!(output.is_empty(), "{}", String::from_utf8_lossy(&output));
    }
    let log = remote.ok(Git, clone, &["log", "-p", "--format=%s"], false);
    assert!(String::from_utf8(log).unwrap().contains("-old\n+a\n"));
    assert!(missing(clone).is_empty(), "log -p fetched {old}");
    remote.ok(Git, clone, &["gc", "-q"], true);
    remote.ok(Git, clone, &["fsck", "--strict"], true);
    assert_eq!(
        remote.ok(Git, clone, &["cat-file", "-p", old], true),
        b"old\n"
    );
}

/// girt checks, lazily fetches into and fetches into a partial clone Git made.
fn girt_uses_git_clone(remote: &Remote, clone: &Path, reference: &Path, old: &str) {
    remote.ok(Girt, clone, &["fsck"], true);
    let offline = remote.output(Girt, clone, &["object", "cat", old], true);
    assert!(!offline.status.success());
    assert_eq!(
        remote.ok(Girt, clone, &["object", "cat", old], false),
        b"old\n"
    );
    assert!(missing(clone).is_empty());
    remote.ok(Git, clone, &["fsck", "--strict"], true);

    remote.commit_c();
    remote.ok(Girt, clone, &["fetch"], false);
    remote.fetch_reference(reference);
    assert_eq!(
        remote.ok(Git, clone, &["rev-parse", "origin/main"], true),
        remote.ok(Git, reference, &["rev-parse", "origin/main"], true),
    );
    let mut expected = missing(reference);
    expected.retain(|id| id != old);
    assert_eq!(missing(clone), expected);
    remote.ok(Girt, clone, &["fsck"], true);
    remote.ok(Git, clone, &["fsck", "--strict"], true);
}

#[rstest]
fn blobless_clone_interop(
    #[values(Git, Girt)] client: Tool,
    #[values(Git, Girt)] server: Tool,
    #[values("sha1", "sha256")] format: &str,
    #[values("0", "2")] protocol: &str,
) {
    let remote = Remote::new(server, format, protocol);
    let reference = remote.clone_reference("blob:none", "reference");
    let clone = remote.clone(client, "blob:none", "clone");
    remote.assert_spoke(protocol);
    let old = remote.rev("HEAD~:a");
    assert_eq!(missing(&clone), std::slice::from_ref(&old));
    assert_matches_git(&clone, &reference);

    // The other tool works on the clone; with Git on both sides, girt reads Git's clone.
    match client {
        Girt => git_uses_girt_clone(&remote, &clone, &old),
        Git => girt_uses_git_clone(&remote, &clone, &reference, &old),
    }
    remote.assert_spoke(protocol);
}

/// `tree:0` clones from girt's server by girt leave out every tree and blob, as Git's do.
#[test]
fn treeless_clone_interop() {
    let remote = Remote::new(Girt, "sha1", "2");
    let reference = remote.clone_reference("tree:0", "reference");
    let clone = remote.clone(Girt, "tree:0", "clone");
    remote.assert_spoke("2");
    assert_matches_git(&clone, &reference);
    assert!(missing(&clone).contains(&remote.rev("HEAD~^{tree}")));
    let log = remote.ok(Git, &clone, &["log", "-p", "--format=%s"], false);
    assert!(String::from_utf8(log).unwrap().contains("-old\n+a\n"));
    remote.ok(Git, &clone, &["fsck", "--strict"], true);
    remote.assert_spoke("2");
}
