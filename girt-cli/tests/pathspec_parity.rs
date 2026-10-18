//! `girt --git log/diff -- <pathspec>` must print what Git prints, over a history with merges.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(command: &mut Command, home: &Path, directory: &Path, date: u64) -> Output {
    let date = format!("@{date} +0000");
    command
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
    date: std::cell::Cell<u64>,
}

impl Fixture {
    fn git(&self, args: &[&str]) {
        self.date.set(self.date.get() + 60);
        let output = run(
            Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .args(args),
            &self.home,
            &self.root,
            self.date.get(),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit(&self, files: &[(&str, &str)], message: &str) {
        for (path, content) in files {
            let path = self.root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message, "--allow-empty"]);
    }
}

/// Two branches merged twice: one merge brings in a change to `src`, the other only touches
/// `docs`, and one merge resolves both sides of `shared` to a new value.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();
    let fixture = Fixture {
        _temp: temp,
        home,
        root,
        date: std::cell::Cell::new(1_700_000_000),
    };
    fixture.git(&["init", "-q", "-b", "main"]);
    fixture.commit(
        &[("src/a.rs", "1\n"), ("docs/x.md", "1\n"), ("shared", "1\n")],
        "root",
    );
    fixture.git(&["checkout", "-qb", "side"]);
    fixture.commit(&[("src/a.rs", "side\n")], "side src");
    fixture.commit(&[("docs/x.md", "side\n")], "side docs");
    fixture.commit(&[("shared", "side\n")], "side shared");
    fixture.git(&["checkout", "-q", "main"]);
    fixture.commit(&[("docs/y.md", "main\n")], "main docs");
    fixture.commit(&[("shared", "main\n")], "main shared");
    fixture.git(&[
        "merge",
        "-q",
        "--no-ff",
        "-s",
        "ours",
        "side",
        "-m",
        "merge side",
    ]);
    fixture.commit(&[("shared", "both\n")], "resolve shared");
    fixture.git(&["checkout", "-q", "side"]);
    fixture.commit(&[("docs/z.md", "side\n")], "side docs only");
    fixture.git(&["checkout", "-q", "main"]);
    fixture.git(&["merge", "-q", "--no-ff", "side", "-m", "merge docs"]);
    fixture.commit(&[("src/b.rs", "b\n")], "main src");
    fixture.commit(&[], "empty");
    fixture
}

const SPECS: &[&[&str]] = &[
    &["src"],
    &["docs"],
    &["shared"],
    &["*.md"],
    &[":!docs"],
    &["src/a.rs"],
    &["missing"],
    &["docs/z.md"],
    &[":(glob)*"],
    &[":(icase)SRC"],
    &["src", "shared"],
    &["."],
    &[":/"],
];

fn compare(fixture: &Fixture, directory: &str, girt: &[&str], git: &[&str]) {
    let directory = fixture.root.join(directory);
    let expected = run(Command::new("git").args(git), &fixture.home, &directory, 0);
    let actual = run(
        Command::new(env!("CARGO_BIN_EXE_girt"))
            .arg("--git")
            .args(girt),
        &fixture.home,
        &directory,
        0,
    );
    assert!(expected.status.success(), "git {git:?}");
    assert!(
        actual.status.success(),
        "girt {girt:?}: {}",
        String::from_utf8_lossy(&actual.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&actual.stdout),
        String::from_utf8_lossy(&expected.stdout),
        "{girt:?}"
    );
}

#[test]
fn log_matches_git() {
    let fixture = fixture();
    for specs in SPECS {
        let mut args = vec!["log", "--oneline", "--"];
        args.extend(*specs);
        compare(&fixture, "", &args, &args);
        let mut girt = vec!["log", "--oneline", "--limit", "2", "--"];
        girt.extend(*specs);
        let mut git = vec!["log", "--oneline", "-n", "2", "--"];
        git.extend(*specs);
        compare(&fixture, "", &girt, &git);
        let mut girt = vec!["log", "HEAD~2", "--"];
        girt.extend(*specs);
        compare(&fixture, "", &girt, &girt);
    }
    compare(
        &fixture,
        "src",
        &["log", "--oneline", "--", "."],
        &["log", "--oneline", "--", "."],
    );
    compare(
        &fixture,
        "src",
        &["log", "--oneline", "--", ":!a.rs"],
        &["log", "--oneline", "--", ":!a.rs"],
    );
}

#[test]
fn diff_matches_git() {
    let fixture = fixture();
    for specs in SPECS {
        let mut girt = vec!["diff", "HEAD~4", "HEAD", "--"];
        girt.extend(*specs);
        let mut git = vec!["diff", "--name-status", "HEAD~4", "HEAD", "--"];
        git.extend(*specs);
        compare(&fixture, "", &girt, &git);
    }
    compare(
        &fixture,
        "docs",
        &["diff", "HEAD~4", "HEAD", "--", "."],
        &["diff", "--name-status", "HEAD~4", "HEAD", "--", "."],
    );
}
