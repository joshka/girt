//! Identity presentation agrees with Git using independently generated commits.
#[path = "../../tests/support/history_git.rs"]
#[allow(dead_code)]
mod oracle;

#[test]
fn log_and_blame_present_canonical_identities() {
    let root = tempfile::tempdir().unwrap();
    oracle::git(root.path(), &["init", "--template="], b"");
    std::fs::write(root.path().join("file"), b"line\n").unwrap();
    oracle::git(root.path(), &["add", "file"], b"");
    oracle::git(root.path(), &["commit", "-m", "original"], b"");
    std::fs::write(
        root.path().join(".mailmap"),
        b"Canonical <canonical@e> <pack@example.com>\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_girt"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    for enabled in [true, false] {
        let flag = if enabled {
            "--use-mailmap=true"
        } else {
            "--use-mailmap=false"
        };
        let git_flag = if enabled {
            "--use-mailmap"
        } else {
            "--no-use-mailmap"
        };
        assert_eq!(
            run(&["--git", "log", flag]),
            oracle::git(root.path(), &["log", git_flag], b"")
        );
        oracle::git(
            root.path(),
            &[
                "config",
                "log.mailmap",
                if enabled { "true" } else { "false" },
            ],
            b"",
        );
        assert_eq!(
            run(&["--git", "log"]),
            oracle::git(root.path(), &["log"], b"")
        );
    }
    assert_eq!(
        run(&["--git", "log", "--use-mailmap"]),
        oracle::git(root.path(), &["log", "--use-mailmap"], b"")
    );
    assert!(!run(&["blame", "file"]).starts_with(b"<"));
    let blame = run(&["blame", "-e", "file"]);
    assert!(blame.starts_with(b"<canonical@e> "));
    let git_blame = oracle::git(root.path(), &["blame", "-e", "HEAD", "--", "file"], b"");
    assert!(
        git_blame
            .windows(b"<canonical@e>".len())
            .any(|w| w == b"<canonical@e>")
    );
    std::fs::remove_file(root.path().join(".mailmap")).unwrap();
    assert!(run(&["blame", "-e", "file"]).starts_with(b"<pack@example.com> "));
    oracle::git(
        root.path(),
        &["config", "mailmap.file", "~/unsupported"],
        b"",
    );
    assert!(!run(&["blame", "file"]).starts_with(b"<"));
    std::fs::write(
        root.path().join("home-map"),
        b"Home <home@e> <pack@example.com>\n",
    )
    .unwrap();
    oracle::git(root.path(), &["config", "mailmap.file", "~/home-map"], b"");
    oracle::git(root.path(), &["config", "--unset", "mailmap.file"], b"");
    oracle::git(root.path(), &["config", "--unset", "log.mailmap"], b"");
    std::fs::write(
        root.path().join("absent-config"),
        b"[mailmap]\nfile = ~/home-map\n[log]\nmailmap = true\n",
    )
    .unwrap();
    let output = run(&["--git", "log"]);
    assert!(
        output
            .windows(b"Home <home@e>".len())
            .any(|w| w == b"Home <home@e>")
    );
}
