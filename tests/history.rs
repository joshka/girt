//! Checks commit history queries against the Git executable.

use girt::{HistoryError, HistoryLimits, ObjectFormat, ObjectId};
use rstest::rstest;
#[path = "support/history_git.rs"]
mod history_git;
use history_git::History;

#[rstest]
#[case::loose(false)]
#[case::packed(true)]
fn criss_cross_matches_git(
    #[case] packed: bool,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let h = History::with_format(
        &[vec![], vec![0], vec![0], vec![1, 2], vec![2, 1]],
        packed,
        format,
    );
    let objects = h.objects();
    let mut reachable = objects
        .walk_with(&[h.ids[3], h.ids[4]], HistoryLimits::default())
        .unwrap();
    reachable.sort();
    assert_eq!(reachable, h.query("rev-list", &[h.ids[3], h.ids[4]]));
    assert_eq!(
        objects
            .merge_bases_with(h.ids[3], h.ids[4], HistoryLimits::default())
            .unwrap(),
        h.query("merge-base", &[h.ids[3], h.ids[4]])
    );
    assert!(
        objects
            .is_ancestor_with(h.ids[1], h.ids[4], HistoryLimits::default())
            .unwrap()
    );
    assert!(
        !objects
            .is_ancestor_with(h.ids[3], h.ids[4], HistoryLimits::default())
            .unwrap()
    );
    assert_eq!(h.query("merge-base", &[h.ids[1], h.ids[4]]), vec![h.ids[1]]);
}

#[rstest]
#[case::empty(vec![], vec![], vec![])]
#[case::root(vec![vec![]], vec![0], vec![0])]
#[case::linear(vec![vec![], vec![0], vec![1]], vec![2], vec![2,1,0])]
#[case::duplicates(vec![vec![], vec![0,0]], vec![1,1], vec![1,0])]
#[case::root_order_not_topological(vec![vec![], vec![0]], vec![0,1], vec![0,1])]
#[case::merge(vec![vec![], vec![0], vec![0], vec![1,2]], vec![3], vec![3,1,2,0])]
fn breadth_first_order(
    #[case] parents: Vec<Vec<usize>>,
    #[case] roots: Vec<usize>,
    #[case] expected: Vec<usize>,
) {
    let h = History::new(&parents, false);
    let roots: Vec<_> = roots.iter().map(|&i| h.ids[i]).collect();
    let expected: Vec<_> = expected.iter().map(|&i| h.ids[i]).collect();
    assert_eq!(
        h.objects()
            .walk_with(&roots, HistoryLimits::default())
            .unwrap(),
        expected
    );
}

#[rstest]
#[case::identical(1,1,vec![1],true)]
#[case::ancestor(0,1,vec![0],true)]
#[case::reverse(1,0,vec![0],false)]
#[case::disconnected(1,2,vec![],false)]
fn endpoint_queries(
    #[case] left: usize,
    #[case] right: usize,
    #[case] bases: Vec<usize>,
    #[case] ancestor: bool,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let h = History::with_format(&[vec![], vec![0], vec![]], false, format);
    let expected: Vec<_> = bases.iter().map(|&i| h.ids[i]).collect();
    assert_eq!(
        h.query("merge-base", &[h.ids[left], h.ids[right]]),
        expected
    );
    assert_eq!(h.is_ancestor(h.ids[left], h.ids[right]), ancestor);
    assert_eq!(
        h.objects()
            .merge_bases_with(h.ids[left], h.ids[right], HistoryLimits::default())
            .unwrap(),
        expected
    );
    assert_eq!(
        h.objects()
            .is_ancestor_with(h.ids[left], h.ids[right], HistoryLimits::default())
            .unwrap(),
        ancestor
    );
}

#[rstest]
#[case::commits(1, 10, "commits")]
#[case::parents(10, 0, "parent occurrences")]
fn graph_limits(#[case] max_commits: usize, #[case] max_parents: usize, #[case] expected: &str) {
    let h = History::new(&[vec![], vec![0]], false);
    let limits = HistoryLimits::default()
        .with_max_commits(max_commits)
        .with_max_parents(max_parents);
    assert!(
        matches!(h.objects().walk_with(&[h.ids[1]],limits), Err(HistoryError::Limit(name)) if name == expected)
    );
}

#[test]
fn missing_parent_is_an_error_even_for_identical_endpoints() {
    let h = History::new(&[vec![], vec![0]], false);
    let hex = h.ids[0].to_string();
    std::fs::remove_file(
        h.root
            .path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]),
    )
    .unwrap();
    assert!(
        matches!(h.objects().merge_bases_with(h.ids[1],h.ids[1],HistoryLimits::default()), Err(HistoryError::Missing(id)) if id == h.ids[0])
    );
}

#[rstest]
#[case::blob("blob", b"data", false)]
#[case::malformed("commit", b"bad commit", true)]
fn invalid_objects(#[case] kind: &str, #[case] data: &[u8], #[case] parse: bool) {
    let h = History::new(&[], false);
    let out = history_git::git(
        h.root.path(),
        &["hash-object", "--literally", "-w", "-t", kind, "--stdin"],
        data,
    );
    let id: ObjectId = std::str::from_utf8(&out).unwrap().trim().parse().unwrap();
    let error = h
        .objects()
        .walk_with(&[id], HistoryLimits::default())
        .unwrap_err();
    assert_eq!(matches!(error, HistoryError::Parse { .. }), parse);
    assert_eq!(matches!(error, HistoryError::NotCommit(_)), !parse);
}

#[test]
fn per_object_limits_preserve_storage_error() {
    let h = History::new(&[vec![]], false);
    let mut limits = HistoryLimits::default();
    limits.read.max_object_bytes = 1;
    assert!(matches!(
        h.objects().walk_with(&h.ids, limits),
        Err(HistoryError::Read { .. })
    ));
}

#[test]
fn exact_limits_accept_duplicate_edges() {
    let h = History::new(&[vec![], vec![0, 0]], false);
    let limits = HistoryLimits::default()
        .with_max_commits(2)
        .with_max_parents(2);
    assert_eq!(
        h.objects().walk_with(&[h.ids[1]], limits).unwrap(),
        vec![h.ids[1], h.ids[0]]
    );
}

#[test]
fn corrupt_parent_preserves_storage_error() {
    let h = History::new(&[vec![], vec![0]], false);
    let hex = h.ids[0].to_string();
    let path = h
        .root
        .path()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::remove_file(&path).unwrap();
    std::fs::write(path, b"invalid zlib").unwrap();
    assert!(matches!(
        h.objects()
            .is_ancestor_with(h.ids[1], h.ids[1], HistoryLimits::default()),
        Err(HistoryError::Read { .. })
    ));
}

/// Multiple roots expose the difference between breadth-first and topological traversal (#416).
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn root_order_can_differ_from_git_topological_order(#[case] format: ObjectFormat) {
    let h = History::with_format(&[vec![], vec![0]], false, format);
    let output = history_git::git(
        h.root.path(),
        &[
            "rev-list",
            "--topo-order",
            &h.ids[0].to_string(),
            &h.ids[1].to_string(),
        ],
        b"",
    );
    let expected: Vec<ObjectId> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(expected, vec![h.ids[1], h.ids[0]]);
    let actual = h
        .objects()
        .walk_with(&h.ids, HistoryLimits::default())
        .unwrap();
    assert_eq!(actual, h.ids);
    assert_ne!(actual, expected);
}

/// A deterministic history with merges, criss-crosses and two disconnected roots.
fn tangled() -> Vec<Vec<usize>> {
    let mut parents = vec![vec![], vec![]];
    let mut seed = 0x9e37_79b9_u32;
    for i in 2..40 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let first = (seed as usize) % i;
        let edges = if seed.is_multiple_of(3) {
            let second = (seed as usize / 7) % i;
            if second == first {
                vec![first]
            } else {
                vec![first, second]
            }
        } else {
            vec![first]
        };
        parents.push(edges);
    }
    parents
}

#[rstest]
fn ahead_behind_matches_rev_list(#[values(false, true)] commit_graph: bool) {
    let h = History::new(&tangled(), true);
    if commit_graph {
        history_git::git(
            h.root.path(),
            &["commit-graph", "write", "--reachable"],
            b"",
        );
    }
    let objects = h.objects();
    let objects = if commit_graph {
        objects
    } else {
        objects.without_commit_graph()
    };
    for left in (0..h.ids.len()).step_by(3) {
        for right in (1..h.ids.len()).step_by(4) {
            let range = format!("{}...{}", h.ids[left], h.ids[right]);
            let output = history_git::git(
                h.root.path(),
                &["rev-list", "--left-right", "--count", &range],
                b"",
            );
            let output = String::from_utf8(output).unwrap();
            let expected: Vec<usize> = output
                .split_whitespace()
                .map(|n| n.parse().unwrap())
                .collect();
            let counts = objects.ahead_behind(h.ids[left], h.ids[right]).unwrap();
            assert_eq!(
                [counts.ahead, counts.behind],
                expected[..],
                "{left}...{right}"
            );
        }
    }
}
