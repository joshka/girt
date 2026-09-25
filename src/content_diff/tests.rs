use std::ops::Range;
use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::*;

fn text(old: &[u8], new: &[u8]) -> Vec<ContentEdit> {
    // Shape tests only use owned coordinates; avoid retaining fixture references in assertions.
    let ContentDiff::Text(result) = diff(
        old,
        new,
        BinaryMode::Text,
        DiffLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap() else {
        panic!("expected changed text")
    };
    result.edits
}

#[rstest]
#[case::add(b"", b"a\nb\n", vec![(0..0, 0..2)])]
#[case::delete(b"a\nb", b"", vec![(0..2, 0..0)])]
#[case::replace(b"a\nb\nc\n", b"a\nx\nc\n", vec![(1..2, 1..2)])]
#[case::insert_middle(b"a\nc\n", b"a\nb\nc\n", vec![(1..1, 1..2)])]
#[case::delete_middle(b"a\nb\nc\n", b"a\nc\n", vec![(1..2, 1..1)])]
#[case::two_edits(b"a\nb\nc\nd\n", b"x\nb\nc\ny\n", vec![(0..1, 0..1), (3..4, 3..4)])]
#[case::repeated_prefix(b"a\na\n", b"a\n", vec![(1..2, 1..1)])]
#[case::ambiguous_swap(b"a\nb\n", b"b\na\n", vec![(0..1, 0..0), (2..2, 1..2)])]
#[case::blank_lines(b"\n\nx\n", b"\nx\n", vec![(1..2, 1..1)])]
#[case::crlf(b"a\r\n", b"a\n", vec![(0..1, 0..1)])]
#[case::cr_is_not_delimiter(b"a\rb", b"a\rc", vec![(0..1, 0..1)])]
#[case::no_final_lf(b"a\n", b"a", vec![(0..1, 0..1)])]
#[case::add_final_lf(b"a", b"a\n", vec![(0..1, 0..1)])]
#[case::non_utf8(b"\xff\nkeep\n", b"\xfe\nkeep\n", vec![(0..1, 0..1)])]
#[case::nul_text(b"a\0\n", b"b\0\n", vec![(0..1, 0..1)])]
fn edit_shapes(
    #[case] old: &[u8],
    #[case] new: &[u8],
    #[case] expected: Vec<(Range<usize>, Range<usize>)>,
) {
    let result = text(old, new);
    let actual: Vec<_> = result
        .iter()
        .map(|e| (e.old_lines.clone(), e.new_lines.clone()))
        .collect();
    assert_eq!(actual, expected);
    assert_reconstructs(old, new);
}

#[rstest]
#[case::empty(b"", BinaryMode::Auto)]
#[case::text(b"x\n", BinaryMode::Text)]
#[case::nul_auto(b"\0", BinaryMode::Auto)]
#[case::binary(b"x", BinaryMode::Binary)]
fn equality_precedes_classification(#[case] input: &[u8], #[case] mode: BinaryMode) {
    assert_eq!(
        diff(
            input,
            input,
            mode,
            DiffLimits::default(),
            &AtomicBool::new(false)
        )
        .unwrap(),
        ContentDiff::Unchanged
    );
}

#[rstest]
#[case::nul_old(b"\0", b"x", BinaryMode::Auto)]
#[case::nul_new(b"x", b"\0", BinaryMode::Auto)]
#[case::forced(b"a\n", b"b\n", BinaryMode::Binary)]
#[case::binary_add(b"", b"\0", BinaryMode::Auto)]
#[case::binary_delete(b"\0", b"", BinaryMode::Auto)]
fn binary_keeps_exact_payloads(#[case] old: &[u8], #[case] new: &[u8], #[case] mode: BinaryMode) {
    assert_eq!(
        diff(
            old,
            new,
            mode,
            DiffLimits::default(),
            &AtomicBool::new(false)
        )
        .unwrap(),
        ContentDiff::Binary { old, new }
    );
}

#[test]
fn auto_scans_past_git_sampling_window() {
    let old = vec![b'x'; 16_384];
    let mut new = old.clone();
    new.push(0);
    assert!(matches!(
        diff(
            &old,
            &new,
            BinaryMode::Auto,
            DiffLimits::default(),
            &AtomicBool::new(false)
        )
        .unwrap(),
        ContentDiff::Binary { .. }
    ));
}

#[test]
fn auto_accepts_invalid_utf8_without_nul() {
    assert!(matches!(
        diff(
            b"\xff",
            b"\xfe",
            BinaryMode::Auto,
            DiffLimits::default(),
            &AtomicBool::new(false)
        )
        .unwrap(),
        ContentDiff::Text(_)
    ));
}

#[rstest]
#[case::bytes(DiffLimits { max_input_bytes: 3, ..DiffLimits::default() }, "input bytes")]
#[case::lines(DiffLimits { max_lines: 1, ..DiffLimits::default() }, "lines")]
#[case::trace(DiffLimits { max_trace: 5, ..DiffLimits::default() }, "trace")]
#[case::work(DiffLimits { max_work: 0, ..DiffLimits::default() }, "work")]
fn rejects_exhausted_bounds(#[case] limits: DiffLimits, #[case] name: &str) {
    assert!(
        matches!(diff(b"a\n", b"b\n", BinaryMode::Text, limits, &AtomicBool::new(false)), Err(DiffError::Limit(found)) if found == name)
    );
}

#[test]
fn exact_input_line_trace_bounds_succeed() {
    let limits = DiffLimits {
        max_input_bytes: 4,
        max_lines: 2,
        max_trace: 6,
        ..DiffLimits::default()
    };
    assert!(
        diff(
            b"a\n",
            b"b\n",
            BinaryMode::Text,
            limits,
            &AtomicBool::new(false)
        )
        .is_ok()
    );
}

#[rstest]
#[case::equal(b"a", b"a", BinaryMode::Text)]
#[case::binary(b"\0", b"x", BinaryMode::Auto)]
fn bytes_bound_also_applies_without_search(
    #[case] old: &[u8],
    #[case] new: &[u8],
    #[case] mode: BinaryMode,
) {
    let limits = DiffLimits {
        max_input_bytes: 1,
        ..DiffLimits::default()
    };
    assert!(matches!(
        diff(old, new, mode, limits, &AtomicBool::new(false)),
        Err(DiffError::Limit("input bytes"))
    ));
}

#[rstest]
#[case::empty(b"", b"")]
#[case::binary(b"\0", b"x")]
#[case::text(b"a\n", b"b\n")]
fn cancellation_precedes_fast_paths(#[case] old: &[u8], #[case] new: &[u8]) {
    assert!(matches!(
        diff(
            old,
            new,
            BinaryMode::Auto,
            DiffLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(DiffError::Cancelled)
    ));
}

#[test]
fn checkpoints_observe_cancellation_after_prior_work() {
    let flag = AtomicBool::new(false);
    let mut budget = Budget {
        remaining: 20,
        cancel: &flag,
    };
    budget.work(5).unwrap();
    flag.store(true, Ordering::Relaxed);
    assert!(matches!(budget.work(1), Err(DiffError::Cancelled)));
}

#[test]
fn exact_work_bound_for_empty_side() {
    let limits = DiffLimits {
        max_work: 3,
        max_trace: 0,
        ..DiffLimits::default()
    };
    assert!(
        diff(
            b"",
            b"x\n",
            BinaryMode::Text,
            limits,
            &AtomicBool::new(false)
        )
        .is_ok()
    );
    assert!(matches!(
        diff(
            b"",
            b"x\n",
            BinaryMode::Text,
            DiffLimits {
                max_work: 2,
                ..limits
            },
            &AtomicBool::new(false)
        ),
        Err(DiffError::Limit("work"))
    ));
}

#[test]
fn long_equal_lines_charge_bytes_not_just_comparisons() {
    let old = vec![b'a'; 8192];
    let limits = DiffLimits {
        max_work: 8191,
        ..DiffLimits::default()
    };
    assert!(matches!(
        diff(
            &old,
            &old,
            BinaryMode::Text,
            limits,
            &AtomicBool::new(false)
        ),
        Err(DiffError::Limit("work"))
    ));
}

#[test]
fn large_unrelated_input_returns_a_limit_not_a_partial_script() {
    let old = b"old\n".repeat(100_000);
    let new = b"new\n".repeat(100_000);
    assert!(matches!(
        diff(
            &old,
            &new,
            BinaryMode::Text,
            DiffLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(DiffError::Limit("trace"))
    ));
}

#[test]
fn large_addition_does_not_allocate_quadratic_trace() {
    let new = b"line\n".repeat(100_000);
    let limits = DiffLimits {
        max_trace: 0,
        ..DiffLimits::default()
    };
    let ContentDiff::Text(result) =
        diff(b"", &new, BinaryMode::Text, limits, &AtomicBool::new(false)).unwrap()
    else {
        panic!("text")
    };
    assert_eq!(result.edits()[0].new_lines, 0..100_000);
}

#[test]
fn large_small_edit_fits_tiny_trace() {
    let old = b"same\n".repeat(100_000);
    let mut new = old.clone();
    new.extend_from_slice(b"tail\n");
    let limits = DiffLimits {
        max_trace: 3,
        ..DiffLimits::default()
    };
    let ContentDiff::Text(result) = diff(
        &old,
        &new,
        BinaryMode::Text,
        limits,
        &AtomicBool::new(false),
    )
    .unwrap() else {
        panic!("text")
    };
    assert_eq!(result.edits()[0].old_lines, 100_000..100_000);
    assert_eq!(result.edits()[0].new_lines, 100_000..100_001);
}

/// Independently exhaust every pair of binary-alphabet sequences up to five lines. Dynamic
/// programming is an intentionally different small-input oracle for minimum edit length.
#[test]
fn exhaustive_short_sequences_reconstruct_with_minimum_cost() {
    check_short_sequences();
}

fn check_short_sequences() {
    let mut sequences = vec![vec![]];
    for length in 1..=5 {
        for bits in 0..(1 << length) {
            sequences.push(
                (0..length)
                    .flat_map(|i| [b'a' + ((bits >> i) & 1) as u8, b'\n'])
                    .collect(),
            );
        }
    }
    for old in &sequences {
        for new in &sequences {
            let cost = assert_reconstructs(old, new);
            assert_eq!(cost, minimum_cost(old, new), "{old:?} -> {new:?}");
        }
    }
}

fn minimum_cost(old: &[u8], new: &[u8]) -> usize {
    let a: Vec<_> = old.split_inclusive(|b| *b == b'\n').collect();
    let b: Vec<_> = new.split_inclusive(|b| *b == b'\n').collect();
    let mut row: Vec<_> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if x == y {
                diagonal
            } else {
                1 + above.min(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

fn assert_reconstructs(old: &[u8], new: &[u8]) -> usize {
    let result = diff(
        old,
        new,
        BinaryMode::Text,
        DiffLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let ContentDiff::Text(text) = result else {
        assert_eq!(old, new);
        return 0;
    };
    assert!(
        text.edits()
            .windows(2)
            .all(|pair| pair[0].old_lines.end < pair[1].old_lines.start
                && pair[0].new_lines.end < pair[1].new_lines.start)
    );
    let mut rebuilt = Vec::new();
    let (mut old_end, mut new_end, mut cost) = (0, 0, 0);
    for edit in text.edits() {
        assert_eq!(
            &old[old_end..edit.old_bytes.start],
            &new[new_end..edit.new_bytes.start]
        );
        assert_eq!(line_offset(old, edit.old_lines.start), edit.old_bytes.start);
        assert_eq!(line_offset(old, edit.old_lines.end), edit.old_bytes.end);
        assert_eq!(line_offset(new, edit.new_lines.start), edit.new_bytes.start);
        assert_eq!(line_offset(new, edit.new_lines.end), edit.new_bytes.end);
        rebuilt.extend_from_slice(&old[old_end..edit.old_bytes.start]);
        rebuilt.extend_from_slice(&new[edit.new_bytes.clone()]);
        old_end = edit.old_bytes.end;
        new_end = edit.new_bytes.end;
        cost += edit.old_lines.len() + edit.new_lines.len();
    }
    assert_eq!(&old[old_end..], &new[new_end..]);
    rebuilt.extend_from_slice(&old[old_end..]);
    assert_eq!(rebuilt, new);
    cost
}

fn line_offset(bytes: &[u8], line: usize) -> usize {
    bytes
        .split_inclusive(|b| *b == b'\n')
        .take(line)
        .map(<[u8]>::len)
        .sum()
}

#[test]
fn varied_byte_sequences_preserve_boundaries_and_minimum_cost() {
    check_varied_bytes();
}

fn check_varied_bytes() {
    let alphabet = [b'a', b'b', b'\n', b'\r', 0, 0xff];
    let mut state = 73u32;
    let mut generate = |length| -> Vec<u8> {
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                alphabet[(state >> 16) as usize % alphabet.len()]
            })
            .collect()
    };
    for length in 0..80 {
        let old = generate(length);
        let new = generate(79 - length);
        assert_eq!(assert_reconstructs(&old, &new), minimum_cost(&old, &new));
    }
}

#[test]
fn line_bound_counts_unterminated_fragment() {
    let limits = DiffLimits {
        max_lines: 1,
        ..DiffLimits::default()
    };
    assert!(matches!(
        diff(
            b"",
            b"a\nb",
            BinaryMode::Text,
            limits,
            &AtomicBool::new(false)
        ),
        Err(DiffError::Limit("lines"))
    ));
}

#[test]
fn empty_equal_inputs_need_no_budget() {
    let limits = DiffLimits {
        max_input_bytes: 0,
        max_lines: 0,
        max_trace: 0,
        max_work: 0,
    };
    assert_eq!(
        diff(b"", b"", BinaryMode::Auto, limits, &AtomicBool::new(false)).unwrap(),
        ContentDiff::Unchanged
    );
}

#[test]
fn checked_budget_rejects_underflow_and_accepts_usize_max() {
    let mut remaining = usize::MAX;
    charge(&mut remaining, usize::MAX, "test").unwrap();
    assert_eq!(remaining, 0);
    assert!(matches!(
        charge(&mut remaining, 1, "test"),
        Err(DiffError::Limit("test"))
    ));
}

#[rstest]
#[case::one_short(15, false)]
#[case::exact(16, true)]
fn traceback_counts_toward_work_budget(#[case] max_work: usize, #[case] succeeds: bool) {
    let limits = DiffLimits {
        max_work,
        ..DiffLimits::default()
    };
    let result = diff(
        b"a\n",
        b"b\n",
        BinaryMode::Text,
        limits,
        &AtomicBool::new(false),
    );
    assert_eq!(result.is_ok(), succeeds);
}
