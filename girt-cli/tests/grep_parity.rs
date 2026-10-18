//! `girt grep` must print what `git grep` prints and exit with the same status.
//!
//! Git runs with one thread: with more, it can drop a `Binary file … matches` line printed first
//! under context options, which girt does not reproduce.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(command: &mut Command, home: &Path, directory: &Path) -> Output {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .output()
        .unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn git(&self, args: &[&str]) {
        let output = run(Command::new("git").args(args), &self.home, &self.root);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, path: &str, content: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Runs both tools from `directory` (relative to the root) and compares their output.
    fn compare(&self, directory: &str, args: &[&str]) -> Option<String> {
        let directory = self.root.join(directory);
        let git = run(
            Command::new("git")
                .args(["-c", "grep.threads=1", "grep"])
                .args(args),
            &self.home,
            &directory,
        );
        let girt = run(
            Command::new(env!("CARGO_BIN_EXE_girt"))
                .arg("grep")
                .args(args),
            &self.home,
            &directory,
        );
        (git.stdout != girt.stdout || git.status.code() != girt.status.code()).then(|| {
            format!(
                "{args:?} in {}\n--- git ({:?})\n{}--- girt ({:?})\n{}{}",
                directory.display(),
                git.status.code(),
                String::from_utf8_lossy(&git.stdout),
                girt.status.code(),
                String::from_utf8_lossy(&girt.stdout),
                String::from_utf8_lossy(&girt.stderr),
            )
        })
    }
}

/// Tracked text, binary, ignored and nested files, a symlink, function-shaped content, a commit
/// that differs from the working tree, and untracked and ignored untracked files.
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
    };
    fixture.git(&["init", "-q", "-b", "main"]);
    fixture.write(
        "a.c",
        b"int main() {\n  foo bar\n  x\n}\nvoid other() {\n  foobar baz\n  y\n  foo\n}\n",
    );
    fixture.write("b.bin", b"foo\0bin\n");
    fixture.write("d/e/f.txt", b"foo deep\nlast foo");
    fixture.write("ig.log", b"ign foo\n");
    fixture.write(".gitignore", b"*.log\n");
    fixture.write(
        "p.txt",
        b"fa\n 1\n 2\n 3 m\n 4\n 5\n 6 m\nfb m\n 7\n 8\n 9\n10 m\n11 m\n",
    );
    fixture.write("s.txt", b"fz\n 0\n  \n x\nfa\n 1\n \t\n\nfb\n 2\n");
    fixture.write("w.txt", b"foofoo foo_x xfoo foo\nabcabc\n_.b\nfoo bx\n");
    fixture.write("crlf.txt", b"crlf foo\r\nnoeol foo");
    fixture.write("pats", b"foo\n\nb.*r\n");
    fixture.write("\u{fc}n\u{ef}.txt", b"unicode\n");
    fixture.git(&["add", "-f", "."]);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("a.c", fixture.root.join("link")).unwrap();
        fixture.git(&["add", "link"]);
    }
    fixture.git(&["-c", "commit.gpgsign=false", "commit", "-qm", "x"]);
    fixture.write("a.c", b"int main() {\n  foo changed\n}\n");
    fixture.write("0.txt", b"foo untracked\n");
    fixture.write("i2.log", b"foo ignored\n");
    fixture
}

const CASES: &[(&str, &[&str])] = &[
    ("", &["foo"]),
    ("", &["-n", "foo"]),
    ("", &["-w", "foo"]),
    ("", &["-o", "-n", "--column", "foo"]),
    ("", &["-c", "foo"]),
    ("", &["-l", "foo"]),
    ("", &["-L", "foo"]),
    ("", &["-q", "foo"]),
    ("", &["nomatch"]),
    ("", &["-h", "foo"]),
    ("", &["-i", "-n", "FOO"]),
    ("", &["-v", "-c", "foo"]),
    ("", &["-z", "-n", "foo"]),
    ("", &["-z", "-l", "foo"]),
    ("", &["-I", "foo"]),
    ("", &["-a", "foo"]),
    ("", &["--cached", "foo"]),
    ("", &["--untracked", "foo"]),
    ("", &["--untracked", "--no-exclude-standard", "foo"]),
    ("", &["foo", "HEAD"]),
    ("", &["foo", "HEAD:d"]),
    ("", &["-l", "foo", "HEAD:d/e/f.txt"]),
    ("", &["foo", "HEAD", "HEAD~0", "--", "a.c"]),
    ("", &["-p", "foo", "HEAD"]),
    ("", &["-W", "foo", "HEAD"]),
    ("", &["-C1", "foo", "HEAD"]),
    ("", &["-3", "baz", "HEAD"]),
    ("", &["--heading", "--break", "-n", "foo"]),
    (
        "",
        &[
            "--break",
            "--heading",
            "-n",
            "-C1",
            "m",
            "--",
            "p.txt",
            "a.c",
        ],
    ),
    ("", &["-A1", "-e", "foo", "--", "b.bin", "ig.log"]),
    ("", &["-p", "m", "--", "p.txt"]),
    ("", &["-p", "-n", "-B1", "m", "--", "p.txt"]),
    ("", &["-n", "-W", "m", "--", "p.txt"]),
    ("", &["-n", "-W", "1", "--", "s.txt"]),
    ("", &["-m1", "-A1", "-n", "m", "--", "p.txt"]),
    ("", &["-e", "foo", "--and", "-e", "bar", "HEAD"]),
    ("", &["-e", "foo", "--and", "--not", "-e", "bar", "HEAD"]),
    ("", &["--all-match", "-e", "foo", "-e", "baz", "HEAD"]),
    (
        "",
        &[
            "(", "-e", "foo", "--or", "-e", "x", ")", "--and", "--not", "-e", "bar", "HEAD",
        ],
    ),
    ("", &["-f", "pats"]),
    ("", &["-w", "-o", "-n", "foo", "--", "w.txt"]),
    ("", &["-w", "-o", r"\.\?b", "--", "w.txt"]),
    ("", &["-o", "x*", "--", "w.txt"]),
    ("", &["-o", "-v", "foo", "--", "w.txt"]),
    ("", &["-E", "-o", "(abc)+"]),
    ("", &["-F", "-e", "(", "HEAD"]),
    ("", &["-n", "foo$", "--", "crlf.txt"]),
    ("", &["--max-depth", "0", "foo"]),
    ("", &["--max-depth", "0", "foo", "--", "d"]),
    ("", &["--max-depth", "0", "foo", "--", "d/e"]),
    ("", &["--max-depth", "0", "foo", "--", "d/*"]),
    ("", &["foo", "--", ":!a.c"]),
    ("", &["foo", "--", ":(icase)A.C"]),
    ("", &["foo", "--", "*.txt"]),
    ("d", &["-n", "foo"]),
    ("d", &["foo", "--", "../a.c"]),
    ("d", &["--full-name", "foo"]),
    ("d", &["foo", "HEAD", "--", "../a.c"]),
    ("d", &["foo", "--", ":/"]),
    ("d", &["--max-depth", "1", "-l", "foo"]),
    ("d", &["--no-index", "-n", "foo"]),
    ("", &["-L", "missing", "--", "a.c"]),
    ("", &["-L", "foo", "--", "a.c"]),
    (
        "",
        &["--all-match", "-m1", "-e", "foo", "-e", "baz", "--", "a.c"],
    ),
    ("", &["-e", "x\nbaz", "HEAD", "--", "a.c"]),
    ("", &["--", "foo", "HEAD"]),
    ("", &["--", "foo", "--", "a.c"]),
    ("", &["unicode"]),
    ("", &["-z", "-l", "unicode"]),
    ("", &["unicode", "HEAD"]),
    ("d", &["unicode", "--", ".."]),
];

#[test]
fn matches_git() {
    let fixture = fixture();
    let failures: Vec<String> = CASES
        .iter()
        .filter_map(|(directory, args)| fixture.compare(directory, args))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
