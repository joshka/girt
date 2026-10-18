//! Patches and blame under `.gitattributes` and diff drivers, compared with `git diff` and
//! `git blame` on original fixtures.
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use girt::attributes::{DirectorySource, State};
use girt::patch::{External, Limits, Options};
use girt::tree::TreeEditor;
use girt::workflow::Environment;
use girt::{InitKind, ObjectFormat, ObjectId, Repo, Repository};
use pack_git::git;
use rstest::rstest;

/// Paths and blob bytes of one tree.
type Spec<'a> = &'a [(&'a str, &'a [u8])];

const RUST_OLD: &[u8] = b"impl Tool {\n    fn run(&self) {\n        one();\n        two();\n        three();\n        four();\n        five();\n    }\n}\n";
const RUST_NEW: &[u8] = b"impl Tool {\n    fn run(&self) {\n        one();\n        two();\n        three();\n        four();\n        FIVE();\n    }\n}\n";
const SECTIONS_OLD: &[u8] = b"BEGIN alpha\nx1\nEND\nBEGIN beta\nx2\nx3\nx4\nx5\nx6\nx7\n";
const SECTIONS_NEW: &[u8] = b"BEGIN alpha\nx1\nEND\nBEGIN beta\nx2\nx3\nx4\nx5\nx6\nX7\n";
const NUL_OLD: &[u8] = b"head\0\none\ntwo\n";
const NUL_NEW: &[u8] = b"head\0\none\nthree\n";

struct Fixture {
    _root: tempfile::TempDir,
    worktree: PathBuf,
    old: ObjectId,
    new: ObjectId,
}

fn tree(repo: &Repository, spec: Spec<'_>) -> ObjectId {
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    for (path, bytes) in spec {
        editor
            .set(path.as_bytes(), loose.write_blob(bytes).unwrap())
            .unwrap();
    }
    editor.write().unwrap()
}

/// A repository with a working tree holding `files` (such as `.gitattributes`), two trees to
/// compare, and `config` set with `git config`. No global or system attribute file applies.
fn fixture(old: Spec<'_>, new: Spec<'_>, files: Spec<'_>, config: &[(&str, &str)]) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("w");
    let repo = Repository::init(ObjectFormat::Sha1, &worktree, InitKind::Worktree).unwrap();
    let (old, new) = (tree(&repo, old), tree(&repo, new));
    for (path, bytes) in files {
        let path = worktree.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    git(&worktree, &["config", "core.attributesFile", "absent"], b"");
    for (key, value) in config {
        git(&worktree, &["config", key, value], b"");
    }
    Fixture {
        _root: root,
        worktree,
        old,
        new,
    }
}

fn environment() -> Environment {
    Environment::default().with("GIT_ATTR_NOSYSTEM", "1")
}

fn handle(directory: &Path) -> Repo {
    Repo::new(Repository::open(directory).unwrap()).with_environment(environment())
}

fn ours(fixture: &Fixture) -> Vec<u8> {
    let mut out = Vec::new();
    handle(&fixture.worktree)
        .write_patch(&mut out, fixture.old, fixture.new)
        .unwrap();
    out
}

fn theirs(fixture: &Fixture, flags: &[&str]) -> Vec<u8> {
    let (old, new) = (fixture.old.to_string(), fixture.new.to_string());
    let mut args = vec!["diff"];
    args.extend(flags);
    args.extend([old.as_str(), new.as_str()]);
    git(&fixture.worktree, &args, b"")
}

fn assert_same(ours: &[u8], theirs: &[u8]) {
    assert!(
        ours == theirs,
        "girt:\n{}\ngit:\n{}",
        String::from_utf8_lossy(ours),
        String::from_utf8_lossy(theirs)
    );
}

/// Binary classification and function context follow the worktree attributes and drivers.
#[rstest]
#[case::binary_macro(b"f binary\n", &[], ("f", RUST_OLD, RUST_NEW))]
#[case::unset_diff(b"f -diff\n", &[], ("f", RUST_OLD, RUST_NEW))]
#[case::set_diff_forces_text(b"f diff\n", &[], ("f", NUL_OLD, NUL_NEW))]
#[case::driver_binary_true(b"f diff=d\n", &[("diff.d.binary", "true")], ("f", RUST_OLD, RUST_NEW))]
#[case::driver_binary_false(b"f diff=d\n", &[("diff.d.binary", "false")], ("f", NUL_OLD, NUL_NEW))]
#[case::unconfigured_driver_detects(b"f diff=d\n", &[], ("f", NUL_OLD, NUL_NEW))]
#[case::text_does_not_force(b"f text\n", &[], ("f", NUL_OLD, NUL_NEW))]
#[case::unset_text_is_not_binary(b"f -text\n", &[], ("f", RUST_OLD, RUST_NEW))]
#[case::extended_group(b"f diff=s\n", &[("diff.s.xfuncname", "^BEGIN (.*)")], ("f", SECTIONS_OLD, SECTIONS_NEW))]
#[case::extended_whole(b"f diff=s\n", &[("diff.s.xfuncname", "BEGIN.*")], ("f", SECTIONS_OLD, SECTIONS_NEW))]
#[case::extended_negation(b"f diff=s\n", &[("diff.s.xfuncname", "!beta\n^BEGIN .*")], ("f", SECTIONS_OLD, SECTIONS_NEW))]
#[case::extended_no_match(b"f diff=s\n", &[("diff.s.xfuncname", "NOTHING")], ("f", SECTIONS_OLD, SECTIONS_NEW))]
#[case::basic(b"f diff=s\n", &[("diff.s.funcname", "^BEGIN \\(a.*\\)")], ("f", SECTIONS_OLD, SECTIONS_NEW))]
#[case::last_pattern_wins(b"f diff=s\n", &[("diff.s.funcname", "^BEGIN \\(a.*\\)"), ("diff.s.xfuncname", "^END")], ("f", SECTIONS_OLD, SECTIONS_NEW))]
#[case::builtin_rust(b"*.rs diff=rust\n", &[], ("lib.rs", RUST_OLD, RUST_NEW))]
#[case::configured_rust_overrides(b"*.rs diff=rust\n", &[("diff.rust.xfuncname", "^impl.*")], ("lib.rs", RUST_OLD, RUST_NEW))]
#[case::default_without_driver(b"", &[], ("lib.rs", RUST_OLD, RUST_NEW))]
fn root_attributes_match_git(
    #[case] attributes: &[u8],
    #[case] config: &[(&str, &str)],
    #[case] (path, old, new): (&str, &[u8], &[u8]),
) {
    let fixture = fixture(
        &[(path, old)],
        &[(path, new)],
        &[(".gitattributes", attributes)],
        config,
    );
    assert_same(&ours(&fixture), &theirs(&fixture, &[]));
}

/// Nested files override the root, `info/attributes` overrides both, and each rename side uses
/// its own path.
#[test]
fn precedence_and_renames_match_git() {
    let old: Spec<'_> = &[
        ("sub/a.rs", RUST_OLD),
        ("sub/deep/b.rs", RUST_OLD),
        ("info.rs", RUST_OLD),
        ("from.rs", SECTIONS_OLD),
        ("text.txt", NUL_OLD),
    ];
    let new: Spec<'_> = &[
        ("sub/a.rs", RUST_NEW),
        ("sub/deep/b.rs", RUST_NEW),
        ("info.rs", RUST_NEW),
        ("to.bin", SECTIONS_NEW),
        ("text.txt", NUL_NEW),
    ];
    let fixture = fixture(
        old,
        new,
        &[
            (
                ".gitattributes",
                b"*.rs diff=rust\n*.bin -diff\n*.txt diff\n",
            ),
            ("sub/.gitattributes", b"a.rs -diff\n"),
            ("sub/deep/.gitattributes", b"/b.rs diff\n"),
            (".git/info/attributes", b"info.rs binary\n"),
        ],
        &[],
    );
    assert_same(&ours(&fixture), &theirs(&fixture, &[]));
}

/// The old side's function pattern is used, falling back to the new side's.
#[test]
fn rename_function_pattern_matches_git() {
    let fixture = fixture(
        &[("a.x", SECTIONS_OLD), ("c.txt", RUST_OLD)],
        &[("b.y", SECTIONS_NEW), ("d.rs", RUST_NEW)],
        &[(
            ".gitattributes",
            b"*.x diff=x\n*.y diff=y\n*.rs diff=rust\n",
        )],
        &[
            ("diff.x.xfuncname", "^BEGIN alpha"),
            ("diff.y.xfuncname", "^BEGIN beta"),
        ],
    );
    assert_same(&ours(&fixture), &theirs(&fixture, &[]));
}

/// Git reads the worktree, not the compared trees: committed attributes alone do not apply.
#[test]
fn committed_attributes_alone_do_not_apply() {
    let fixture = fixture(
        &[("f", RUST_OLD)],
        &[("f", RUST_NEW), (".gitattributes", b"f binary\n")],
        &[],
        &[],
    );
    assert_same(&ours(&fixture), &theirs(&fixture, &[]));
}

/// `attr.tree` makes Git read attributes from that tree instead of the worktree.
#[test]
fn attr_tree_replaces_the_worktree() {
    let fixture = fixture(
        &[("f", RUST_OLD), ("g", RUST_OLD)],
        &[
            ("f", RUST_NEW),
            ("g", RUST_NEW),
            (".gitattributes", b"f binary\n"),
        ],
        &[(".gitattributes", b"g binary\n")],
        &[],
    );
    let new = fixture.new.to_string();
    git(&fixture.worktree, &["config", "attr.tree", &new], b"");
    assert_same(&ours(&fixture), &theirs(&fixture, &[]));
}

/// A bare repository reads only `info/attributes` (and global files).
#[test]
fn bare_repository_matches_git() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("b.git");
    let repo = Repository::init(ObjectFormat::Sha1, &directory, InitKind::Bare).unwrap();
    let old = tree(&repo, &[("f", RUST_OLD), ("g", RUST_OLD)]);
    let new = tree(&repo, &[("f", RUST_NEW), ("g", RUST_NEW)]);
    std::fs::create_dir_all(directory.join("info")).unwrap();
    std::fs::write(directory.join("info/attributes"), b"f -diff\n").unwrap();
    git(
        &directory,
        &["config", "core.attributesFile", "absent"],
        b"",
    );
    let mut patch = Vec::new();
    handle(&directory)
        .write_patch(&mut patch, old, new)
        .unwrap();
    let expected = git(
        &directory,
        &["diff", &old.to_string(), &new.to_string()],
        b"",
    );
    assert_same(&patch, &expected);
}

/// A symlinked `.gitattributes` in the worktree is skipped, as Git skips it.
#[cfg(unix)]
#[test]
fn symlinked_worktree_file_is_ignored_as_by_git() {
    let fixture = fixture(
        &[("f", RUST_OLD)],
        &[("f", RUST_NEW)],
        &[("elsewhere", b"f binary\n")],
        &[],
    );
    std::os::unix::fs::symlink("elsewhere", fixture.worktree.join(".gitattributes")).unwrap();
    assert_same(&ours(&fixture), &theirs(&fixture, &[]));
}

/// girt never runs textconv: the handle refuses it, and the explicit form can ignore it as
/// `--no-textconv` does.
#[test]
fn textconv_is_refused_or_ignored() {
    let fixture = fixture(
        &[("f", RUST_OLD)],
        &[("f", RUST_NEW)],
        &[(".gitattributes", b"f diff=up\n")],
        &[
            ("diff.up.textconv", "tr a-z A-Z"),
            ("diff.up.xfuncname", "^impl.*"),
        ],
    );
    let error = handle(&fixture.worktree)
        .write_patch(Vec::new(), fixture.old, fixture.new)
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<girt::patch::Error>(),
        Some(girt::patch::Error::External {
            kind: "textconv",
            ..
        })
    ));

    let repo = Repository::open(&fixture.worktree).unwrap();
    let objects = repo.objects().unwrap();
    let entries = objects
        .diff_trees(Some(fixture.old), Some(fixture.new))
        .unwrap()
        .entries;
    let mut attributes = repo.diff_attributes(&environment()).unwrap();
    let mut patch = Vec::new();
    objects
        .write_patch_with(
            &mut patch,
            &entries,
            Options::default().with_external(External::Ignore),
            Some(&mut attributes),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_same(&patch, &theirs(&fixture, &["--no-textconv"]));
}

/// Blame refuses a textconv driver and is unaffected by `binary`, as in `git blame`.
#[test]
fn blame_applies_textconv_policy() {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("w");
    Repository::init(ObjectFormat::Sha1, &worktree, InitKind::Worktree).unwrap();
    std::fs::write(worktree.join("conv"), b"a\nb\n").unwrap();
    std::fs::write(worktree.join("bin"), b"a\nb\n").unwrap();
    std::fs::write(
        worktree.join(".gitattributes"),
        b"conv diff=up\nbin binary\n",
    )
    .unwrap();
    git(&worktree, &["config", "core.attributesFile", "absent"], b"");
    git(&worktree, &["config", "diff.up.textconv", "cat"], b"");
    git(&worktree, &["add", "."], b"");
    git(&worktree, &["commit", "-qm", "files"], b"");
    let handle = handle(&worktree);
    let error = handle.blame("HEAD", b"conv").unwrap_err();
    assert!(matches!(
        error.downcast_ref::<girt::repo::Error>(),
        Some(girt::repo::Error::Textconv { driver, .. }) if driver == b"up"
    ));
    assert_eq!(handle.blame("HEAD", b"bin").unwrap()[0].line_count, 2);
}

/// Attributes from a tree source are read lazily per directory, as `git check-attr
/// --source` reports them.
#[test]
fn tree_source_matches_check_attr() {
    let fixture = fixture(
        &[],
        &[
            (".gitattributes", b"*.rs diff=rust\n"),
            ("a/.gitattributes", b"x.rs -diff\n"),
            ("a/x.rs", b""),
        ],
        &[],
        &[],
    );
    let repo = Repository::open(&fixture.worktree).unwrap();
    let mut lookup = repo
        .attributes(&environment(), DirectorySource::Tree(fixture.new))
        .unwrap();
    let new = fixture.new.to_string();
    let reported = git(
        &fixture.worktree,
        &[
            "check-attr",
            "--source",
            &new,
            "diff",
            "--",
            "a/x.rs",
            "b/y.rs",
        ],
        b"",
    );
    assert_eq!(reported, b"a/x.rs: diff: unset\nb/y.rs: diff: rust\n");
    assert_eq!(lookup.get(b"a/x.rs", b"diff").unwrap(), State::Unset);
    assert_eq!(
        lookup.get(b"b/y.rs", b"diff").unwrap(),
        State::Value(b"rust")
    );
}
