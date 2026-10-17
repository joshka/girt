//! Git display helpers compared with the Git executable's own output.
#[path = "support/rewrite_git.rs"]
mod fixture;

use std::sync::atomic::AtomicBool;

use fixture::{Fixture, git, id};
use girt::display::{DateFormat, quote_path};
use girt::rewrites::{Limits, Options};
use girt::{EntryMode, ObjectId, ReadLimits};
use rstest::rstest;

/// Builds a tree from raw entries with `git mktree`, which also checks each mode's object kind.
fn tree(f: &Fixture, entries: &[(EntryMode, ObjectId, &[u8])]) -> ObjectId {
    let mut rows = Vec::new();
    for (mode, id, name) in entries {
        let kind = mode.object_kind().as_str();
        rows.extend_from_slice(format!("{} {kind} {id}\t", mode.octal()).as_bytes());
        rows.extend_from_slice(name);
        rows.push(0);
    }
    id(&git(f.root.path(), &["mktree", "-z", "--missing"], &rows))
}

fn lines(bytes: Vec<u8>) -> Vec<String> {
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Every byte Git allows in a tree entry name quotes as `git ls-tree` prints it.
#[test]
fn quote_path_matches_git_for_every_byte() {
    let f = Fixture::new("sha1");
    let blob = f.blob(b"x");
    let names: Vec<Vec<u8>> = (1..=u8::MAX)
        .filter(|&byte| byte != b'/')
        .map(|byte| vec![b'n', byte])
        .collect();
    let entries: Vec<_> = names
        .iter()
        .map(|name| (EntryMode::Blob, blob, name.as_slice()))
        .collect();
    let tree = tree(&f, &entries).to_string();
    let mut raw: Vec<Vec<u8>> = git(f.root.path(), &["ls-tree", "-z", "--name-only", &tree], b"")
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    raw.sort();
    let mut expected = names;
    expected.sort();
    assert_eq!(raw, expected);

    let quoted = lines(git(f.root.path(), &["ls-tree", "--name-only", &tree], b""));

    let ours: Vec<String> = raw
        .iter()
        .map(|name| quote_path(name).to_string())
        .collect();
    assert_eq!(ours, quoted);
}

/// Octal modes and kind names match `git ls-tree` for every entry mode.
#[test]
fn entry_modes_match_git_ls_tree() {
    let f = Fixture::new("sha1");
    let blob = f.blob(b"x");
    let subtree = tree(&f, &[(EntryMode::Blob, blob, b"inner")]);
    let commit = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"stand-in commit id");
    let entries = [
        (EntryMode::Blob, blob, &b"blob"[..]),
        (EntryMode::Executable, blob, b"executable"),
        (EntryMode::Gitlink, commit, b"gitlink"),
        (EntryMode::Symlink, blob, b"symlink"),
        (EntryMode::Tree, subtree, b"tree"),
    ];
    let tree = tree(&f, &entries).to_string();

    let listed = lines(git(f.root.path(), &["ls-tree", &tree], b""));

    let ours: Vec<String> = entries
        .iter()
        .map(|(mode, id, name)| {
            let kind = mode.object_kind().as_str();
            format!("{} {kind} {id}\t{}", mode.octal(), quote_path(name))
        })
        .collect();
    assert_eq!(ours, listed);
}

/// Status letters for tree changes and rewrites match `git diff-tree --name-status`.
#[test]
fn change_letters_match_git_diff_tree() {
    let f = Fixture::new("sha1");
    let one = f.blob(b"one\n");
    let two = f.blob(b"two\n");
    let moved = f.blob(b"moved content\n");
    let commit = ObjectId::for_blob(girt::ObjectFormat::Sha1, b"stand-in commit id");
    let old = tree(
        &f,
        &[
            (EntryMode::Blob, one, &b"content"[..]),
            (EntryMode::Blob, one, b"deleted"),
            (EntryMode::Blob, one, b"executable"),
            (EntryMode::Blob, one, b"gitlink"),
            (EntryMode::Blob, moved, b"old name"),
            (EntryMode::Blob, one, b"symlink"),
            (EntryMode::Blob, one, b"tab\there"),
        ],
    );
    let new = tree(
        &f,
        &[
            (EntryMode::Blob, one, &b"added"[..]),
            (EntryMode::Blob, two, b"content"),
            (EntryMode::Executable, one, b"executable"),
            (EntryMode::Gitlink, commit, b"gitlink"),
            (EntryMode::Blob, moved, b"new name"),
            (EntryMode::Symlink, one, b"symlink"),
            (EntryMode::Blob, two, b"tab\there"),
        ],
    );
    let objects = f.objects();
    let cancel = AtomicBool::new(false);
    let (old_name, new_name) = (old.to_string(), new.to_string());
    let args = ["diff-tree", "-r", "--name-status", &old_name, &new_name];

    let plain = lines(git(
        f.root.path(),
        &[&args[..], &["--no-renames"]].concat(),
        b"",
    ));
    let renamed = lines(git(f.root.path(), &[&args[..], &["-M"]].concat(), b""));

    let changes = objects
        .compare_trees_with(Some(old), Some(new), Default::default(), &cancel)
        .unwrap();
    let ours: Vec<String> = changes
        .iter()
        .map(|change| format!("{}\t{}", change.status().letter(), quote_path(&change.path)))
        .collect();
    assert_eq!(ours, plain);
    let rewrites = objects
        .detect_rewrites(
            Some(old),
            Some(new),
            Options::default(),
            Limits::default(),
            None,
            &cancel,
        )
        .unwrap();
    let rewrite = &rewrites[0];
    let line = format!(
        "{}{:03}\t{}\t{}",
        rewrite.kind.letter(),
        rewrite.similarity,
        quote_path(&rewrite.source),
        quote_path(&rewrite.target)
    );
    assert!(renamed.contains(&line), "{line:?} not in {renamed:?}");
}

/// Dates read from commits format as `git log --date=default` and `--date=iso` print them.
#[rstest]
#[case::epoch("0 +0000")]
#[case::half_hour("1700000000 +0530")]
#[case::west_single_digit_day("1696320000 -0800")]
#[case::leap_day("1709210096 +1400")]
#[case::negative_zero("4102444800 -0000")]
#[case::short_zone("1700000000 +130")]
#[case::year_9999("253402300799 +0000")]
#[case::noncanonical_east("1700000000 +0090")]
#[case::noncanonical_west("1700000000 -0199")]
#[case::noncanonical_carry("1700000000 +9999")]
#[case::local_epoch("3600 -0100")]
#[case::padded_zone("1700000000 +000090")]
#[case::five_digit_zone("1700000000 +12345")]
#[case::absent_default_zone("1700000000 -0001")]
#[case::epoch_absent_zone("60 -0001")]
fn dates_match_git_log(#[case] date: &str) {
    let f = Fixture::new("sha1");
    let tree = f.tree(&[]);
    let raw = format!(
        "tree {tree}\nauthor A <a@example.com> {date}\ncommitter C <c@example.com> 1 +0000\n\nm\n"
    );
    let commit = id(&git(
        f.root.path(),
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            "--stdin",
        ],
        raw.as_bytes(),
    ));
    let commit_name = commit.to_string();
    let log = |format: &str| {
        let date = format!("--date={format}");
        let output = git(
            f.root.path(),
            &["log", "-1", "--format=%ad", &date, &commit_name],
            b"",
        );
        String::from_utf8(output).unwrap().trim_end().to_owned()
    };

    let parsed = f
        .objects()
        .read_commit_with(commit, ReadLimits::default())
        .unwrap();
    let date = parsed.author().unwrap().unwrap();

    assert_eq!(
        date.format_date(DateFormat::Default).unwrap().to_string(),
        log("default")
    );
    assert_eq!(
        date.format_date(DateFormat::Iso).unwrap().to_string(),
        log("iso")
    );
}

/// Original raw commits straddle Git 2.56's local epoch refusal boundary.
#[rstest]
#[case::epoch_west("0 -0001")]
#[case::one_second_before("59 -0001")]
fn pre_epoch_local_dates_are_refused_like_git(#[case] date: &str) {
    let f = Fixture::new("sha1");
    let tree = f.tree(&[]);
    let raw = format!(
        "tree {tree}\nauthor A <a@example.com> {date}\ncommitter C <c@example.com> 1 +0000\n\nm\n"
    );
    let commit = id(&git(
        f.root.path(),
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            "--stdin",
        ],
        raw.as_bytes(),
    ));
    let parsed = f.objects().read_commit(commit).unwrap();
    let author = parsed.author().unwrap().unwrap();
    let observe = |format: &str| {
        std::process::Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", f.root.path().join("absent-config"))
            .env("LC_ALL", "C")
            .current_dir(f.root.path())
            .args(["log", "-1", "--format=%ad", format, &commit.to_string()])
            .output()
            .unwrap()
    };
    let default = observe("--date=default");
    let iso = observe("--date=iso");
    assert!(!default.status.success());
    assert!(!iso.status.success());
    assert!(String::from_utf8_lossy(&default.stderr).contains("Timestamp before Unix epoch"));
    assert!(String::from_utf8_lossy(&iso.stderr).contains("Timestamp before Unix epoch"));
    assert!(matches!(
        author.format_date(DateFormat::Default),
        Err(girt::CommitError::InvalidDate)
    ));
    assert!(matches!(
        author.format_date(DateFormat::Iso),
        Err(girt::CommitError::InvalidDate)
    ));
}

/// Git prints no date for signed seconds; the recorded formatter explicitly refuses them.
#[rstest]
#[case::negative_with_positive_local("-60 +0100")]
#[case::explicit_plus("+5 +0000")]
fn signed_seconds_are_explicitly_refused(#[case] date: &str) {
    let f = Fixture::new("sha1");
    let tree = f.tree(&[]);
    let raw = format!(
        "tree {tree}\nauthor A <a@example.com> {date}\ncommitter C <c@example.com> 1 +0000\n\nm\n"
    );
    let commit = id(&git(
        f.root.path(),
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            "--stdin",
        ],
        raw.as_bytes(),
    ));
    let parsed = f.objects().read_commit(commit).unwrap();
    let author = parsed.author().unwrap().unwrap();
    assert_eq!(
        git(
            f.root.path(),
            &[
                "log",
                "-1",
                "--format=%ad",
                "--date=default",
                &commit.to_string()
            ],
            b""
        ),
        b"\n"
    );
    assert_eq!(
        git(
            f.root.path(),
            &[
                "log",
                "-1",
                "--format=%ad",
                "--date=iso",
                &commit.to_string()
            ],
            b""
        ),
        b"\n"
    );
    assert!(matches!(
        author.format_date(DateFormat::Default),
        Err(girt::CommitError::InvalidDate)
    ));
    assert!(matches!(
        author.format_date(DateFormat::Iso),
        Err(girt::CommitError::InvalidDate)
    ));
}
