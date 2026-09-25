//! Original both-format fixtures generated through Git, without upstream source or test data.
#[path = "support/layout_git.rs"]
mod git;
use std::fs;

use girt::refs::{
    Expected, LogOutcome, RefEdit, RefName, ReferenceError, Reflog, ReflogRecord, Target,
    TransactionError,
};
use girt::{ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git::git(
        root.path(),
        &["init", "--bare", &format!("--object-format={}", format)],
        b"",
    );
    let tree = git::git(root.path(), &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let tip = git::git(root.path(), &["commit-tree", tree], b"original fixture\n");
    let tip = ObjectId::from_hex(format, std::str::from_utf8(&tip).unwrap().trim()).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, tip)
}
fn name(s: &str) -> RefName {
    RefName::new(s).unwrap()
}
fn append() -> Reflog {
    Reflog::Append {
        committer: Signature {
            name: b"A".to_vec(),
            email: b"a@b".to_vec(),
            seconds: 1,
            offset_minutes: 0,
        },
        message: b"append".to_vec(),
    }
}
fn edit(tip: ObjectId, reference: &str) -> RefEdit {
    RefEdit {
        name: name(reference),
        dereference: false,
        target: Some(Target::Direct(tip)),
        expected: Expected::AbsentOr(Target::Direct(tip)),
        reflog: append(),
    }
}

#[rstest]
#[case::canonical(b"A <a@b> 1 +0000", b"A", 0)]
#[case::noncanonical_minutes(b"A <a@b> 1 +0060", b"A", 60)]
#[case::empty_name(b" <a@b> 1 +0000", b"", 0)]
#[case::padded_name(b"A  <a@b> 1 +0000", b"A", 0)]
#[case::large_zone(b"A <a@b> 1 +2460", b"A", 1500)]
#[case::empty_email(b"A <> 1 +0000", b"A", 0)]
fn imported_records_preserve_bytes_and_allow_append(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] identity: &[u8],
    #[case] expected_name: &[u8],
    #[case] offset: i16,
) {
    let (root, repo, tip) = fixture(format);
    git::git(root.path(), &["update-ref", "HEAD", &tip.to_string()], b"");
    fs::create_dir_all(root.path().join("logs")).unwrap();
    let bytes = [
        format!("{} {tip} ", ObjectId::null(format)).as_bytes(),
        identity,
        b"\tfixture\n",
    ]
    .concat();
    fs::write(root.path().join("logs/HEAD"), &bytes).unwrap();
    let shown = git::git(
        root.path(),
        &["reflog", "show", "--format=%H %gn %gs", "HEAD"],
        b"",
    );
    assert_eq!(
        shown,
        [format!("{tip} ").as_bytes(), expected_name, b" fixture\n"].concat()
    );
    let records = ReflogRecord::parse(format, &bytes).unwrap();
    assert_eq!(records[0].as_bytes(), bytes);
    assert_eq!(records[0].entry().committer.name, expected_name);
    assert_eq!(records[0].entry().committer.offset_minutes, offset);
    let mut head = edit(tip, "HEAD");
    head.expected = Expected::Exists;
    repo.references().unwrap().transaction(&[head]).unwrap();
    let after = fs::read(root.path().join("logs/HEAD")).unwrap();
    assert!(after.starts_with(&bytes));
    assert_eq!(
        repo.references()
            .unwrap()
            .reflog(&name("HEAD"))
            .unwrap()
            .unwrap()
            .len(),
        2
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn packed_and_shadowed_deletion_removes_selected_history(#[case] format: ObjectFormat) {
    let (root, repo, tip) = fixture(format);
    let refs = repo.references().unwrap();
    refs.transaction(&[
        edit(tip, "refs/heads/packed"),
        edit(tip, "refs/heads/shadow"),
    ])
    .unwrap();
    git::git(root.path(), &["pack-refs", "--all", "--prune"], b"");
    refs.update_without_reflog(
        &name("refs/heads/shadow"),
        Target::Direct(tip),
        Expected::Exists,
    )
    .unwrap();
    let mut packed = edit(tip, "refs/heads/packed");
    packed.target = None;
    packed.expected = Expected::Exists;
    packed.reflog = Reflog::Delete;
    let mut shadow = packed.clone();
    shadow.name = name("refs/heads/shadow");
    let result = refs.transaction(&[packed, shadow]).unwrap();
    assert_eq!(result[0].logs[0].1, LogOutcome::Deleted);
    assert_eq!(result[1].logs[0].1, LogOutcome::Deleted);
    assert_eq!(
        git::git(root.path(), &["for-each-ref", "--format=%(refname)"], b""),
        b""
    );
    assert!(!root.path().join("logs/refs/heads/packed").exists());
    assert!(!root.path().join("logs/refs/heads/shadow").exists());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn conditional_keep_race_has_one_value_and_git_observes_it(#[case] format: ObjectFormat) {
    let (root, repo, tip) = fixture(format);
    let other = git::git(
        root.path(),
        &[
            "commit-tree",
            String::from_utf8(git::git(root.path(), &["mktree"], b""))
                .unwrap()
                .trim(),
        ],
        b"different\n",
    );
    let other = ObjectId::from_hex(format, std::str::from_utf8(&other).unwrap().trim()).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let run = |id| {
            barrier.wait();
            repo.references()
                .unwrap()
                .transaction(&[edit(id, "refs/jj/keep")])
        };
        let a = scope.spawn(move || run(tip));
        let b = scope.spawn(move || run(other));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(a.is_ok(), b.is_ok());
    let failure = a.as_ref().err().or(b.as_ref().err()).unwrap();
    assert!(matches!(
        failure,
        TransactionError::Prepare {
            source: ReferenceError::Locked(_) | ReferenceError::Mismatch { .. },
            ..
        }
    ));
    let stored = repo
        .references()
        .unwrap()
        .resolve(&name("refs/jj/keep"), 0)
        .unwrap()
        .id
        .unwrap();
    assert_eq!(
        git::git(root.path(), &["rev-parse", "refs/jj/keep"], b""),
        format!("{stored}\n").as_bytes()
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .reflog(&name("refs/jj/keep"))
            .unwrap()
            .unwrap()
            .len(),
        1
    );
}

#[cfg(windows)]
#[rstest]
#[case::device("refs/heads/CON")]
#[case::device_extension("refs/heads/nul.txt")]
#[case::trailing_dot("refs/heads/a./b")]
#[case::pipe("refs/heads/a|b")]
fn windows_storage_refuses_filename_aliases(#[case] reference: &str) {
    let (_root, repo, tip) = fixture(ObjectFormat::Sha1);
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction(&[edit(tip, reference)]),
        Err(TransactionError::Prepare {
            source: ReferenceError::Unsupported(_),
            ..
        })
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn prepared_git_writer_excludes_conditional_girt_writer(#[case] format: ObjectFormat) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let (root, repo, tip) = fixture(format);
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "start\ncreate refs/jj/keep {tip}\nprepare").unwrap();
    input.flush().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "start: ok\n");
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "prepare: ok\n");
    let result = repo
        .references()
        .unwrap()
        .transaction(&[edit(tip, "refs/jj/keep")]);
    writeln!(input, "commit").unwrap();
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(matches!(
        result,
        Err(TransactionError::Prepare {
            source: ReferenceError::Locked(_),
            ..
        })
    ));
    assert_eq!(
        repo.references()
            .unwrap()
            .read(&name("refs/jj/keep"))
            .unwrap(),
        Some(Target::Direct(tip))
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .reflog(&name("refs/jj/keep"))
            .unwrap(),
        None
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn symbolic_unborn_head_logging_agrees_with_git(#[case] format: ObjectFormat) {
    let (root, repo, tip) = fixture(format);
    let before = fs::read(root.path().join("HEAD")).unwrap();
    git::git(
        root.path(),
        &["update-ref", "refs/heads/target", &tip.to_string()],
        b"",
    );
    fs::create_dir_all(root.path().join("logs")).unwrap();
    fs::write(root.path().join("logs/HEAD"), b"").unwrap();
    git::git(
        root.path(),
        &["symbolic-ref", "-m", "switch", "HEAD", "refs/heads/target"],
        b"",
    );
    let git_records = repo
        .references()
        .unwrap()
        .reflog(&name("HEAD"))
        .unwrap()
        .unwrap();
    fs::write(root.path().join("HEAD"), before).unwrap();
    fs::write(root.path().join("logs/HEAD"), b"").unwrap();
    let mut head = edit(tip, "HEAD");
    head.target = Some(Target::Symbolic(name("refs/heads/target")));
    head.expected = Expected::Exists;
    repo.references().unwrap().transaction(&[head]).unwrap();
    let records = repo
        .references()
        .unwrap()
        .reflog(&name("HEAD"))
        .unwrap()
        .unwrap();
    assert_eq!(records.len(), git_records.len());
    assert_eq!(records[0].old, git_records[0].old);
    assert_eq!(records[0].new, git_records[0].new);
    assert_eq!(records[0].old, ObjectId::null(format));
    assert_eq!(records[0].new, tip);
}
