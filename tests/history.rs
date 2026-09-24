use girt::{HistoryError, HistoryLimits, ObjectId};
use rstest::rstest;
#[path = "support/history_git.rs"]
mod history_git;
use history_git::History;

#[rstest]
#[case::loose(false)]
#[case::packed(true)]
fn criss_cross_matches_git(#[case] packed: bool) {
    let h = History::new(&[vec![], vec![0], vec![0], vec![1, 2], vec![2, 1]], packed);
    let objects = h.objects();
    let mut reachable = objects
        .walk(&[h.ids[3], h.ids[4]], HistoryLimits::default())
        .unwrap();
    reachable.sort();
    assert_eq!(reachable, h.query("rev-list", &[h.ids[3], h.ids[4]]));
    assert_eq!(
        objects
            .merge_bases(h.ids[3], h.ids[4], HistoryLimits::default())
            .unwrap(),
        h.query("merge-base", &[h.ids[3], h.ids[4]])
    );
    assert!(
        objects
            .is_ancestor(h.ids[1], h.ids[4], HistoryLimits::default())
            .unwrap()
    );
    assert!(
        !objects
            .is_ancestor(h.ids[3], h.ids[4], HistoryLimits::default())
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
        h.objects().walk(&roots, HistoryLimits::default()).unwrap(),
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
) {
    let h = History::new(&[vec![], vec![0], vec![]], false);
    let expected: Vec<_> = bases.iter().map(|&i| h.ids[i]).collect();
    assert_eq!(
        h.query("merge-base", &[h.ids[left], h.ids[right]]),
        expected
    );
    assert_eq!(h.is_ancestor(h.ids[left], h.ids[right]), ancestor);
    assert_eq!(
        h.objects()
            .merge_bases(h.ids[left], h.ids[right], HistoryLimits::default())
            .unwrap(),
        expected
    );
    assert_eq!(
        h.objects()
            .is_ancestor(h.ids[left], h.ids[right], HistoryLimits::default())
            .unwrap(),
        ancestor
    );
}

#[rstest]
#[case::commits(1, 10, "commits")]
#[case::parents(10, 0, "parent occurrences")]
fn graph_limits(#[case] max_commits: usize, #[case] max_parents: usize, #[case] expected: &str) {
    let h = History::new(&[vec![], vec![0]], false);
    let limits = HistoryLimits {
        max_commits,
        max_parents,
        ..HistoryLimits::default()
    };
    assert!(
        matches!(h.objects().walk(&[h.ids[1]],limits), Err(HistoryError::Limit(name)) if name == expected)
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
        matches!(h.objects().merge_bases(h.ids[1],h.ids[1],HistoryLimits::default()), Err(HistoryError::Missing(id)) if id == h.ids[0])
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
        .walk(&[id], HistoryLimits::default())
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
        h.objects().walk(&h.ids, limits),
        Err(HistoryError::Read { .. })
    ));
}

#[test]
fn exact_limits_accept_duplicate_edges() {
    let h = History::new(&[vec![], vec![0, 0]], false);
    let limits = HistoryLimits {
        max_commits: 2,
        max_parents: 2,
        ..HistoryLimits::default()
    };
    assert_eq!(
        h.objects().walk(&[h.ids[1]], limits).unwrap(),
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
            .is_ancestor(h.ids[1], h.ids[1], HistoryLimits::default()),
        Err(HistoryError::Read { .. })
    ));
}
