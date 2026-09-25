use std::fs;
use std::path::Path;
use std::process::Command;

use rstest::rstest;

use super::{Limits, Table};
use crate::ObjectFormat;
use crate::refs::{RefName, Target};

// These fixtures are original Git CLI observations, never upstream test data.
pub(super) fn git(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub(super) fn fixture(format: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--ref-format=reftable",
            "--initial-branch=main",
            &format!("--object-format={format}"),
        ],
    );
    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "original fixture"],
    );
    root
}

fn tables(root: &Path) -> Vec<Table> {
    let directory = root.join(".git/reftable");
    let list = fs::read_to_string(directory.join("tables.list")).unwrap();
    list.lines()
        .map(|name| {
            let bytes = fs::read(directory.join(name)).unwrap();
            Table::decode(&bytes, Limits::default())
                .unwrap_or_else(|error| panic!("{name}: {error}; {bytes:02x?}"))
        })
        .collect()
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn reads_git_committed_tip_and_logs(#[case] spelling: &str, #[case] format: ObjectFormat) {
    let root = fixture(spelling);
    let tables = tables(root.path());
    let tip = git(root.path(), &["rev-parse", "HEAD"]);
    let hex = String::from_utf8(tip).unwrap();
    let id = crate::ObjectId::from_hex(format, hex.trim()).unwrap();
    let branch = RefName::new(b"refs/heads/main").unwrap();
    let record = tables
        .iter()
        .rev()
        .flat_map(|table| &table.references)
        .find(|record| record.name == branch)
        .unwrap();
    assert_eq!(record.target, Some(Target::Direct(id)));
    let log = tables
        .iter()
        .flat_map(|table| &table.logs)
        .find(|record| record.name == branch)
        .unwrap();
    assert_eq!(log.value.as_ref().unwrap().new, id);
    assert_eq!(log.value.as_ref().unwrap().name, b"Fixture");
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn reads_git_multiblock_compacted_table(#[case] format: &str) {
    let root = fixture(format);
    populate(root.path());
    git(root.path(), &["refs", "optimize"]);
    let tables = tables(root.path());
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].references.len(), 302);
}

fn populate(root: &Path) {
    use std::io::Write;
    use std::process::Stdio;
    let tip = git(root, &["rev-parse", "HEAD"]);
    let tip = String::from_utf8(tip).unwrap();
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for index in 0..300 {
        writeln!(input, "create refs/tags/fixture-{index:04} {}", tip.trim()).unwrap();
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn git_reads_reencoded_references_and_reflogs(#[case] format: &str) {
    let root = fixture(format);
    populate(root.path());
    git(root.path(), &["refs", "optimize"]);
    let before = git(root.path(), &["show-ref", "--head"]);
    let logs = git(
        root.path(),
        &["reflog", "show", "--format=%H %gn %gs", "HEAD"],
    );
    let decoded = tables(root.path());
    let bytes = decoded[0].encode(Limits::default()).unwrap();
    let directory = root.path().join(".git/reftable");
    fs::write(directory.join("original-codec.ref"), bytes).unwrap();
    fs::write(directory.join("tables.list"), b"original-codec.ref\n").unwrap();
    assert_eq!(git(root.path(), &["show-ref", "--head"]), before);
    assert_eq!(
        git(
            root.path(),
            &["reflog", "show", "--format=%H %gn %gs", "HEAD"]
        ),
        logs
    );
    git(root.path(), &["refs", "verify"]);
}

#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn reads_git_log_deletion(#[case] format: &str) {
    let root = fixture(format);
    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "second fixture"],
    );
    git(root.path(), &["reflog", "delete", "HEAD@{1}"]);
    let decoded = tables(root.path());
    assert!(!decoded.is_empty());
}
