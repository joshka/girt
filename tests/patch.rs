//! Unified patches compared byte for byte with `git diff` on original tree fixtures.
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::sync::atomic::AtomicBool;

use girt::content_diff::BinaryMode;
use girt::patch::{Limits, Options};
use girt::rewrites::{self, Copies};
use girt::tree::TreeEditor;
use girt::tree_diff::Entry;
use girt::{EntryMode, InitKind, ObjectFormat, ObjectId, Repository, TreeValue};
use pack_git::git;
use rstest::rstest;

/// One tree entry: path, mode and blob bytes (a gitlink's bytes only seed its commit ID).
type Spec<'a> = &'a [(&'a [u8], EntryMode, &'a [u8])];

const fn blob(
    path: &'static [u8],
    bytes: &'static [u8],
) -> (&'static [u8], EntryMode, &'static [u8]) {
    (path, EntryMode::Blob, bytes)
}

const fn exe(
    path: &'static [u8],
    bytes: &'static [u8],
) -> (&'static [u8], EntryMode, &'static [u8]) {
    (path, EntryMode::Executable, bytes)
}

const fn link(
    path: &'static [u8],
    target: &'static [u8],
) -> (&'static [u8], EntryMode, &'static [u8]) {
    (path, EntryMode::Symlink, target)
}

const fn sub(
    path: &'static [u8],
    seed: &'static [u8],
) -> (&'static [u8], EntryMode, &'static [u8]) {
    (path, EntryMode::Gitlink, seed)
}

/// How paths are related across the two trees.
#[derive(Clone, Copy, Debug)]
enum Pairing {
    Exact,
    Renames,
    Copies,
}

const NUMBERED: &[u8] =
    b"fn one() {\n    1\n}\n\nfn two() {\n    2\n}\n\nfn three() {\n    3\n}\n\n\
fn four() {\n    4\n}\n\nfn five() {\n    5\n}\n\nfn six() {\n    6\n}\n";
const NUMBERED_EDITED: &[u8] =
    b"fn one() {\n    1\n}\n\nfn two() {\n    22\n}\n\nfn three() {\n    3\n}\n\n\
fn four() {\n    4\n}\n\nfn five() {\n    5\n}\n\nfn six() {\n    66\n}\n";
const ONE_FUNCTION: &[u8] = b"fn main() {\n    a\n    b\n    c\n    d\n    e\n    f\n    g\n    h\n    i\n    j\n    k\n    l\n    m\n}\n";
const ONE_FUNCTION_EDITED: &[u8] = b"fn main() {\n    a\n    B\n    c\n    d\n    e\n    f\n    g\n    h\n    i\n    j\n    k\n    L\n    m\n}\n";
const PROSE: &[u8] = b"alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\nindia\njuliet\n";
const PROSE_EDITED: &[u8] =
    b"alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel\nindia\njuliet\nkilo\n";

fn tree(repo: &Repository, spec: Spec<'_>) -> ObjectId {
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    for (path, mode, bytes) in spec {
        let id = match mode {
            EntryMode::Gitlink => ObjectId::for_blob(repo.object_format(), bytes),
            _ => loose.write_blob(bytes).unwrap(),
        };
        editor
            .set_entry(path, TreeValue { mode: *mode, id })
            .unwrap();
    }
    editor.write().unwrap()
}

/// Renders the girt patch for two trees and the matching `git diff` output.
fn patches(
    format: ObjectFormat,
    old: Spec<'_>,
    new: Spec<'_>,
    pairing: Pairing,
    options: Options,
    git_flags: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
    let (old, new) = (tree(&repo, old), tree(&repo, new));
    let objects = repo.objects().unwrap();
    let copies = match pairing {
        Pairing::Copies => Copies::Modified,
        _ => Copies::Disabled,
    };
    let entries: Vec<Entry> = match pairing {
        Pairing::Exact => objects
            .compare_trees(Some(old), Some(new))
            .unwrap()
            .into_iter()
            .map(Entry::Change)
            .collect(),
        Pairing::Renames | Pairing::Copies => {
            // Git pairs empty files by default.
            let options = rewrites::Options::default()
                .with_copies(copies)
                .with_track_empty(true);
            objects
                .diff_trees_with(
                    Some(old),
                    Some(new),
                    options,
                    Default::default(),
                    &AtomicBool::new(false),
                )
                .unwrap()
                .entries
        }
    };
    let mut ours = Vec::new();
    objects
        .write_patch_with(
            &mut ours,
            &entries,
            options,
            None,
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let pairing = match pairing {
        Pairing::Exact => "--no-renames",
        Pairing::Renames => "-M",
        Pairing::Copies => "-C",
    };
    let (old, new) = (old.to_string(), new.to_string());
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        if options.indent_heuristic {
            "--indent-heuristic"
        } else {
            "--no-indent-heuristic"
        },
        pairing,
    ];
    args.extend(git_flags);
    args.extend([old.as_str(), new.as_str()]);
    let theirs = git(repo.git_dir(), &args, b"");
    (ours, theirs)
}

/// Compares exact bytes; lossy text appears only in the failure message.
fn assert_same(ours: &[u8], theirs: &[u8]) {
    assert!(
        ours == theirs,
        "girt:\n{}\ngit:\n{}",
        String::from_utf8_lossy(ours),
        String::from_utf8_lossy(theirs)
    );
}

/// Every header form and content shape `git diff` prints for exact paths.
#[rstest]
#[case::text_hunks_and_function_context(
    &[blob(b"src/lib.rs", NUMBERED)],
    &[blob(b"src/lib.rs", NUMBERED_EDITED)],
)]
#[case::function_context_shared_by_hunks(
    &[blob(b"main.rs", ONE_FUNCTION)],
    &[blob(b"main.rs", ONE_FUNCTION_EDITED)],
)]
#[case::addition_and_deletion(
    &[blob(b"gone", b"old\nlines\n")],
    &[blob(b"added", b"new\nlines\n")],
)]
#[case::empty_files(
    &[blob(b"empty-old", b"")],
    &[blob(b"empty-new", b"")],
)]
#[case::mode_only(
    &[blob(b"tool", b"#!/bin/sh\n")],
    &[exe(b"tool", b"#!/bin/sh\n")],
)]
#[case::mode_and_content(
    &[blob(b"tool", b"#!/bin/sh\n")],
    &[exe(b"tool", b"#!/bin/sh\nexit 0\n")],
)]
#[case::missing_final_newlines(
    &[blob(b"a", b"one\ntwo"), blob(b"b", b"one\ntwo\n"), blob(b"c", b"x\ntail")],
    &[blob(b"a", b"one\ntwo\n"), blob(b"b", b"one\ntwo"), blob(b"c", b"y\ntail")],
)]
#[case::crlf_and_invalid_utf8(
    &[blob(b"dos.txt", b"one\r\ntwo\r\n\xff\xfe\r\n")],
    &[blob(b"dos.txt", b"one\r\nTWO\r\n\xff\xfe\r\n")],
)]
#[case::binary(
    &[blob(b"bin", b"\0old"), blob(b"bin-gone", b"\0x")],
    &[blob(b"bin", b"\0new"), blob(b"bin-new", b"\0y")],
)]
#[case::symlinks_and_type_changes(
    &[link(b"link", b"target"), blob(b"was-file", b"file\n"), link(b"was-link", b"t")],
    &[link(b"link", b"other"), link(b"was-file", b"file"), blob(b"was-link", b"t\n")],
)]
#[case::gitlinks(
    &[sub(b"sub", b"1"), sub(b"sub-gone", b"2"), sub(b"sub-to-file", b"3")],
    &[sub(b"sub", b"4"), sub(b"sub-new", b"5"), blob(b"sub-to-file", b"file\n")],
)]
#[case::spaces_in_quoted_and_binary_names(
    &[blob(b"x y\tz", b"1\n"), blob(b"caf\xc3\xa9 y", b"1\n"), blob(b"bin sp", b"\0a"), blob(b"gone sp", b"x\n")],
    &[blob(b"x y\tz", b"2\n"), blob(b"caf\xc3\xa9 y", b"2\n"), blob(b"bin sp", b"\0b"), blob(b"new sp", b"y\n")],
)]
#[case::quoted_paths(
    &[blob(b"sp ace", b"1\n"), blob(b"tab\there", b"1\n"), blob(b"caf\xc3\xa9", b"1\n"), blob(b"q\"uote", b"1\n"), blob(b"back\\slash", b"1\n")],
    &[blob(b"sp ace", b"2\n"), blob(b"tab\there", b"2\n"), blob(b"caf\xc3\xa9", b"2\n"), blob(b"q\"uote", b"2\n"), blob(b"back\\slash", b"2\n")],
)]
fn exact_paths_match_git(#[case] old: Spec<'_>, #[case] new: Spec<'_>) {
    let (ours, theirs) = patches(
        ObjectFormat::Sha1,
        old,
        new,
        Pairing::Exact,
        Options::default(),
        &[],
    );
    assert_same(&ours, &theirs);
}

/// Renames and copies, including mode changes, edits, quoting and identical content.
#[rstest]
#[case::identical_rename(
    &[blob(b"old-name", PROSE)],
    &[blob(b"new-name", PROSE)],
    Pairing::Renames,
)]
#[case::empty_rename(
    &[blob(b"empty", b""), blob(b"keep", PROSE)],
    &[blob(b"moved", b""), blob(b"keep", PROSE)],
    Pairing::Renames,
)]
#[case::edited_rename_with_mode(
    &[blob(b"old-name", PROSE)],
    &[exe(b"dir/new name", PROSE_EDITED)],
    Pairing::Renames,
)]
#[case::identical_rename_with_mode(
    &[blob(b"old-name", PROSE)],
    &[exe(b"caf\xc3\xa9", PROSE)],
    Pairing::Renames,
)]
#[case::copy_from_modified(
    &[blob(b"source", PROSE)],
    &[blob(b"source", PROSE_EDITED), blob(b"copy", PROSE)],
    Pairing::Copies,
)]
fn rewrites_match_git(#[case] old: Spec<'_>, #[case] new: Spec<'_>, #[case] pairing: Pairing) {
    let (ours, theirs) = patches(
        ObjectFormat::Sha1,
        old,
        new,
        pairing,
        Options::default(),
        &[],
    );
    assert_same(&ours, &theirs);
}

/// Context width, abbreviation and forced text match the corresponding Git options.
#[rstest]
#[case::no_context(Options::default().with_context_lines(0), &["-U0"])]
#[case::one_line(Options::default().with_context_lines(1), &["-U1"])]
#[case::merged_hunks(Options::default().with_context_lines(8), &["-U8"])]
#[case::whole_file(Options::default().with_context_lines(100), &["-U100"])]
#[case::abbrev(Options::default().with_abbrev_len(Some(12)), &["--abbrev=12"])]
#[case::text(Options::default().with_binary(BinaryMode::Text), &["--text"])]
fn options_match_git(#[case] options: Options, #[case] flags: &[&str]) {
    let old: Spec<'_> = &[
        blob(b"src/lib.rs", NUMBERED),
        blob(b"bin", b"keep\n\0old\n"),
        blob(b"gone", b"x\n"),
    ];
    let new: Spec<'_> = &[
        blob(b"src/lib.rs", NUMBERED_EDITED),
        blob(b"bin", b"keep\n\0new\n"),
    ];
    let (ours, theirs) = patches(ObjectFormat::Sha1, old, new, Pairing::Exact, options, flags);
    assert_same(&ours, &theirs);
}

/// SHA-256 identities and the null identity use the same abbreviation rules.
#[test]
fn sha256_matches_git() {
    let old: Spec<'_> = &[blob(b"file", b"one\n")];
    let new: Spec<'_> = &[blob(b"file", b"two\n"), blob(b"added", b"three\n")];
    let (ours, theirs) = patches(
        ObjectFormat::Sha256,
        old,
        new,
        Pairing::Exact,
        Options::default(),
        &[],
    );
    assert_same(&ours, &theirs);
}

/// The handle prints what plain `git diff` prints, including its default rename pairing.
#[test]
fn handle_matches_plain_git_diff() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare).unwrap();
    let old = tree(
        &repo,
        &[
            blob(b"empty", b""),
            blob(b"prose", PROSE),
            blob(b"lib.rs", NUMBERED),
        ],
    );
    let new = tree(
        &repo,
        &[
            blob(b"moved", b""),
            blob(b"renamed", PROSE_EDITED),
            blob(b"lib.rs", NUMBERED_EDITED),
        ],
    );
    let git_dir = repo.git_dir().to_path_buf();
    let mut ours = Vec::new();
    girt::Repo::new(repo)
        .write_patch(&mut ours, old, new)
        .unwrap();
    let theirs = git(&git_dir, &["diff", &old.to_string(), &new.to_string()], b"");
    assert_same(&ours, &theirs);
}

/// Shared boundary selection reaches patch output in both modes and object formats.
#[rstest]
fn indent_placement_matches_git(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(false, true)] indent: bool,
) {
    let (ours, theirs) = patches(
        format,
        &[blob(b"code", b"head\n\nhead\n  body\n}\n}\n\n\n}\n")],
        &[blob(
            b"code",
            b"head\n\nhead\n  body\nhead\n  body\n}\n}\n\n\n}\n",
        )],
        Pairing::Exact,
        Options::default().with_indent_heuristic(indent),
        &[],
    );
    assert_same(&ours, &theirs);
}

#[test]
fn binary_payload_option_keeps_text_index_abbreviations() {
    let (ours, theirs) = patches(
        ObjectFormat::Sha1,
        &[blob(b"text", b"old\n")],
        &[blob(b"text", b"new\n")],
        Pairing::Exact,
        Options::default().with_binary_patch(true),
        &["--binary"],
    );
    assert_same(&ours, &theirs);
}
