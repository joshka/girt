//! `girt remote heads <remote>` over smart HTTP compared with `git ls-remote <remote>`, against a
//! loopback server that requires Basic authentication. Credentials come from a recording askpass
//! script; no real credential helper or terminal is used.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

// Stops the loopback server when the test ends, including on panic.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn server(repository: &Path, requests: &Path) -> (Server, String) {
    let mut child = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/http/server.py"
        ))
        .arg(repository)
        .args(["--authorization", "Basic dTpw", "--requests"])
        .arg(requests)
        .arg("--owner")
        .arg(std::process::id().to_string())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 required for the HTTP fixture");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line.trim().parse().expect("server port");
    (Server(child), format!("http://127.0.0.1:{port}/repo"))
}

// Runs `program` with isolated configuration, no inherited Git or proxy variables, and `askpass`.
fn run(program: &str, args: &[&str], directory: &Path, askpass: &Path) -> Output {
    isolated(program, directory)
        .env("HOME", directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", askpass)
        .env_remove("SSH_AUTH_SOCK")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

// `program` in `directory` without inherited Git, proxy or askpass variables, and with isolated
// configuration.
fn isolated(program: &str, directory: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy().to_ascii_lowercase();
        if key_text.starts_with("git_") || key_text.ends_with("_proxy") || key_text == "ssh_askpass"
        {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"));
    command
}

fn git(directory: &Path, args: &[&str]) {
    let output = isolated("git", directory)
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn remote_heads_authenticates_like_git_ls_remote() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "--quiet", "--initial-branch=main", "."]);
    git(
        &source,
        &["commit", "--quiet", "--allow-empty", "-m", "first"],
    );
    git(&source, &["tag", "-a", "-m", "tag", "v1"]);
    let bare = root.join("bare.git");
    git(root, &["clone", "--quiet", "--bare", "source", "bare.git"]);
    let (_server, url) = server(&bare, &root.join("requests"));

    let local = root.join("local");
    std::fs::create_dir(&local).unwrap();
    git(&local, &["init", "--quiet", "."]);
    git(&local, &["remote", "add", "origin", &url]);
    git(&local, &["config", "credential.helper", ""]);
    let askpass = root.join("askpass");
    std::fs::write(
        &askpass,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"{}/questions\"\n\
             case \"$1\" in Username*) echo u;; *) echo p;; esac\n",
            root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o755)).unwrap();

    let git_output = run("git", &["ls-remote", "origin"], &local, &askpass);
    assert!(
        git_output.status.success(),
        "{}",
        String::from_utf8_lossy(&git_output.stderr)
    );
    let git_questions = std::fs::read_to_string(root.join("questions")).unwrap();
    std::fs::remove_file(root.join("questions")).unwrap();

    let girt_output = run(
        env!("CARGO_BIN_EXE_girt"),
        &["remote", "heads", "origin"],
        &local,
        &askpass,
    );
    assert!(
        girt_output.status.success(),
        "{}",
        String::from_utf8_lossy(&girt_output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&girt_output.stdout),
        String::from_utf8_lossy(&git_output.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("questions")).unwrap(),
        git_questions
    );
}
