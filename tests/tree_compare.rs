//! Original trees generated through Git's public CLI; no upstream source or fixtures are used.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::{EntryMode, ObjectId, PackLimits, Repository, TreeChange, TreeCompareLimits, TreeValue};
use rstest::rstest;

fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn id(output: &[u8]) -> ObjectId {
    std::str::from_utf8(output).unwrap().trim().parse().unwrap()
}

fn tree(root: &Path, entries: &[(&str, ObjectId, &[u8])]) -> ObjectId {
    let mut input = Vec::new();
    for (mode, id, name) in entries {
        let kind = match *mode {
            "040000" => "tree",
            "160000" => "commit",
            _ => "blob",
        };
        input.extend_from_slice(format!("{mode} {kind} {id}\t").as_bytes());
        input.extend_from_slice(name);
        input.push(0);
    }
    id(&git(root, &["mktree", "-z", "--missing"], &input))
}

struct Fixture {
    root: tempfile::TempDir,
    old: ObjectId,
    new: ObjectId,
    empty: ObjectId,
}

impl Fixture {
    fn new(packed: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        git(
            path,
            &["init", "--bare", "--object-format=sha1", "--template=", "."],
            b"",
        );
        let a = id(&git(path, &["hash-object", "-w", "--stdin"], b"first\n"));
        let b = id(&git(path, &["hash-object", "-w", "--stdin"], b"second\n"));
        let empty = tree(path, &[]);
        let nested_a = tree(path, &[("100644", a, b"leaf"), ("100644", a, b"same")]);
        let nested_b = tree(path, &[("100644", b, b"leaf"), ("100644", a, b"same")]);
        // Gitlinks intentionally refer to absent foreign commits.
        let link_a = ObjectId::Sha1([3; 20]);
        let link_b = ObjectId::Sha1([4; 20]);
        let old = tree(
            path,
            &[
                ("100644", a, b"a"),
                ("100644", a, b"a.c"),
                ("040000", nested_a, b"nested"),
                ("100644", a, b"gone"),
                ("100644", a, b"exec"),
                ("100644", a, b"type"),
                ("120000", a, b"symlink"),
                ("160000", link_a, b"submodule"),
                ("100644", a, b"\xff\n\t"),
                ("040000", nested_a, b"z"),
            ],
        );
        let new = tree(
            path,
            &[
                ("040000", nested_b, b"a"),
                ("100644", b, b"a.c"),
                ("100644", b, b"a0"),
                ("040000", nested_b, b"nested"),
                ("040000", empty, b"empty"),
                ("100755", a, b"exec"),
                ("120000", a, b"type"),
                ("120000", b, b"symlink"),
                ("160000", link_b, b"submodule"),
                ("100644", b, b"\xff\n\t"),
                ("100644", b, b"z"),
            ],
        );
        if packed {
            // Explicit roots retain tree-only histories without manufacturing commits or refs.
            git(
                path,
                &["pack-objects", "--revs", "objects/pack/fixture"],
                format!("{old}\n{new}\n").as_bytes(),
            );
            git(path, &["prune-packed"], b"");
        }
        Self {
            root,
            old,
            new,
            empty,
        }
    }

    fn compare(&self, old: Option<ObjectId>, new: Option<ObjectId>) -> Vec<TreeChange> {
        Repository::open(self.root.path())
            .unwrap()
            .objects(PackLimits::default())
            .unwrap()
            .compare_trees(
                old,
                new,
                TreeCompareLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap()
    }

    fn expected(&self, old: ObjectId, new: ObjectId) -> Vec<TreeChange> {
        let output = git(
            self.root.path(),
            &[
                "diff-tree",
                "--raw",
                "-r",
                "-z",
                "--no-renames",
                "--no-commit-id",
                "--no-abbrev",
                "--no-ext-diff",
                "--ignore-submodules=none",
                &old.to_string(),
                &new.to_string(),
            ],
            b"",
        );
        let fields: Vec<_> = output
            .split(|&b| b == 0)
            .filter(|field| !field.is_empty())
            .collect();
        let (records, remainder) = fields.as_chunks::<2>();
        assert!(remainder.is_empty());
        let mut changes: Vec<_> = records
            .iter()
            .map(|record| {
                let header: Vec<_> = std::str::from_utf8(record[0])
                    .unwrap()
                    .split_whitespace()
                    .collect();
                TreeChange {
                    path: record[1].to_vec(),
                    old: raw_value(&header[0][1..], header[2]),
                    new: raw_value(header[1], header[3]),
                }
            })
            .collect();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }
}

fn raw_value(mode: &str, id: &str) -> Option<TreeValue> {
    let mode = match mode {
        "000000" => return None,
        "100644" => EntryMode::Blob,
        "100755" => EntryMode::Executable,
        "120000" => EntryMode::Symlink,
        "160000" => EntryMode::Gitlink,
        _ => panic!("unexpected leaf mode {mode}"),
    };
    Some(TreeValue {
        mode,
        id: id.parse().unwrap(),
    })
}

#[rstest]
#[case::loose(false)]
#[case::packed(true)]
fn structural_results_agree_with_git(#[case] packed: bool) {
    let f = Fixture::new(packed);
    assert_eq!(
        f.compare(Some(f.old), Some(f.new)),
        f.expected(f.old, f.new)
    );
    assert_eq!(
        f.compare(Some(f.new), Some(f.old)),
        f.expected(f.new, f.old)
    );
    assert_eq!(f.compare(None, Some(f.new)), f.expected(f.empty, f.new));
    assert_eq!(f.compare(Some(f.old), None), f.expected(f.old, f.empty));
    assert!(f.compare(Some(f.old), Some(f.old)).is_empty());
}
