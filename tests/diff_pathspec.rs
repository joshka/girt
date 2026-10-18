//! Path-limited tree comparison compared with `git diff-tree -r -- <pathspec>`.
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::pathspec::{Context, Pathspec};
use girt::tree_diff::Entry;
use girt::{EntryMode, ObjectId, Repository, Tree, TreeEntry};

fn git(directory: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(directory)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Diff Fixture")
        .env("GIT_AUTHOR_EMAIL", "diff@example.invalid")
        .env("GIT_COMMITTER_NAME", "Diff Fixture")
        .env("GIT_COMMITTER_EMAIL", "diff@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

fn tree(root: &Path, revision: &str) -> ObjectId {
    let id = git(root, &["rev-parse", &format!("{revision}^{{tree}}")]);
    String::from_utf8(id).unwrap().trim().parse().unwrap()
}

const SPECS: &[&[&str]] = &[
    &["."],
    &["src"],
    &["src/"],
    &["sr"],
    &["*.rs"],
    &[":(glob)*.rs"],
    &[":(glob)**/*.rs"],
    &["src/*"],
    &[":(glob)src/*"],
    &[":!src"],
    &["src", ":!src/deep"],
    &[":(icase)SRC/A.RS"],
    &["docs/guide.md"],
    &["d*"],
    &["moved/b.txt"],
    &["moved", "old"],
    &[":!old"],
    &["kind"],
    &["kind/inner"],
    &[":(literal)st[a]r"],
    &["st[a]r"],
];

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    git(root, &["init", "-q", "--template=", "."]);
    write(
        root,
        &[
            ("src/a.rs", "a\n"),
            ("src/deep/b.rs", "b\n"),
            ("src/keep.txt", "k\n"),
            ("docs/guide.md", "g\n"),
            ("old/b.txt", "moved content that is long enough to pair\n"),
            ("kind", "file\n"),
            ("st[a]r", "s\n"),
            ("stuck", "s\n"),
        ],
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "one"]);
    std::fs::remove_file(root.join("kind")).unwrap();
    std::fs::rename(root.join("old"), root.join("moved")).unwrap();
    write(
        root,
        &[
            ("src/a.rs", "a2\n"),
            ("src/deep/b.rs", "b2\n"),
            ("src/new.rs", "n\n"),
            ("docs/guide.md", "g2\n"),
            ("kind/inner", "dir\n"),
            ("st[a]r", "s2\n"),
            ("stuck", "s2\n"),
        ],
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "two"]);
    temp
}

fn lines(bytes: Vec<u8>) -> Vec<String> {
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn matches_git_diff_tree() {
    let temp = fixture();
    let root = temp.path();
    let repository = Repository::open(root).unwrap();
    let objects = repository.objects().unwrap();
    let (old, new) = (tree(root, "HEAD~1"), tree(root, "HEAD"));
    for specs in SPECS {
        let paths = Pathspec::parse(*specs, &Context::new()).unwrap();
        let cancel = AtomicBool::new(false);

        let mut args = vec!["diff-tree", "-r", "--name-status", "HEAD~1", "HEAD", "--"];
        args.extend(*specs);
        let changes = objects
            .compare_trees_matching(Some(old), Some(new), &paths, Default::default(), &cancel)
            .unwrap();
        let actual: Vec<_> = changes
            .iter()
            .map(|change| {
                format!(
                    "{}\t{}",
                    change.status().letter(),
                    String::from_utf8_lossy(&change.path)
                )
            })
            .collect();
        assert_eq!(actual, lines(git(root, &args)), "{specs:?}");

        args.insert(2, "-M");
        let comparison = objects
            .diff_trees_matching(
                Some(old),
                Some(new),
                &paths,
                Default::default(),
                Default::default(),
                &cancel,
            )
            .unwrap();
        let actual: Vec<_> = comparison
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Change(change) => format!(
                    "{}\t{}",
                    change.status().letter(),
                    String::from_utf8_lossy(&change.path)
                ),
                Entry::Rewrite {
                    kind,
                    similarity,
                    source,
                    target,
                    ..
                } => format!(
                    "{}{similarity:03}\t{}\t{}",
                    kind.letter(),
                    String::from_utf8_lossy(source),
                    String::from_utf8_lossy(target)
                ),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(actual, lines(git(root, &args)), "{specs:?} -M");
    }
}

/// A subtree outside the pathspec is never read: a missing tree object there goes unnoticed,
/// while an unlimited comparison reports it.
#[test]
fn skips_unselected_subtrees() {
    let temp = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("r"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let loose = repository.loose_objects();
    let format = girt::ObjectFormat::Sha1;
    let blob = |content: &[u8]| loose.write_blob(content).unwrap();
    let missing = |byte: u8| ObjectId::from_bytes(format, &[byte; 20]).unwrap();
    let root = |file: ObjectId, absent: ObjectId| {
        let entries = vec![
            TreeEntry {
                mode: EntryMode::Blob,
                name: b"file".to_vec(),
                id: file,
            },
            TreeEntry {
                mode: EntryMode::Tree,
                name: b"vendor".to_vec(),
                id: absent,
            },
        ];
        loose
            .write_tree(&Tree::new(format, entries).unwrap())
            .unwrap()
    };
    let old = root(blob(b"one\n"), missing(1));
    let new = root(blob(b"two\n"), missing(2));
    let objects = repository.objects().unwrap();
    let cancel = AtomicBool::new(false);
    let paths = Pathspec::parse(["file"], &Context::new()).unwrap();
    let changes = objects
        .compare_trees_matching(Some(old), Some(new), &paths, Default::default(), &cancel)
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert!(
        objects
            .compare_trees_with(Some(old), Some(new), Default::default(), &cancel)
            .is_err()
    );
}

#[test]
fn refuses_attribute_magic() {
    let temp = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        girt::ObjectFormat::Sha1,
        temp.path().join("r"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let objects = repository.objects().unwrap();
    let paths = Pathspec::parse([":(attr:text)"], &Context::new()).unwrap();
    let result = objects.compare_trees_matching(
        None,
        None,
        &paths,
        Default::default(),
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(girt::TreeCompareError::Unsupported(_))
    ));
}
