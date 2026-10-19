//! Checkout, patches, rename detection, merges and blame on partial clones made by Git: each
//! reports the blobs it needs as `MissingObjectsError`, and succeeds with Git's result after
//! explicit fetches.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/apply_git.rs"]
mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use girt::workflow::{self, Environment, NoCallbacks};
use girt::{MissingObjectsError, ObjectFormat, ObjectId, ObjectKind, Repository};
use rstest::rstest;
use support::git;

/// A source with a modified file, an unchanged nested file and an edited rename, cloned with
/// `--filter=blob:none --no-checkout` so every blob is missing and nothing is checked out.
struct PartialClone {
    _root: tempfile::TempDir,
    clone: PathBuf,
}

/// Lines long enough that a one-line edit keeps the rename above Git's default similarity.
const MOVED: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";

fn partial_clone(format: ObjectFormat) -> PartialClone {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(
        &source,
        &[
            "init",
            "-q",
            "--template=",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
        b"",
    );
    git(&source, &["config", "uploadpack.allowFilter", "true"], b"");
    std::fs::create_dir(source.join("d")).unwrap();
    std::fs::write(source.join("a"), "a\n").unwrap();
    std::fs::write(source.join("d/b"), "b\n").unwrap();
    // Has no effect on output; only its presence matters.
    std::fs::write(source.join("d/.gitattributes"), "b diff=custom\n").unwrap();
    std::fs::write(source.join("moved"), MOVED).unwrap();
    git(&source, &["add", "."], b"");
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "one"],
        b"",
    );
    std::fs::write(source.join("a"), "c\n").unwrap();
    std::fs::remove_file(source.join("moved")).unwrap();
    std::fs::write(source.join("renamed"), MOVED.replace("five", "FIVE")).unwrap();
    git(&source, &["add", "-A"], b"");
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "two"],
        b"",
    );
    // A branch whose tree holds only a root `.gitattributes`, to name as the attribute source.
    let blob = git(
        &source,
        &["hash-object", "-w", "--stdin"],
        b"a diff=custom\n",
    );
    let blob = std::str::from_utf8(&blob).unwrap().trim();
    let entry = format!("100644 blob {blob}\t.gitattributes\n");
    let tree = git(&source, &["mktree"], entry.as_bytes());
    let tree = std::str::from_utf8(&tree).unwrap().trim().to_owned();
    let commit = git(&source, &["commit-tree", "-m", "attrs", &tree], b"");
    let commit = std::str::from_utf8(&commit).unwrap().trim().to_owned();
    git(&source, &["update-ref", "refs/heads/attrs", &commit], b"");
    let clone = root.path().join("clone");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--template=",
            "--no-local",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", source.display()),
            clone.to_str().unwrap(),
        ],
        b"",
    );
    PartialClone { _root: root, clone }
}

/// Runs Git under the same isolation as `support::git_output`, never fetching a missing object.
fn git_offline(dir: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // Fixture commands must not start background maintenance that writes into `objects/`
        // while a test inspects it.
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "gc.auto")
        .env("GIT_CONFIG_VALUE_1", "0")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-config"))
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn rev(dir: &Path, revision: &str) -> ObjectId {
    std::str::from_utf8(&git_offline(dir, &["rev-parse", revision]))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn ids(revisions: &[&str], dir: &Path) -> BTreeSet<ObjectId> {
    revisions
        .iter()
        .map(|revision| rev(dir, revision))
        .collect()
}

/// Asserts `missing` lists exactly `expected`, each once, as blobs of a partial clone.
fn assert_lists(missing: &MissingObjectsError, expected: &BTreeSet<ObjectId>) {
    assert_eq!(missing.objects.len(), expected.len(), "{missing:?}");
    assert!(
        missing
            .objects
            .iter()
            .all(|(_, kind)| *kind == ObjectKind::Blob)
    );
    assert_eq!(&missing.ids().collect::<BTreeSet<_>>(), expected);
}

fn fetch(dir: &Path, missing: &MissingObjectsError) {
    let repository = Repository::open(dir).unwrap();
    let report = workflow::fetch_needed(
        &repository,
        missing,
        &Environment::default(),
        &mut NoCallbacks,
    )
    .unwrap();
    assert_eq!(report.fetched.len(), missing.objects.len());
}

/// Checkout lists every target blob before touching the worktree; after one fetch it matches
/// Git's checkout of the same commit.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn checkout_lists_missing_blobs_then_matches_git(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let dir = &partial.clone;
    let tree = rev(dir, "HEAD^{tree}");
    let failure = Repository::open(dir)
        .unwrap()
        .checkout_tree(None, Some(tree))
        .unwrap_err();
    let girt::checkout::Error::MissingObjects(missing) = &*failure.cause else {
        panic!("unexpected cause {:?}", failure.cause);
    };
    let needed = [
        "HEAD:a",
        "HEAD:d/b",
        "HEAD:d/.gitattributes",
        "HEAD:renamed",
    ];
    assert_lists(missing, &ids(&needed, dir));
    assert!(failure.report.applied.is_empty());
    assert!(!dir.join(".git/index").exists());
    assert!(!dir.join("a").exists());

    fetch(dir, missing);
    Repository::open(dir)
        .unwrap()
        .checkout_tree(None, Some(tree))
        .unwrap();
    assert_eq!(std::fs::read(dir.join("a")).unwrap(), b"c\n");
    // Git agrees the index and worktree match HEAD, without fetching anything.
    git_offline(dir, &["update-index", "-q", "--refresh"]);
    assert!(git_offline(dir, &["status", "--porcelain"]).is_empty());
}

/// A patch lists both sides of every changed file before writing anything; after one fetch it
/// matches `git diff --no-renames`.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn patch_lists_missing_blobs_then_matches_git(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let dir = &partial.clone;
    let (old, new) = (rev(dir, "HEAD~1^{tree}"), rev(dir, "HEAD^{tree}"));
    let render = || {
        let objects = Repository::open(dir).unwrap().objects().unwrap();
        let entries: Vec<_> = objects
            .compare_trees(Some(old), Some(new))
            .unwrap()
            .into_iter()
            .map(girt::tree_diff::Entry::Change)
            .collect();
        let mut out = Vec::new();
        let options = girt::patch::Options::default().with_abbrev_len(Some(12));
        objects
            .write_patch_with(
                &mut out,
                &entries,
                options,
                None,
                Default::default(),
                &AtomicBool::new(false),
            )
            .map(|()| out)
    };
    let error = render().unwrap_err();
    let girt::patch::Error::MissingObjects(missing) = &error else {
        panic!("unexpected error {error:?}");
    };
    let needed = ["HEAD~1:a", "HEAD:a", "HEAD~1:moved", "HEAD:renamed"];
    assert_lists(missing, &ids(&needed, dir));

    fetch(dir, missing);
    let expected = git_offline(
        dir,
        &["diff", "--no-renames", "--abbrev=12", "HEAD~1", "HEAD"],
    );
    assert_eq!(
        String::from_utf8(render().unwrap()).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

/// Rename detection lists every candidate blob; after one fetch it finds Git's rename.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn rename_detection_lists_missing_candidates(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let dir = &partial.clone;
    let (old, new) = (rev(dir, "HEAD~1^{tree}"), rev(dir, "HEAD^{tree}"));
    let detect = || {
        Repository::open(dir)
            .unwrap()
            .objects()
            .unwrap()
            .detect_rewrites(
                Some(old),
                Some(new),
                Default::default(),
                Default::default(),
                None,
                &AtomicBool::new(false),
            )
    };
    let error = detect().unwrap_err();
    let girt::rewrites::Error::MissingObjects(missing) = &error else {
        panic!("unexpected error {error:?}");
    };
    assert_lists(missing, &ids(&["HEAD~1:moved", "HEAD:renamed"], dir));

    fetch(dir, missing);
    let rewrites = detect().unwrap();
    let status = git_offline(dir, &["diff", "-M", "--name-status", "HEAD~1", "HEAD"]);
    let renamed = String::from_utf8(status)
        .unwrap()
        .lines()
        .find(|line| line.starts_with('R'))
        .unwrap()
        .to_owned();
    assert_eq!(rewrites.len(), 1);
    assert_eq!(
        format!("R{:03}\tmoved\trenamed", rewrites[0].similarity),
        renamed
    );
}

/// Blame reports the first missing version, so a caller fetches as the walk reaches each one.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn blame_reports_each_missing_version(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let dir = &partial.clone;
    let blame = || girt::Repo::new(Repository::open(dir).unwrap()).blame("HEAD", b"a");
    let mut requested = Vec::new();
    let hunks = loop {
        let error = match blame() {
            Ok(hunks) => break hunks,
            Err(error) => error,
        };
        let Some(girt::blame::Error::MissingObjects(missing)) = error.downcast_ref() else {
            panic!("unexpected error {error:?}");
        };
        assert_eq!(missing.objects.len(), 1);
        requested.extend(missing.ids());
        fetch(dir, missing);
    };
    // The tip's version, then its parent's, which differs on the only line.
    assert_eq!(requested, [rev(dir, "HEAD:a"), rev(dir, "HEAD~1:a")]);
    assert_eq!(hunks.len(), 1);
    assert_eq!(hunks[0].commit, rev(dir, "HEAD"));
}

/// The attribute source's `.gitattributes` blobs join the patch's list, so no driver lookup
/// fails after part of the patch is written.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn patch_lists_missing_attribute_source_blobs(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let dir = &partial.clone;
    let (old, new) = (rev(dir, "HEAD~1^{tree}"), rev(dir, "HEAD^{tree}"));
    let environment = Environment::default().with("GIT_ATTR_SOURCE", "origin/attrs");
    let render = || {
        let repository = Repository::open(dir).unwrap();
        let objects = repository.objects().unwrap();
        let mut attributes = repository.diff_attributes(&environment).unwrap();
        let entries: Vec<_> = objects
            .compare_trees(Some(old), Some(new))
            .unwrap()
            .into_iter()
            .map(girt::tree_diff::Entry::Change)
            .collect();
        let mut out = Vec::new();
        let options = girt::patch::Options::default().with_abbrev_len(Some(12));
        objects
            .write_patch_with(
                &mut out,
                &entries,
                options,
                Some(&mut attributes),
                Default::default(),
                &AtomicBool::new(false),
            )
            .map(|()| out)
    };
    // With the changed files already fetched, only the attribute file is missing.
    let content = ["HEAD~1:a", "HEAD:a", "HEAD~1:moved", "HEAD:renamed"];
    let content = MissingObjectsError::new(
        ids(&content, dir)
            .into_iter()
            .map(|id| (id, ObjectKind::Blob))
            .collect(),
    );
    fetch(dir, &content);
    let error = render().unwrap_err();
    let girt::patch::Error::MissingObjects(missing) = &error else {
        panic!("unexpected error {error:?}");
    };
    assert_lists(missing, &ids(&["origin/attrs:.gitattributes"], dir));

    fetch(dir, missing);
    let expected = git_offline(
        dir,
        &[
            "--attr-source=origin/attrs",
            "diff",
            "--no-renames",
            "--abbrev=12",
            "HEAD~1",
            "HEAD",
        ],
    );
    assert_eq!(
        String::from_utf8(render().unwrap()).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

/// A filtered checkout lists the attribute files it reads: the target's own and those of an
/// explicit attribute source outside the target tree.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn filtered_checkout_lists_missing_attribute_blobs(#[case] format: ObjectFormat) {
    use girt::filter::Commands;

    let partial = partial_clone(format);
    let dir = &partial.clone;
    let tree = rev(dir, "HEAD^{tree}");
    let checkout = |environment: &Environment| {
        let repository = Repository::open(dir).unwrap();
        let mut current = repository.filters(environment, Commands::Refuse).unwrap();
        let mut next = repository
            .filters_for_checkout(environment, tree, Commands::Refuse)
            .unwrap();
        repository.checkout_tree_with_filters(
            None,
            Some(tree),
            &mut current,
            &mut next,
            Default::default(),
            &AtomicBool::new(false),
        )
    };
    let target = [
        "HEAD:a",
        "HEAD:d/b",
        "HEAD:d/.gitattributes",
        "HEAD:renamed",
    ];

    // The target's own attribute files are among its blobs.
    let failure = checkout(&Environment::default()).unwrap_err();
    let girt::checkout::Error::MissingObjects(missing) = &*failure.cause else {
        panic!("unexpected cause {:?}", failure.cause);
    };
    assert_lists(missing, &ids(&target, dir));

    // An attribute source adds its own files.
    let source = Environment::default().with("GIT_ATTR_SOURCE", "origin/attrs");
    let failure = checkout(&source).unwrap_err();
    let girt::checkout::Error::MissingObjects(missing) = &*failure.cause else {
        panic!("unexpected cause {:?}", failure.cause);
    };
    let mut needed = ids(&target, dir);
    needed.insert(rev(dir, "origin/attrs:.gitattributes"));
    assert_lists(missing, &needed);
    assert!(failure.report.applied.is_empty());

    fetch(dir, missing);
    checkout(&source).unwrap();
    git_offline(dir, &["update-index", "-q", "--refresh"]);
    assert!(git_offline(dir, &["status", "--porcelain"]).is_empty());
}

/// Branches `ours` and `theirs` from one base: both edit different lines of `both`, `ours`
/// renames `moved` with an edit while `theirs` edits it in place, and both rewrite `conflict`.
/// Cloned with `--filter=blob:none --no-checkout`. Returns the clone and the full source.
fn merge_partial_clone(format: ObjectFormat) -> (PartialClone, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let init = format!("--object-format={format}");
    git(
        &source,
        &["init", "-q", "--template=", "-b", "main", &init],
        b"",
    );
    git(&source, &["config", "uploadpack.allowFilter", "true"], b"");
    let commit = |message: &str| {
        git(&source, &["add", "-A"], b"");
        git(
            &source,
            &["commit", "-q", "--no-gpg-sign", "-m", message],
            b"",
        );
    };
    let both = "1\n2\n3\n4\n5\n6\n7\n8\n";
    std::fs::write(source.join("both"), both).unwrap();
    std::fs::write(source.join("moved"), MOVED).unwrap();
    std::fs::write(source.join("conflict"), "base\n").unwrap();
    std::fs::write(source.join("kept"), "kept\n").unwrap();
    commit("base");
    git(&source, &["checkout", "-q", "-b", "ours"], b"");
    std::fs::write(source.join("both"), both.replace('1', "ONE")).unwrap();
    std::fs::remove_file(source.join("moved")).unwrap();
    std::fs::write(source.join("renamed"), MOVED.replace("five", "FIVE")).unwrap();
    std::fs::write(source.join("conflict"), "ours\n").unwrap();
    commit("ours");
    git(&source, &["checkout", "-q", "-b", "theirs", "main"], b"");
    std::fs::write(source.join("both"), both.replace('8', "EIGHT")).unwrap();
    std::fs::write(source.join("moved"), MOVED.replace("two", "TWO")).unwrap();
    std::fs::write(source.join("conflict"), "theirs\n").unwrap();
    commit("theirs");
    let clone = root.path().join("clone");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--template=",
            "--no-local",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", source.display()),
            clone.to_str().unwrap(),
        ],
        b"",
    );
    (PartialClone { _root: root, clone }, source)
}

/// The leaves under `tree`, reading generated trees from `generated` before the repository.
fn leaves(
    objects: &girt::Objects,
    generated: &[girt::Tree],
    tree: ObjectId,
    prefix: &str,
    out: &mut BTreeSet<(String, ObjectId)>,
) {
    let tree = match generated.iter().find(|candidate| candidate.id() == tree) {
        Some(tree) => tree.clone(),
        None => objects.read_tree(tree).unwrap(),
    };
    for entry in tree.entries() {
        let path = format!("{prefix}{}", String::from_utf8_lossy(&entry.name));
        if entry.mode == girt::EntryMode::Tree {
            leaves(objects, generated, entry.id, &format!("{path}/"), out);
        } else {
            out.insert((path, entry.id));
        }
    }
}

/// A rename-aware merge lists the rename candidates and both-sides blobs before merging content,
/// then the content that rename alignment pairs up; after those fetches it matches
/// `git merge-tree --write-tree`, conflict stages included.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn merge_lists_missing_blobs_then_matches_git(#[case] format: ObjectFormat) {
    let (partial, source) = merge_partial_clone(format);
    let dir = &partial.clone;
    let base = rev(dir, "origin/main^{tree}");
    let ours = rev(dir, "origin/ours^{tree}");
    let theirs = rev(dir, "origin/theirs^{tree}");
    let merge = || {
        Repository::open(dir)
            .unwrap()
            .objects()
            .unwrap()
            .merge_trees(base, ours, theirs)
    };

    // Rename candidates on the ours side, and every side of the paths both branches changed.
    let Err(girt::merge::RenameError::MissingObjects(first)) = merge() else {
        panic!("expected missing objects");
    };
    let needed = [
        "origin/main:moved",
        "origin/ours:renamed",
        "origin/main:both",
        "origin/ours:both",
        "origin/theirs:both",
        "origin/main:conflict",
        "origin/ours:conflict",
        "origin/theirs:conflict",
    ];
    assert_lists(&first, &ids(&needed, dir));
    // Commit merges report the same blobs through their own variant.
    let commits = Repository::open(dir)
        .unwrap()
        .objects()
        .unwrap()
        .merge_commits(rev(dir, "origin/ours"), rev(dir, "origin/theirs"));
    let Err(girt::merge::CommitError::MissingObjects(from_commits)) = commits else {
        panic!("expected missing objects from the commit merge");
    };
    assert_eq!(from_commits, first);
    fetch(dir, &first);

    // Once renames are known, theirs' edit of the rename source is the only content left.
    let Err(girt::merge::RenameError::MissingObjects(second)) = merge() else {
        panic!("expected the aligned rename content");
    };
    assert_lists(&second, &ids(&["origin/theirs:moved"], dir));
    fetch(dir, &second);

    let result = merge().unwrap();
    let output = support::git_output(
        &source,
        &["merge-tree", "--write-tree", "ours", "theirs"],
        b"",
    );
    assert_eq!(output.status.code(), Some(1), "one conflict");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut lines = stdout.lines();
    let git_tree: ObjectId = lines.next().unwrap().parse().unwrap();
    // Conflicted file info: `<mode> <object> <stage>\t<path>`, up to the blank line.
    let git_stages: BTreeSet<(String, ObjectId, u8)> = lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let (info, path) = line.split_once('\t').unwrap();
            let fields: Vec<_> = info.split(' ').collect();
            (
                path.to_owned(),
                fields[1].parse().unwrap(),
                fields[2].parse().unwrap(),
            )
        })
        .collect();
    let stages: BTreeSet<(String, ObjectId, u8)> = result
        .conflicts
        .iter()
        .flat_map(|conflict| conflict.index_entries().unwrap())
        .map(|entry| {
            let path = String::from_utf8(entry.path).unwrap();
            (path, entry.id, entry.stage as u8)
        })
        .collect();
    assert_eq!(stages, git_stages);
    assert_eq!(result.conflicts.len(), 1);

    // Outside the conflict, where Git stores a marker blob, the trees agree path by path.
    let objects = Repository::open(dir).unwrap().objects().unwrap();
    let mut ours_leaves = BTreeSet::new();
    leaves(
        &objects,
        &result.trees,
        result.tree.id(),
        "",
        &mut ours_leaves,
    );
    let source_objects = Repository::open(&source).unwrap().objects().unwrap();
    let mut git_leaves = BTreeSet::new();
    leaves(&source_objects, &[], git_tree, "", &mut git_leaves);
    let clean = |set: BTreeSet<(String, ObjectId)>| -> BTreeSet<_> {
        set.into_iter()
            .filter(|(path, _)| path != "conflict")
            .collect()
    };
    assert_eq!(clean(ours_leaves), clean(git_leaves));
}
