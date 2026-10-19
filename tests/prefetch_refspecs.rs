//! Pure prefetch refspec policy compared with isolated Git CLI execution.
use std::sync::atomic::AtomicBool;

use girt::ObjectFormat;
use girt::maintenance::{
    PrefetchLimits, PrefetchRefspecs, PrefetchRefspecsError, PrefetchSkipReason,
};
use girt::refs::RefName;
use girt::remote::RefSource;
use rstest::rstest;

#[path = "support/maintenance_git.rs"]
mod oracle;

#[test]
fn rewrite_preserves_bytes_force_and_negative_selectors() {
    let prepared = PrefetchRefspecs::parse(
        [
            b"+refs/heads/\xff:refs/cache/\xff".as_slice(),
            b"^refs/heads/skip",
            b"refs/heads/main",
            b"+refs/tags/*:refs/custom/*",
        ],
        PrefetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let specs: Vec<_> = prepared
        .refspecs()
        .specs()
        .iter()
        .map(|spec| spec.as_bytes())
        .collect();
    assert_eq!(
        specs,
        [
            b"+refs/heads/\xff:refs/prefetch/cache/\xff".as_slice(),
            b"^refs/heads/skip"
        ]
    );
    assert_eq!(prepared.skipped()[0].index, 2);
    assert_eq!(
        prepared.skipped()[0].reason,
        PrefetchSkipReason::NoDestination
    );
    assert_eq!(prepared.skipped()[1].index, 3);
    assert_eq!(prepared.skipped()[1].reason, PrefetchSkipReason::TagSource);
}

#[rstest]
#[case::count(0)]
#[case::input(1)]
#[case::output(2)]
fn every_specification_consumes_the_limits(#[case] resource: usize) {
    let mut limits = PrefetchLimits::default();
    match resource {
        0 => limits.max_specs = 0,
        1 => limits.max_input_bytes = 0,
        2 => limits.max_output_bytes = 0,
        _ => unreachable!(),
    }
    let result = PrefetchRefspecs::parse(
        ["refs/heads/main:refs/cache/main"],
        limits,
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(PrefetchRefspecsError::SpecLimit
            | PrefetchRefspecsError::InputByteLimit
            | PrefetchRefspecsError::OutputByteLimit)
    ));
}

#[test]
fn cancellation_precedes_input_iteration() {
    let input =
        std::iter::from_fn(|| -> Option<&'static str> { panic!("input must not be visited") });
    assert!(matches!(
        PrefetchRefspecs::parse(input, PrefetchLimits::default(), &AtomicBool::new(true)),
        Err(PrefetchRefspecsError::Cancelled)
    ));
}

#[test]
fn malformed_skipped_source_is_still_refused() {
    let error = PrefetchRefspecs::parse(
        ["refs/heads/main", "+refs/tags/*:not-full"],
        PrefetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    let PrefetchRefspecsError::Input(source) = error else {
        panic!("wrong error: {error:?}");
    };
    assert_eq!(source.index, 1);
}

#[rstest]
#[case::normal(vec!["+refs/heads/*:refs/remotes/origin/*"])]
#[case::custom(vec!["+refs/heads/*:refs/custom/*", "+refs/tags/*:refs/tags/*"])]
#[case::source_only(vec!["refs/heads/main"])]
#[case::tags_to_custom(vec!["+refs/tags/*:refs/customtags/*"])]
#[case::heads_to_tags(vec!["+refs/heads/*:refs/tags/*"])]
#[case::negative(vec!["+refs/heads/*:refs/remotes/origin/*", "^refs/heads/main"])]
#[case::broad(vec!["+refs/*:refs/cache/*"])]
#[case::broad_negative(vec!["+refs/*:refs/cache/*", "^refs/tags/*"])]
#[case::head(vec!["HEAD:refs/cache/head"])]
fn mapped_destinations_match_git_prefetch(
    #[case] specs: Vec<&str>,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    for name in ["source.git", "local.git"] {
        oracle::git(
            root.path(),
            &[
                "init",
                "--bare",
                "--template=",
                "--initial-branch=main",
                &format!("--object-format={format}"),
                name,
            ],
            b"",
        );
    }
    let source = root.path().join("source.git");
    let local = root.path().join("local.git");
    let tree = oracle::git(&source, &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let commit = oracle::git(&source, &["commit-tree", tree], b"one\n");
    let commit = std::str::from_utf8(&commit).unwrap().trim();
    oracle::git(&source, &["update-ref", "refs/heads/main", commit], b"");
    oracle::git(
        &source,
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "-a",
            "-m",
            "tag",
            "v1",
            commit,
        ],
        b"",
    );
    oracle::git(
        &local,
        &["config", "remote.origin.url", source.to_str().unwrap()],
        b"",
    );
    for spec in &specs {
        oracle::git(
            &local,
            &["config", "--add", "remote.origin.fetch", spec],
            b"",
        );
    }
    oracle::git(
        &local,
        &[
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.autoDetach=false",
            "-c",
            "gc.autoDetach=false",
            "maintenance",
            "run",
            "--task=prefetch",
        ],
        b"",
    );
    let names = oracle::git(&local, &["for-each-ref", "--format=%(refname)"], b"");
    let mut expected: Vec<_> = names
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    expected.sort();
    let advertised = oracle::git(
        &source,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
        b"",
    );
    let mut sources = vec![RefSource {
        name: RefName::HEAD,
        id: commit.parse().unwrap(),
    }];
    for line in advertised
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let separator = line.iter().position(|byte| *byte == b' ').unwrap();
        sources.push(RefSource {
            name: RefName::new(&line[separator + 1..]).unwrap(),
            id: std::str::from_utf8(&line[..separator])
                .unwrap()
                .parse()
                .unwrap(),
        });
    }
    let prepared =
        PrefetchRefspecs::parse(&specs, PrefetchLimits::default(), &AtomicBool::new(false))
            .unwrap();
    let mut actual: Vec<_> = prepared
        .refspecs()
        .map(&sources)
        .unwrap()
        .into_iter()
        .filter_map(|mapping| mapping.destination.map(|name| name.as_bytes().to_vec()))
        .collect();
    actual.sort();
    assert_eq!(actual, expected);
    assert!(!local.join("FETCH_HEAD").exists());
    if actual.is_empty() {
        let counts = oracle::git(&local, &["count-objects", "-v"], b"");
        assert!(String::from_utf8(counts).unwrap().starts_with("count: 0\n"));
    }
    oracle::git(&local, &["fsck", "--strict"], b"");
}

#[test]
fn cancellation_after_a_batch_precedes_next_input() {
    use std::cell::Cell;
    use std::sync::atomic::Ordering;
    let cancel = AtomicBool::new(false);
    let visited = Cell::new(0);
    let input = std::iter::from_fn(|| {
        visited.set(visited.get() + 1);
        cancel.store(true, Ordering::Relaxed);
        Some("refs/heads/main:refs/cache/main")
    });
    assert!(matches!(
        PrefetchRefspecs::parse(input, PrefetchLimits::default(), &cancel),
        Err(PrefetchRefspecsError::Cancelled)
    ));
    assert_eq!(visited.get(), 64);
}

#[rstest]
#[case::source_only("refs/heads/main")]
#[case::tags("+refs/tags/*:refs/cache/*")]
fn ignored_specifications_still_consume_input(#[case] spec: &str) {
    let mut limits = PrefetchLimits::default();
    limits.max_input_bytes = 0;
    assert!(matches!(
        PrefetchRefspecs::parse([spec], limits, &AtomicBool::new(false)),
        Err(PrefetchRefspecsError::InputByteLimit)
    ));
    limits.max_input_bytes = usize::MAX;
    limits.max_specs = 0;
    assert!(matches!(
        PrefetchRefspecs::parse([spec], limits, &AtomicBool::new(false)),
        Err(PrefetchRefspecsError::SpecLimit)
    ));
}

#[test]
fn negative_selectors_consume_output_bytes() {
    let mut limits = PrefetchLimits::default();
    limits.max_output_bytes = 0;
    assert!(matches!(
        PrefetchRefspecs::parse(["^refs/heads/skip"], limits, &AtomicBool::new(false)),
        Err(PrefetchRefspecsError::OutputByteLimit)
    ));
}
