//! Commit message cleanup compared with `git stripspace` and `git commit` on an original corpus.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::message::{Cleanup, CommentPrefix, cleanup};

/// Messages written for these tests: blank-line runs, comments, whitespace kinds and scissors.
const CORPUS: &[&[u8]] = &[
    b"",
    b"\n",
    b"subject",
    b"subject\n",
    b"\n\n  subject  \n\n\n\tbody\t\n\n",
    b"subject\r\n\r\nbody \r\n",
    b"a\x0b\x0c \nb\xc2\xa0\n",
    b"a\0b  \n",
    b"# only a comment\n#\n",
    b"subject\n# comment\n\n# comment\nbody\n",
    b" #indented\n#\ttab\n##double\nlast",
    b"// slash\n//\n/x\n; semi\n",
    b"subject\n\n\n\n# ------------------------ >8 ------------------------\ndiff\n",
    b"subject\n#  ------------------------ >8 ------------------------\nkept\n",
    b"subject\n# ------------------------ >8 ------------------------ x\nkept\n",
    b"subject\n// ------------------------ >8 ------------------------\ncut\n",
    b"subject\n# ------------------------ >8 ------------------------ \ncut\n",
    b"subject\n# ------------------------ >8 ------------------------\t\r\ncut\n",
    b"subject\n  #\nb # ------------------------ >8 ------------------------\n",
];

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Message Author")
        .env("GIT_AUTHOR_EMAIL", "message@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Message Author")
        .env("GIT_COMMITTER_EMAIL", "message@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .env("GIT_EDITOR", "true")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git required for message compatibility tests");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn whitespace_and_strip_match_stripspace() {
    let directory = tempfile::tempdir().unwrap();
    for prefix in [&b"#"[..], b"//", b";"] {
        let setting = format!(
            "core.commentString={}",
            std::str::from_utf8(prefix).unwrap()
        );
        let comment = CommentPrefix::new(prefix).unwrap();
        for message in CORPUS {
            let whitespace = git(directory.path(), &["-c", &setting, "stripspace"], message);
            let strip = git(
                directory.path(),
                &["-c", &setting, "stripspace", "-s"],
                message,
            );
            assert_eq!(
                cleanup(message, Cleanup::Whitespace, &comment),
                whitespace,
                "{message:?}"
            );
            assert_eq!(
                cleanup(message, Cleanup::Strip, &comment),
                strip,
                "{prefix:?} {message:?}"
            );
        }
    }
}

/// `git commit -e -F` with `commit.cleanup=scissors` stores what [`Cleanup::Scissors`] returns.
#[test]
fn scissors_matches_commit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q", "--template=", "."], b"");
    for prefix in ["#", "//"] {
        let comment = CommentPrefix::new(prefix.as_bytes()).unwrap();
        let setting = format!("core.commentString={prefix}");
        for message in CORPUS {
            let expected = cleanup(message, Cleanup::Scissors, &comment);
            // Git aborts an empty commit and refuses NUL bytes in a message.
            if expected.is_empty() || message.contains(&0) {
                continue;
            }
            std::fs::write(root.join("message"), message).unwrap();
            git(
                root,
                &[
                    "-c",
                    "commit.cleanup=scissors",
                    "-c",
                    &setting,
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-e",
                    "-F",
                    "message",
                ],
                b"",
            );
            let stored = git(root, &["cat-file", "commit", "HEAD"], b"");
            let body = stored
                .windows(2)
                .position(|pair| pair == b"\n\n")
                .map(|at| &stored[at + 2..])
                .unwrap();
            assert_eq!(body, expected, "{prefix} {message:?}");
        }
    }
}
