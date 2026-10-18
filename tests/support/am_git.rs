//! Original stopped-am fixture shared by interoperability and fault tests.
use std::fs;
use std::path::Path;

use super::oracle::{git, git_output};

pub fn stopped(root: &Path, format: &str, binary: bool) -> Vec<u8> {
    git(
        root,
        &["init", "-b", "main", &format!("--object-format={format}")],
        b"",
    );
    fs::write(root.join("file"), b"base\n").unwrap();
    git(root, &["add", "file"], b"");
    git(root, &["commit", "-m", "base"], b"");
    let base = git(root, &["rev-parse", "HEAD"], b"");
    let base = std::str::from_utf8(&base).unwrap().trim();
    let content = if binary {
        b"binary\0postimage".as_slice()
    } else {
        b"changed\n-- \n"
    };
    fs::write(root.join("file"), content).unwrap();
    fs::write(root.join("new"), b"new file\n").unwrap();
    git(root, &["add", "file", "new"], b"");
    git(root, &["commit", "-m", "mail subject\n\nmail body"], b"");
    let expected = git(root, &["rev-parse", "HEAD"], b"");
    let mail = git(root, &["format-patch", "--stdout", "--binary", "-1"], b"");
    git(root, &["checkout", "-b", "destination", base], b"");
    fs::write(root.join("new"), b"untracked obstruction\n").unwrap();
    assert!(!git_output(root, &["am"], &mail).status.success());
    fs::remove_file(root.join("new")).unwrap();
    assert_eq!(fs::read(root.join("file")).unwrap(), b"base\n");
    expected
}
