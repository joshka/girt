//! Bisect compatibility with the Git executable: next-commit choice and shared state.
#![cfg(any(target_os = "linux", target_os = "macos"))]

#[path = "support/bisect_git.rs"]
mod support;

use std::collections::HashSet;

use girt::bisect::{Bisect, Mark, ResetTo, Scope, Selection, Start, Step, Terms};
use girt::pathspec::{Context, Pathspec};
use girt::{ObjectId, Repository, Signature};
use support::{Graph, Rng, git, run};

fn person() -> Signature {
    Signature {
        name: b"Bisect Author".to_vec(),
        email: b"bisect@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    }
}

/// What `git bisect` printed after its last command.
#[derive(Debug, PartialEq)]
enum Observed {
    Test(ObjectId, usize, usize),
    Found(ObjectId),
    OnlySkipped(Vec<ObjectId>),
    Untestable,
}

fn observe(stdout: &str) -> Observed {
    if let Some(rest) = stdout.split("Bisecting: ").nth(1) {
        // Git can print -1 when the best split reaches every counted commit; girt reports 0.
        let remaining: i64 = rest
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap_or_else(|_| panic!("{stdout}"));
        let remaining = remaining.max(0) as usize;
        let steps = rest
            .split("(roughly ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let id = rest.split('[').nth(1).unwrap().split(']').next().unwrap();
        return Observed::Test(id.parse().unwrap(), remaining, steps);
    }
    if let Some((id, _)) = stdout.split_once(" is the first ") {
        return Observed::Found(id.trim().parse().unwrap());
    }
    if stdout.contains("only 'skip'ped commits left") {
        let ids = stdout
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        return Observed::OnlySkipped(ids);
    }
    if stdout.contains("No testable commit found") {
        return Observed::Untestable;
    }
    panic!("unrecognised git bisect output: {stdout}");
}

fn selected(selection: Selection) -> Observed {
    match selection {
        Selection::Test {
            commit,
            remaining,
            steps,
        } => Observed::Test(commit, remaining, steps),
        Selection::Found(commit) => Observed::Found(commit),
        Selection::OnlySkipped(ids) => Observed::OnlySkipped(ids),
        Selection::Untestable => Observed::Untestable,
        other => panic!("unexpected {other:?}"),
    }
}

/// One random case: a graph, a range, options and skips, compared with `git bisect`.
fn compare(seed: u64) {
    let mut rng = Rng::new(seed);
    let count = 5 + rng.below(30);
    let graph = Graph::random(seed, count, seed.is_multiple_of(3));
    let path = graph.path();

    let new = graph.id("main");
    let mut old = graph.id(&format!("c{}", rng.below(count / 3 + 1)));
    if run(
        path,
        &[
            "merge-base",
            "--is-ancestor",
            &old.to_string(),
            &new.to_string(),
        ],
    )
    .status
    .code()
        != Some(0)
    {
        old = git(path, &["merge-base", &old.to_string(), &new.to_string()])
            .parse()
            .unwrap();
    }
    let first_parent = rng.chance(30);
    let paths: Vec<String> = if rng.chance(40) {
        (0..1 + rng.below(2))
            .map(|_| format!("f{}", rng.below(5)))
            .collect()
    } else {
        vec![]
    };

    let repo = Repository::open(path).unwrap();
    let pathspec = (!paths.is_empty()).then(|| Pathspec::parse(&paths, &Context::new()).unwrap());
    let scope = Scope::default()
        .with_first_parent(first_parent)
        .with_paths(pathspec);
    let candidates = repo
        .objects()
        .unwrap()
        .bisect_candidates(new, &[old], &scope)
        .unwrap();

    // Skip a random subset of the testable commits other than the new one.
    let mut skipped = HashSet::new();
    if rng.chance(50) {
        for id in candidates.testable().filter(|&id| id != new) {
            if rng.chance(35) {
                skipped.insert(id);
            }
        }
    }

    let mut start = vec!["bisect", "start"];
    if first_parent {
        start.push("--first-parent");
    }
    let (new_text, old_text) = (new.to_string(), old.to_string());
    start.extend([new_text.as_str(), old_text.as_str(), "--"]);
    start.extend(paths.iter().map(String::as_str));
    let mut output = run(path, &start);
    if !skipped.is_empty() && output.status.success() {
        let ids: Vec<String> = skipped.iter().map(ToString::to_string).collect();
        let mut skip = vec!["bisect", "skip"];
        skip.extend(ids.iter().map(String::as_str));
        output = run(path, &skip);
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        selected(candidates.select(&skipped)),
        observe(&stdout),
        "seed {seed}: first_parent {first_parent}, paths {paths:?}, {} skipped\n{stdout}",
        skipped.len()
    );
}

#[test]
fn next_commit_matches_git_on_random_graphs() {
    for seed in 0..80 {
        compare(seed);
    }
}

/// Whether `commit` has the property: it reaches `culprit`.
fn is_new(repo: &Repository, culprit: ObjectId, commit: ObjectId) -> bool {
    repo.objects()
        .unwrap()
        .is_ancestor(culprit, commit)
        .unwrap()
}

fn history(count: usize) -> Graph {
    Graph::random(7, count, false)
}

/// A commit between `c0` and `main`, `fraction` of the way down Git's walk order.
fn culprit(graph: &Graph, fraction: f64) -> ObjectId {
    let range = git(graph.path(), &["rev-list", "main", "^c0"]);
    let ids: Vec<&str> = range.lines().collect();
    ids[(ids.len() as f64 * fraction) as usize].parse().unwrap()
}

#[test]
fn git_continues_a_girt_bisection() {
    let graph = history(30);
    let path = graph.path();
    let repo = Repository::open(path).unwrap();
    let new = graph.id("main");
    let old = graph.id("c0");
    let culprit = culprit(&graph, 0.4);
    assert!(is_new(&repo, culprit, new));

    let terms = Terms::new("broken", "fixed").unwrap();
    let start = Start::default()
        .with_new(new)
        .with_olds(vec![old])
        .with_terms(terms);
    let (bisect, step) = Bisect::start(&repo, person(), start).unwrap();
    let Step::Test { commit, .. } = step else {
        panic!("{step:?}")
    };
    assert_eq!(graph.id("HEAD"), commit);
    drop(bisect);

    // Git reads girt's terms and references and finishes the search.
    let found = loop {
        let head = graph.id("HEAD");
        let term = if is_new(&repo, culprit, head) {
            "broken"
        } else {
            "fixed"
        };
        let output = git(path, &["bisect", term]);
        if let Some((id, _)) = output.split_once(" is the first 'broken' commit") {
            break id.parse::<ObjectId>().unwrap();
        }
    };
    assert!(is_new(&repo, culprit, found));
    let log = git(path, &["bisect", "log"]);
    assert!(log.contains("git bisect start '--term-new=broken' '--term-old=fixed'"));

    git(path, &["bisect", "reset"]);
    assert_eq!(git(path, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert!(!path.join(".git/BISECT_START").exists());
}

#[test]
fn girt_continues_a_git_bisection() {
    let graph = history(40);
    let path = graph.path();
    let repo = Repository::open(path).unwrap();
    let culprit = culprit(&graph, 0.3);

    git(
        path,
        &["bisect", "start", "--", "f0", "f1", "f2", "f3", "f4"],
    );
    git(path, &["bisect", "bad", "main"]);
    git(path, &["bisect", "good", "c0"]);
    let head = graph.id("HEAD");
    let mark = if is_new(&repo, culprit, head) {
        "bad"
    } else {
        "good"
    };
    git(path, &["bisect", mark]);
    git(path, &["bisect", "skip"]);
    let expected_head = graph.id("HEAD");

    let mut bisect = Bisect::open(&repo, person()).unwrap().unwrap();
    assert_eq!(bisect.current().unwrap(), expected_head);
    assert_eq!(bisect.skipped().len(), 1);
    assert_eq!(bisect.paths().len(), 5);

    let mut commit = expected_head;
    let found = loop {
        let mark = if is_new(&repo, culprit, commit) {
            Mark::New
        } else {
            Mark::Old
        };
        match bisect.mark(mark, &[commit]).unwrap() {
            Step::Test { commit: next, .. } => {
                assert_eq!(graph.id("HEAD"), next);
                commit = next;
            }
            Step::Found(found) => break found,
            // A skipped neighbour can hide the answer among the skipped commits.
            Step::OnlySkipped(possible) => {
                assert!(possible.iter().any(|&id| is_new(&repo, culprit, id)));
                git(path, &["bisect", "reset"]);
                return;
            }
            other => panic!("{other:?}"),
        }
    };
    assert!(is_new(&repo, culprit, found));

    // Git reads the log girt appended to and resets girt's state.
    let log = git(path, &["bisect", "log"]);
    assert!(log.contains(&format!("# first 'bad' commit: [{found}]")));
    git(path, &["bisect", "reset"]);
    assert_eq!(git(path, &["for-each-ref", "refs/bisect"]), "");
}

#[test]
fn replaying_a_git_log_reaches_the_same_commit() {
    let graph = history(25);
    let path = graph.path();
    let repo = Repository::open(path).unwrap();

    git(path, &["bisect", "start", "main", "c0"]);
    git(path, &["bisect", "bad"]);
    git(path, &["bisect", "skip"]);
    git(path, &["bisect", "good"]);
    let head = graph.id("HEAD");
    let log = run(path, &["bisect", "log"]).stdout;
    git(path, &["bisect", "reset"]);

    let (bisect, step) = Bisect::replay(&repo, person(), &log).unwrap();
    let Step::Test { commit, .. } = step else {
        panic!("{step:?}")
    };
    assert_eq!(commit, head);
    assert_eq!(graph.id("HEAD"), head);
    bisect.reset(ResetTo::Original).unwrap();
    assert_eq!(git(path, &["symbolic-ref", "HEAD"]), "refs/heads/main");
}

#[test]
fn no_checkout_moves_bisect_head_only() {
    let graph = history(20);
    let path = graph.path();
    let repo = Repository::open(path).unwrap();
    let main = graph.id("main");

    let start = Start::default()
        .with_new(main)
        .with_olds(vec![graph.id("c0")])
        .with_no_checkout(true)
        .with_first_parent(true);
    let (bisect, step) = Bisect::start(&repo, person(), start).unwrap();
    let Step::Test { commit, .. } = step else {
        panic!("{step:?}")
    };
    assert_eq!(graph.id("HEAD"), main);
    assert_eq!(graph.id("BISECT_HEAD"), commit);
    assert!(path.join(".git/BISECT_FIRST_PARENT").exists());
    drop(bisect);

    // Git continues against BISECT_HEAD and agrees on the next commit.
    let output = git(path, &["bisect", "good"]);
    let bisect = Bisect::open(&repo, person()).unwrap().unwrap();
    assert!(bisect.is_no_checkout());
    assert!(output.contains(&format!("[{}]", bisect.current().unwrap())));
    bisect.reset(ResetTo::Original).unwrap();
    assert_eq!(
        run(path, &["rev-parse", "-q", "--verify", "BISECT_HEAD"])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn run_reports_the_first_new_commit() {
    let graph = history(30);
    let path = graph.path();
    let repo = Repository::open(path).unwrap();
    let culprit = culprit(&graph, 0.6);
    let start = Start::default()
        .with_new(graph.id("main"))
        .with_olds(vec![graph.id("c0")]);
    let (mut bisect, _) = Bisect::start(&repo, person(), start).unwrap();

    let step = bisect
        .run(|commit| {
            if is_new(&repo, culprit, commit) {
                girt::bisect::Verdict::New
            } else {
                girt::bisect::Verdict::Old
            }
        })
        .unwrap();
    let Step::Found(found) = step else {
        panic!("{step:?}")
    };
    assert!(is_new(&repo, culprit, found));
    // Git agrees when replaying girt's log.
    let log = std::fs::read(path.join(".git/BISECT_LOG")).unwrap();
    bisect.reset(ResetTo::Original).unwrap();
    std::fs::write(path.join("replay.log"), log).unwrap();
    let output = git(path, &["bisect", "replay", "replay.log"]);
    assert!(
        output.contains(&format!("{found} is the first 'bad' commit")),
        "{output}"
    );
    git(path, &["bisect", "reset"]);
}

/// A line `c0..c<count>` on `main`, and `side` branching from `c0`.
fn line_with_side(count: usize) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(path, &["init", "-q", "-b", "main", "--template=", "."]);
    for i in 0..count {
        std::fs::write(path.join("f"), format!("{i}\n")).unwrap();
        git(path, &["add", "f"]);
        git(path, &["commit", "-q", "-m", &format!("c{i}")]);
    }
    git(
        path,
        &[
            "checkout",
            "-q",
            "-b",
            "side",
            "main~{}".replace("{}", &(count - 1).to_string()).as_str(),
        ],
    );
    std::fs::write(path.join("g"), "side\n").unwrap();
    git(path, &["add", "g"]);
    git(path, &["commit", "-q", "-m", "side"]);
    git(path, &["checkout", "-q", "main"]);
    root
}

#[test]
fn a_new_verdict_rechecks_ancestry() {
    let root = line_with_side(8);
    let path = root.path();
    let repo = Repository::open(path).unwrap();
    let id = |revision: &str| -> ObjectId { git(path, &["rev-parse", revision]).parse().unwrap() };

    let start = Start::default()
        .with_new(id("main"))
        .with_olds(vec![id("main~4")]);
    let (mut bisect, _) = Bisect::start(&repo, person(), start).unwrap();

    // main~4 is not an ancestor of side, so their merge base must be tested, as Git does.
    let step = bisect.mark(Mark::New, &[id("side")]).unwrap();
    assert_eq!(step, Step::MergeBase(id("main~7")));
    bisect.reset(ResetTo::Original).unwrap();
}

#[test]
fn reset_retries_after_a_held_head_lock() {
    let root = line_with_side(12);
    let path = root.path();
    let repo = Repository::open(path).unwrap();
    let id = |revision: &str| -> ObjectId { git(path, &["rev-parse", revision]).parse().unwrap() };
    let start = Start::default()
        .with_new(id("main"))
        .with_olds(vec![id("main~11")]);
    let (bisect, step) = Bisect::start(&repo, person(), start).unwrap();
    let Step::Test { commit: tested, .. } = step else {
        panic!("{step:?}")
    };

    // A held HEAD lock fails the reset before any file changes.
    std::fs::write(path.join(".git/HEAD.lock"), "").unwrap();
    assert!(bisect.reset(ResetTo::Original).is_err());
    assert_eq!(id("HEAD"), tested);
    assert_eq!(
        git(path, &["status", "--porcelain", "--untracked-files=no"]),
        ""
    );

    std::fs::remove_file(path.join(".git/HEAD.lock")).unwrap();
    let bisect = Bisect::open(&repo, person()).unwrap().unwrap();
    bisect.reset(ResetTo::Original).unwrap();
    assert_eq!(git(path, &["symbolic-ref", "HEAD"]), "refs/heads/main");
}
