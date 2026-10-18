//! `Repository::describe` compared with `git describe` on original generated histories.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use girt::describe::{Candidates, DescribeOptions};
use girt::{ObjectId, Repository};
use rstest::rstest;

fn run(directory: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_COMMITTER_NAME", "Describe Tagger")
        .env("GIT_COMMITTER_EMAIL", "tagger@example.com")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git required for describe compatibility tests");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> String {
    let output = run(directory, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A small deterministic generator, so each seed gives the same history.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Writes a history of `count` commits with merges and nondecreasing commit dates (some equal),
/// then tags about a sixth of them, annotated or lightweight, some sharing a commit, and adds a
/// few branches and other references.
fn history(seed: u64, count: usize) -> (tempfile::TempDir, Vec<ObjectId>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(
        path,
        &["init", "--quiet", "--bare", "--template=", "."],
        b"",
    );
    let tree = git(path, &["mktree"], b"");
    let mut rng = Rng(seed);
    let mut ids: Vec<ObjectId> = Vec::new();
    let mut time = 1_700_000_000u64;
    for i in 0..count {
        let mut parents = Vec::new();
        if i > 0 {
            parents.push(if rng.below(2) == 0 {
                i - 1
            } else {
                rng.below(i)
            });
            if rng.below(4) == 0 {
                let other = rng.below(i);
                if !parents.contains(&other) {
                    parents.push(other);
                }
            }
        }
        time += 60 * rng.below(3) as u64;
        let mut payload = format!("tree {tree}\n");
        for parent in parents {
            payload.push_str(&format!("parent {}\n", ids[parent]));
        }
        payload.push_str(&format!(
            "author A <a@example.com> {time} +0000\ncommitter A <a@example.com> {time} +0000\n\nnode {i}\n"
        ));
        let id = git(
            path,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            payload.as_bytes(),
        );
        ids.push(id.parse().unwrap());
    }
    for n in 0..count / 6 + 1 {
        let target = ids[rng.below(count)].to_string();
        let name = format!("{}{n}", ["v", "rel-", "x/"][rng.below(3)]);
        if rng.below(3) == 0 {
            git(path, &["tag", &name, &target], b"");
        } else {
            // Repeated tagger dates exercise the name tie-break.
            let date = format!("@{} +0000", 1_700_000_000 + 1000 * rng.below(3));
            let output = Command::new("git")
                .current_dir(path)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
                .env("GIT_COMMITTER_NAME", "Describe Tagger")
                .env("GIT_COMMITTER_EMAIL", "tagger@example.com")
                .env("GIT_COMMITTER_DATE", date)
                .args(["tag", "-a", "-m", &name, &name, &target])
                .output()
                .unwrap();
            assert!(output.status.success());
        }
    }
    for name in [
        "refs/heads/main",
        "refs/heads/topic",
        "refs/remotes/origin/main",
        "refs/notes/x",
    ] {
        let target = ids[rng.below(count)].to_string();
        git(path, &["update-ref", name, &target], b"");
    }
    (root, ids)
}

/// Option sets as `git describe` arguments and the equivalent girt options.
fn option_sets() -> Vec<(Vec<&'static str>, DescribeOptions)> {
    let base = DescribeOptions::new;
    vec![
        (vec![], base()),
        (vec!["--tags"], base().with_lightweight_tags()),
        (vec!["--all"], base().with_candidates_from(Candidates::All)),
        (
            vec!["--tags", "--match", "v*"],
            base().with_lightweight_tags().with_match("v*"),
        ),
        (
            vec!["--tags", "--match", "v*", "--match", "x/*"],
            base()
                .with_lightweight_tags()
                .with_match("v*")
                .with_match("x/*"),
        ),
        (
            vec!["--tags", "--exclude", "rel-*"],
            base().with_lightweight_tags().with_exclude("rel-*"),
        ),
        (
            vec!["--all", "--match", "*main"],
            base()
                .with_candidates_from(Candidates::All)
                .with_match("*main"),
        ),
        (
            vec!["--tags", "--candidates=1"],
            base().with_lightweight_tags().with_candidates(1),
        ),
        (
            vec!["--tags", "--candidates=2"],
            base().with_lightweight_tags().with_candidates(2),
        ),
        (
            vec!["--tags", "--candidates=0"],
            base().with_lightweight_tags().with_candidates(0),
        ),
        (
            vec!["--tags", "--first-parent"],
            base().with_lightweight_tags().with_first_parent(true),
        ),
        (vec!["--first-parent"], base().with_first_parent(true)),
    ]
}

#[rstest]
fn describe_matches_git(
    #[values(1, 2, 3, 4)] seed: u64,
    #[values(false, true)] commit_graph: bool,
) {
    let (root, ids) = history(seed, 50);
    let path = root.path();
    if commit_graph {
        git(path, &["commit-graph", "write", "--reachable"], b"");
    }
    let repository = Repository::open(path).unwrap();
    let objects = repository.objects().unwrap();
    let estimates = std::cell::Cell::new(0usize);
    for (args, options) in option_sets() {
        for id in ids.iter().step_by(2) {
            let hex = id.to_string();
            let mut command = vec!["describe", "--long", "--abbrev=40"];
            command.extend(&args);
            command.push(&hex);
            let output = run(path, &command, b"");
            let described = output
                .status
                .success()
                .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned());
            let description = repository.describe(&objects, *id, &options).unwrap();
            let context = format!("seed {seed} graph {commit_graph} {args:?} {hex}");
            assert_eq!(description.is_some(), described.is_some(), "{context}");
            let (Some(description), Some(described)) = (description, described) else {
                continue;
            };
            let exact = |reference: &str| {
                let mut count = vec!["rev-list", "--count"];
                if args.contains(&"--first-parent") {
                    count.push("--first-parent");
                }
                let tag = format!("^{reference}");
                count.extend([hex.as_str(), tag.as_str()]);
                git(path, &count, b"").parse::<usize>().unwrap()
            };
            // girt's distance is always the exact count for the reference it chose.
            let reference = String::from_utf8(description.reference.as_bytes().to_vec()).unwrap();
            assert_eq!(description.distance, exact(&reference), "{context}");
            // Git's distance is a date-ordered estimate. Where it is exact, Git and girt agree on
            // every byte; elsewhere Git may print a larger number or prefer another reference.
            let (name, _) = described.rsplit_once("-g").unwrap();
            let (name, git_distance) = name.rsplit_once('-').unwrap();
            let git_distance = git_distance.parse::<usize>().unwrap();
            let git_exact = exact(&described_ref(name, &args));
            assert!(
                description.distance <= git_exact,
                "{context}: girt chose a farther name"
            );
            if git_distance == git_exact && description.distance == git_exact {
                let full = objects.abbreviate(*id, 40).unwrap();
                let actual = String::from_utf8(description.format(Some(&full), true)).unwrap();
                assert_eq!(actual, described, "{context}");
            } else {
                estimates.set(estimates.get() + 1);
            }
        }
    }
    eprintln!(
        "seed {seed}: Git's estimate differed from girt's exact answer {} times",
        estimates.get()
    );
}

/// The full reference for a printed name: `--all` names keep their category under `refs/`.
fn described_ref(name: &str, args: &[&str]) -> String {
    if args.contains(&"--all") {
        format!("refs/{name}")
    } else {
        format!("refs/tags/{name}")
    }
}

/// `--abbrev=0`, default abbreviation and `--long` shapes on one tagged history.
#[test]
fn output_shapes_match_git() {
    let (root, ids) = history(7, 30);
    let path = root.path();
    let repository = Repository::open(path).unwrap();
    let objects = repository.objects().unwrap();
    let options = DescribeOptions::new().with_lightweight_tags();
    for id in &ids {
        let hex = id.to_string();
        let Some(description) = repository.describe(&objects, *id, &options).unwrap() else {
            continue;
        };
        let abbreviated = objects.abbreviate(*id, objects.auto_abbrev_len()).unwrap();
        for (flags, prefix, long) in [
            (&["--tags"][..], Some(&abbreviated), false),
            (&["--tags", "--long"][..], Some(&abbreviated), true),
            (&["--tags", "--abbrev=0"][..], None, false),
        ] {
            let mut command = vec!["describe"];
            command.extend(flags);
            command.push(&hex);
            assert_eq!(
                String::from_utf8(description.format(prefix, long)).unwrap(),
                git(path, &command, b""),
                "{flags:?} {hex}"
            );
        }
    }
}

/// A linear history of `count` commits written by Git, with `main` at the tip.
fn linear(count: usize) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(
        path,
        &["init", "--quiet", "--bare", "--template=", "."],
        b"",
    );
    let tree = git(path, &["mktree"], b"");
    let mut parent: Option<String> = None;
    for i in 0..count {
        let mut payload = format!("tree {tree}\n");
        if let Some(parent) = &parent {
            payload.push_str(&format!("parent {parent}\n"));
        }
        let time = 1_700_000_000 + i * 60;
        payload.push_str(&format!(
            "author A <a@example.com> {time} +0000\ncommitter A <a@example.com> {time} +0000\n\nn{i}\n"
        ));
        parent = Some(git(
            path,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            payload.as_bytes(),
        ));
    }
    git(
        path,
        &["update-ref", "refs/heads/main", parent.as_deref().unwrap()],
        b"",
    );
    root
}

/// A tag just behind the tip is found without reading the rest of the history: the search stops
/// once every queued commit is behind the tag, and the commit-graph bounds the count.
#[test]
fn recent_tag_reads_only_recent_history() {
    let root = linear(400);
    let path = root.path();
    git(path, &["tag", "-a", "-m", "v1", "v1", "main~1"], b"");
    git(path, &["commit-graph", "write", "--reachable"], b"");
    let repository = Repository::open(path).unwrap();
    let objects = repository.objects().unwrap();
    let tip: ObjectId = git(path, &["rev-parse", "main"], b"").parse().unwrap();
    let options =
        DescribeOptions::new().with_limits(girt::HistoryLimits::default().with_max_commits(20));
    let description = repository
        .describe(&objects, tip, &options)
        .unwrap()
        .unwrap();
    assert_eq!(
        (description.name.as_slice(), description.distance),
        (&b"v1"[..], 1)
    );
}

/// Git names a commit through a symbolic reference under the symbolic name.
#[test]
fn symbolic_references_match_git() {
    let root = linear(3);
    let path = root.path();
    git(path, &["update-ref", "refs/heads/z", "main~1"], b"");
    git(
        path,
        &["symbolic-ref", "refs/tags/sym", "refs/heads/z"],
        b"",
    );
    let repository = Repository::open(path).unwrap();
    let objects = repository.objects().unwrap();
    let id: ObjectId = git(path, &["rev-parse", "main~1"], b"").parse().unwrap();
    for (args, options) in [
        (
            &["--tags"][..],
            DescribeOptions::new().with_lightweight_tags(),
        ),
        (
            &["--all"][..],
            DescribeOptions::new().with_candidates_from(Candidates::All),
        ),
    ] {
        let mut command = vec!["describe"];
        command.extend(args);
        let hex = id.to_string();
        command.push(&hex);
        let description = repository
            .describe(&objects, id, &options)
            .unwrap()
            .unwrap();
        assert_eq!(
            String::from_utf8(description.format(None, false)).unwrap(),
            git(path, &command, b"")
        );
    }
}

/// A tag whose reference was renamed is printed under the tag object's name, always long.
#[test]
fn renamed_tags_match_git() {
    let root = linear(3);
    let path = root.path();
    git(path, &["tag", "-a", "-m", "o", "original", "main~1"], b"");
    let tag = git(path, &["rev-parse", "original"], b"");
    git(path, &["update-ref", "refs/tags/renamed", &tag], b"");
    git(path, &["tag", "-d", "original"], b"");
    let repository = Repository::open(path).unwrap();
    let objects = repository.objects().unwrap();
    for revision in ["main", "main~1"] {
        let id: ObjectId = git(path, &["rev-parse", revision], b"").parse().unwrap();
        let hex = id.to_string();
        let abbreviated = objects.abbreviate(id, objects.auto_abbrev_len()).unwrap();
        for (args, options, prefix) in [
            (vec![], DescribeOptions::new(), Some(&abbreviated)),
            (vec!["--abbrev=0"], DescribeOptions::new(), None),
            (
                vec!["--all"],
                DescribeOptions::new().with_candidates_from(Candidates::All),
                Some(&abbreviated),
            ),
            (
                vec!["--match", "ren*"],
                DescribeOptions::new().with_match("ren*"),
                Some(&abbreviated),
            ),
        ] {
            let mut command = vec!["describe"];
            command.extend(&args);
            command.push(&hex);
            let description = repository
                .describe(&objects, id, &options)
                .unwrap()
                .unwrap();
            assert_eq!(
                String::from_utf8(description.format(prefix, false)).unwrap(),
                git(path, &command, b""),
                "{revision} {args:?}"
            );
        }
    }
}
