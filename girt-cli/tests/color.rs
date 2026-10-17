//! Color policy is exercised in subprocesses so tests never mutate the process environment.

use std::fs;
use std::process::{Command, Output};

fn git(directory: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "Color Test")
        .env("GIT_AUTHOR_EMAIL", "color@example.com")
        .env("GIT_COMMITTER_NAME", "Color Test")
        .env("GIT_COMMITTER_EMAIL", "color@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    git(path, &["init", "-q"]);
    fs::write(path.join("file"), "old\n").unwrap();
    git(path, &["add", "."]);
    git(
        path,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "first"],
    );
    git(path, &["tag", "first"]);
    fs::write(path.join("file"), "new\n").unwrap();
    git(path, &["add", "."]);
    git(
        path,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "second"],
    );
    fs::write(path.join("staged"), "staged\n").unwrap();
    git(path, &["add", "staged"]);
    fs::write(path.join("file"), "unstaged\n").unwrap();
    fs::write(path.join("untracked"), "untracked\n").unwrap();
    directory
}

fn girt(path: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_girt"));
    command.args(args).current_dir(path);
    for name in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "TERM", "GIRT_LOG"] {
        command.env_remove(name);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

#[test]
fn human_output_obeys_color_policy_and_git_stays_plain() {
    let directory = fixture();
    for command in [
        &["status", "--untracked"][..],
        &["diff", "first", "HEAD", "--patch"],
        &["diff", "first", "HEAD"],
        &["log"],
        &["log", "--oneline"],
    ] {
        let plain = girt(directory.path(), command, &[]);
        assert!(plain.status.success(), "{plain:?}");
        assert!(!plain.stdout.contains(&0x1b), "{command:?}");
        let mut always = vec!["--color", "always"];
        always.extend(command);
        let colored = girt(directory.path(), &always, &[("NO_COLOR", "1")]);
        assert!(colored.status.success(), "{colored:?}");
        assert!(colored.stdout.contains(&0x1b), "{command:?}");

        let no_color = girt(
            directory.path(),
            command,
            &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")],
        );
        assert_eq!(plain.stdout, no_color.stdout);
        let mut never = vec!["--color=never"];
        never.extend(command);
        assert_eq!(
            plain.stdout,
            girt(directory.path(), &never, &[("CLICOLOR_FORCE", "1")]).stdout
        );

        let mut git = vec!["--git", "--color=always"];
        git.extend(command);
        let output = girt(directory.path(), &git, &[("CLICOLOR_FORCE", "1")]);
        assert!(output.status.success(), "{output:?}");
        assert!(!output.stdout.contains(&0x1b), "{command:?}");
    }
}

#[test]
fn help_and_parse_errors_honor_early_flags() {
    let directory = fixture();
    let help = girt(
        directory.path(),
        &["--help", "--color=always"],
        &[("NO_COLOR", "1")],
    );
    assert!(help.status.success());
    assert!(help.stdout.contains(&0x1b));
    let git_help = girt(
        directory.path(),
        &["--git", "--color=always", "--help"],
        &[],
    );
    assert!(!git_help.stdout.contains(&0x1b));
    let help = girt(
        directory.path(),
        &["status", "--help", "--color", "never"],
        &[("CLICOLOR_FORCE", "1")],
    );
    assert!(!help.stdout.contains(&0x1b));
    let error = girt(
        directory.path(),
        &["--bad", "--color=never"],
        &[("CLICOLOR_FORCE", "1")],
    );
    assert!(!error.status.success());
    assert!(!error.stderr.contains(&0x1b));
    let error = girt(
        directory.path(),
        &["--color=always", "--bad"],
        &[("NO_COLOR", "1")],
    );
    assert!(error.stderr.contains(&0x1b));
}

#[test]
fn diagnostics_follow_stderr_policy() {
    let directory = tempfile::tempdir().unwrap();
    let error = girt(
        directory.path(),
        &["--color=always", "status"],
        &[("NO_COLOR", "1")],
    );
    assert!(!error.status.success());
    assert!(error.stderr.contains(&0x1b));
    let error = girt(
        directory.path(),
        &["--color=never", "status"],
        &[("CLICOLOR_FORCE", "1")],
    );
    assert!(!error.stderr.contains(&0x1b));
}

#[test]
fn colors_identify_changes_and_preserve_patch_bytes() {
    let directory = fixture();
    let status = girt(
        directory.path(),
        &["--color=always", "status", "--untracked"],
        &[],
    );
    assert!(status.stdout.windows(6).any(|bytes| bytes == b"\x1b[32mA"));
    assert!(status.stdout.windows(6).any(|bytes| bytes == b"\x1b[31mM"));
    assert!(status.stdout.windows(7).any(|bytes| bytes == b"\x1b[31m??"));
    let names = girt(
        directory.path(),
        &["--color=always", "diff", "first", "HEAD"],
        &[],
    );
    assert!(names.stdout.ends_with(b"\x1b[0m\n"));
    assert!(!names.stdout.ends_with(b"\n\x1b[0m"));
    let patch = girt(
        directory.path(),
        &["--color=always", "diff", "first", "HEAD", "--patch"],
        &[],
    );
    let text = String::from_utf8(patch.stdout).unwrap();
    assert!(text.contains("\x1b[1mdiff --git"));
    assert!(text.contains("\x1b[36m@@"));
    assert!(text.contains("\x1b[32m+new"));
    assert!(text.contains("\x1b[31m-old"));
    let plain = girt(
        directory.path(),
        &["--color=never", "diff", "first", "HEAD", "--patch"],
        &[],
    );
    let stripped = text
        .replace("\x1b[1m", "")
        .replace("\x1b[36m", "")
        .replace("\x1b[32m", "")
        .replace("\x1b[31m", "")
        .replace("\x1b[0m", "");
    assert_eq!(stripped.as_bytes(), plain.stdout);
}
