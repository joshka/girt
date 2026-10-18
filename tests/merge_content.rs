//! Original fixtures compared only with Git's executable under isolated configuration.
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::content_diff::{BinaryMode, DiffError, DiffLimits};
use girt::merge::{
    ConflictStyle, ContentHunk, ContentOptions, Error, Favor, MarkerOptions, content, content_with,
};
use rstest::rstest;

fn oracle(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    style: ConflictStyle,
    favor: Favor,
) -> (Vec<u8>, i32) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("base"), base).unwrap();
    std::fs::write(root.path().join("ours"), ours).unwrap();
    std::fs::write(root.path().join("theirs"), theirs).unwrap();
    let mut git = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            git.env_remove(key);
        }
    }
    git.current_dir(root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
        .env("LC_ALL", "C")
        .args([
            "merge-file",
            "-p",
            "-L",
            "ours",
            "-L",
            "base",
            "-L",
            "theirs",
        ]);
    match style {
        ConflictStyle::Merge => {}
        ConflictStyle::Diff3 => {
            git.arg("--diff3");
        }
        ConflictStyle::Zdiff3 => {
            git.arg("--zdiff3");
        }
        _ => panic!("unsupported fixture style"),
    }
    match favor {
        Favor::Conflict => {}
        Favor::Ours => {
            git.arg("--ours");
        }
        Favor::Theirs => {
            git.arg("--theirs");
        }
        Favor::Union => {
            git.arg("--union");
        }
        _ => panic!("unsupported fixture favor"),
    }
    let result = git.args(["ours", "base", "theirs"]).output().unwrap();
    assert!(result.stderr.is_empty(), "{:?}", result.stderr);
    (result.stdout, result.status.code().unwrap())
}

#[rstest]
#[case::independent(b"a\nb\nc\n", b"A\nb\nc\n", b"a\nb\nC\n")]
#[case::adjacent(b"a\nb\nc\n", b"A\nb\nc\n", b"a\nB\nc\n")]
#[case::overlap(b"a\nb\nc\n", b"a\nleft\nc\n", b"a\nright\nc\n")]
#[case::identical(b"old\n", b"same\n", b"same\n")]
#[case::unchanged(b"a\n", b"a\n", b"new\n")]
#[case::empty(b"", b"ours\n", b"theirs\n")]
#[case::deletion(b"one\ntwo\n", b"", b"one\nTWO\n")]
#[case::unterminated(b"old", b"ours", b"theirs")]
#[case::bytes(b"old\xff\n", b"ours\xff\n", b"theirs\xfe\n")]
#[case::boundaries(b"old\n", b"common\nx\ntail\n", b"common\ny\ntail\n")]
#[case::insertion(b"a\nb\n", b"x\na\nb\n", b"A\nb\n")]
fn agrees_with_git(
    #[case] base: &[u8],
    #[case] ours: &[u8],
    #[case] theirs: &[u8],
    #[values(ConflictStyle::Merge, ConflictStyle::Diff3, ConflictStyle::Zdiff3)]
    style: ConflictStyle,
    #[values(Favor::Conflict, Favor::Ours, Favor::Theirs, Favor::Union)] favor: Favor,
) {
    // Union is a computation policy with Git's default boundary refinement. Rendering a
    // clean result does not change that computation when another marker style is selected.
    let oracle_style = if favor == Favor::Union {
        ConflictStyle::Merge
    } else {
        style
    };
    let (expected, status) = oracle(base, ours, theirs, oracle_style, favor);
    let result = content_with(
        base,
        ours,
        theirs,
        ContentOptions::default().with_favor(favor),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut markers = MarkerOptions::default();
    markers.style = style;
    assert_eq!(result.render(&markers), expected);
    assert_eq!(result.is_clean(), status == 0);
}

#[test]
fn preserves_conflict_sides_independently_of_markers() {
    let result = content(b"old", b"left", b"right").unwrap();
    assert_eq!(
        result.hunks(),
        &[ContentHunk::Conflict {
            base: b"old".to_vec(),
            ours: b"left".to_vec(),
            theirs: b"right".to_vec()
        }]
    );
}

#[test]
fn binary_conflict_keeps_all_inputs_and_renders_ours() {
    let result = content(b"old\0", b"ours\0", b"theirs\0").unwrap();
    assert!(result.is_binary());
    assert!(!result.is_clean());
    assert_eq!(result.render(&MarkerOptions::default()), b"ours\0");
}

#[test]
fn forced_text_merges_nul_lines() {
    let result = content_with(
        b"old\0",
        b"ours\0",
        b"theirs\0",
        ContentOptions::default().with_binary(BinaryMode::Text),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!result.is_binary());
    assert!(!result.is_clean());
}

#[test]
fn cancellation_precedes_equal_input_resolution() {
    assert!(matches!(
        content_with(
            b"same",
            b"same",
            b"same",
            ContentOptions::default(),
            &AtomicBool::new(true)
        ),
        Err(Error::Diff(DiffError::Cancelled))
    ));
}

#[test]
fn bounds_apply_to_equal_inputs() {
    let options =
        ContentOptions::default().with_limits(DiffLimits::default().with_max_input_bytes(0));
    assert!(matches!(
        content_with(b"a", b"a", b"a", options, &AtomicBool::new(false)),
        Err(Error::Diff(DiffError::Limit(_)))
    ));
}

#[test]
fn marker_width_and_byte_labels_are_caller_selected() {
    let result = content(b"old\n", b"ours\n", b"theirs\n").unwrap();
    let mut options = MarkerOptions::default();
    options.width = std::num::NonZeroUsize::new(3).unwrap();
    options.ours = b"left\xff".to_vec();
    options.theirs.clear();
    assert_eq!(
        result.render(&options),
        b"<<< left\xff\nours\n===\ntheirs\n>>>\n"
    );
}

#[rstest]
#[case::ours(Favor::Ours, b"ours\0", true)]
#[case::theirs(Favor::Theirs, b"theirs\0", true)]
#[case::union(Favor::Union, b"ours\0", false)]
fn binary_favor_is_explicit(#[case] favor: Favor, #[case] expected: &[u8], #[case] clean: bool) {
    let result = content_with(
        b"base\0",
        b"ours\0",
        b"theirs\0",
        ContentOptions::default().with_favor(favor),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.render(&MarkerOptions::default()), expected);
    assert_eq!(result.is_clean(), clean);
}
