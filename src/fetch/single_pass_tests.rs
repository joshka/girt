//! Received objects are verified in one pass: identity, framing and typed connectivity are proven
//! while importing, and publication reuses that proof instead of walking the received graph again.
//!
//! Histories and packs are generated with the Git executable. No upstream implementation or test
//! source is used.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::*;
use crate::fetch::{
    Advertisement, FetchError, FetchLimits, FetchOptions, KnownHistory, ReceivedFetch,
    ValidationObserver,
};
use crate::{InitKind, ObjectFormat, ObjectKind};

fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_id(path: &Path, args: &[&str], input: &[u8]) -> ObjectId {
    std::str::from_utf8(&git(path, args, input))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// A Git-written history with nested trees, two commits and an annotated tag.
struct History {
    _root: tempfile::TempDir,
    source: Repository,
    format: ObjectFormat,
    commits: Vec<ObjectId>,
    tag: ObjectId,
}

fn history(format: ObjectFormat) -> History {
    let root = tempfile::tempdir().unwrap();
    let source = Repository::init(format, root.path().join("source"), InitKind::Bare).unwrap();
    let dir = source.git_dir();
    let mut commits: Vec<ObjectId> = Vec::new();
    for round in 0..2 {
        let a = git_id(dir, &["hash-object", "-w", "--stdin"], b"shared\n");
        let b = git_id(
            dir,
            &["hash-object", "-w", "--stdin"],
            format!("round {round}\n").as_bytes(),
        );
        let leaf = git_id(
            dir,
            &["mktree"],
            format!("100644 blob {b}\tleaf\n").as_bytes(),
        );
        let mid = git_id(
            dir,
            &["mktree"],
            format!("040000 tree {leaf}\tinner\n100644 blob {a}\tshared\n").as_bytes(),
        );
        let top = git_id(
            dir,
            &["mktree"],
            format!("040000 tree {mid}\tdir\n100755 blob {b}\ttool\n").as_bytes(),
        );
        let mut args = vec!["commit-tree".to_owned(), top.to_string()];
        if let Some(parent) = commits.last() {
            args.extend(["-p".to_owned(), parent.to_string()]);
        }
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        commits.push(git_id(dir, &args, format!("round {round}\n").as_bytes()));
    }
    let head = commits.last().unwrap();
    git(
        dir,
        &["update-ref", "refs/heads/main", &head.to_string()],
        b"",
    );
    git(
        dir,
        &["tag", "-a", "-m", "release", "v1", &head.to_string()],
        b"",
    );
    let tag = git_id(dir, &["rev-parse", "refs/tags/v1"], b"");
    History {
        _root: root,
        source,
        format,
        commits,
        tag,
    }
}

fn destination(format: ObjectFormat, specs: &[&str]) -> (tempfile::TempDir, FetchRequest) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let specs =
        Refspecs::parse(Direction::Fetch, specs.iter().map(|spec| spec.as_bytes())).unwrap();
    let request = FetchRequest::prepare(
        repo,
        specs,
        std::collections::BTreeSet::new(),
        Reflog::Preserve,
    )
    .unwrap();
    (root, request)
}

fn clone(history: &History) -> (tempfile::TempDir, FetchReady) {
    let (root, request) = destination(
        history.format,
        &[
            "+refs/heads/*:refs/remotes/origin/*",
            "+refs/tags/*:refs/tags/*",
        ],
    );
    let cancel = AtomicBool::new(false);
    let ready = request
        .receive_local(
            history.source.git_dir(),
            &KnownHistory::default(),
            FetchLimits::default(),
            crate::transport::TransportControl::new(&cancel),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    (root, ready)
}

/// Tree edges are the bulk of a history's structure (62.9M on git/git). Import proves them while
/// each tree is in hand; only commit and tag edges stay for fast-forward ancestry.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn received_tree_edges_are_not_retained(#[case] format: ObjectFormat) {
    let history = history(format);
    let (_root, ready) = clone(&history);
    let verified = &ready.received.verified;
    assert_eq!(verified.len(), 12, "every received object keeps its proof");
    assert_eq!(verified.retained_edges(ObjectKind::Tree), 0);
    // Two root trees and one parent, plus the tag's target.
    assert_eq!(verified.retained_edges(ObjectKind::Commit), 3);
    assert_eq!(verified.retained_edges(ObjectKind::Tag), 1);
    assert!(verified.contains(history.tag));
}

/// Publication proves the installed pack is the validated artifact and that lookup selects it,
/// then trusts the connectivity proven at import instead of walking the received graph again.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn clone_publication_does_not_rewalk_received_objects(#[case] format: ObjectFormat) {
    let history = history(format);
    let (_root, ready) = clone(&history);
    let repository = ready.request.repository.clone();
    crate::fetch::known::WALKED.with(|walked| walked.set(0));
    crate::pack::counters::reset();
    ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(crate::fetch::known::WALKED.with(std::cell::Cell::get), 0);
    let inflations =
        crate::pack::counters::INFLATIONS.with(|counts| counts.borrow().values().sum::<usize>());
    assert_eq!(inflations, 0);
    let refs = repository.references().unwrap();
    let main = refs
        .resolve(&RefName::new(b"refs/remotes/origin/main").unwrap(), 8)
        .unwrap()
        .id;
    assert_eq!(main, history.commits.last().copied());
    git(
        repository.git_dir(),
        &["fsck", "--strict", "--no-dangling"],
        b"",
    );
}

/// An incremental fetch still re-reads known-local dependencies down to existing references.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn incremental_publication_still_checks_local_dependencies(#[case] format: ObjectFormat) {
    let history = history(format);
    let (_root, request) = destination(format, &["+refs/heads/*:refs/remotes/origin/*"]);
    let repository = request.repository.clone();
    drop(request);
    // The destination already has the first commit's history under another name.
    let first = history.commits[0];
    let pack = git(
        history.source.git_dir(),
        &["pack-objects", "--stdout", "--revs"],
        format!("{first}\n").as_bytes(),
    );
    git(repository.git_dir(), &["index-pack", "--stdin"], &pack);
    repository
        .references()
        .unwrap()
        .update_without_reflog(
            &RefName::new(b"refs/heads/base").unwrap(),
            Target::Direct(first),
            Expected::Absent,
        )
        .unwrap();
    let cancel = AtomicBool::new(false);
    let objects = repository.objects(Default::default()).unwrap();
    let known = KnownHistory::new(&objects, &[first], FetchLimits::default(), &cancel).unwrap();
    let request = FetchRequest::prepare(
        repository.clone(),
        Refspecs::parse(
            Direction::Fetch,
            [b"+refs/heads/*:refs/remotes/origin/*".as_slice()],
        )
        .unwrap(),
        std::collections::BTreeSet::new(),
        Reflog::Preserve,
    )
    .unwrap();
    let ready = request
        .receive_local(
            history.source.git_dir(),
            &known,
            FetchLimits::default(),
            crate::transport::TransportControl::new(&cancel),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(!ready.received.dependencies().is_empty());
    crate::fetch::known::WALKED.with(|walked| walked.set(0));
    ready.finish(FetchUpdateLimits::default(), &cancel).unwrap();
    assert!(crate::fetch::known::WALKED.with(std::cell::Cell::get) > 0);
    git(
        repository.git_dir(),
        &["fsck", "--strict", "--no-dangling"],
        b"",
    );
}

fn validate(
    format: ObjectFormat,
    pack: Vec<u8>,
    wants: &[ObjectId],
) -> Result<ReceivedFetch, FetchError> {
    ReceivedFetch::validate_known(
        Advertisement {
            refs: vec![],
            capabilities: vec![format!("object-format={format}").into_bytes()],
        },
        wants.to_vec(),
        pack,
        &KnownHistory::default(),
        vec![],
        FetchOptions {
            limits: FetchLimits::default(),
            depth: None,
        },
        &mut ValidationObserver::new(&AtomicBool::new(false), &mut |_| {}),
    )
}

/// A pack can carry objects that no want reaches. Git's connectivity check walks from the new tips,
/// so a broken extra is tolerated, while the same tree under a wanted commit is rejected.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unreachable_broken_extra_matches_git_connectivity(#[case] format: ObjectFormat) {
    let history = history(format);
    let dir = history.source.git_dir();
    let missing = git_id(dir, &["hash-object", "--stdin"], b"never stored\n");
    let broken = git_id(
        dir,
        &["mktree", "--missing"],
        format!("100644 blob {missing}\tgone\n").as_bytes(),
    );
    let broken_commit = git_id(dir, &["commit-tree", &broken.to_string()], b"broken\n");
    let good = *history.commits.last().unwrap();
    let mut list = git(dir, &["rev-list", "--objects", &good.to_string()], b"");
    list.extend_from_slice(format!("{broken}\n{broken_commit}\n").as_bytes());
    let pack = git(dir, &["pack-objects", "--stdout"], &list);

    // Git: index the pack and walk from each tip.
    let observed = tempfile::tempdir().unwrap();
    let object_format = format!("--object-format={format}");
    git(
        observed.path(),
        &["init", "-q", "--bare", &object_format, "."],
        b"",
    );
    git(observed.path(), &["index-pack", "--stdin"], &pack);
    git(
        observed.path(),
        &["rev-list", "--objects", &good.to_string()],
        b"",
    );
    let status = Command::new("git")
        .current_dir(observed.path())
        .args(["rev-list", "--objects", &broken_commit.to_string()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "Git rejects the broken tip");

    let received = validate(format, pack.clone(), &[good]).unwrap();
    assert_eq!(received.object_count(), 13);
    assert!(matches!(
        validate(format, pack, &[broken_commit]),
        Err(FetchError::Missing(id)) if id == missing
    ));
}

/// A tree entry whose mode names a blob but whose target is a received tree is a typed edge
/// failure, as it is for Git's `pack-objects --revs` walk over the same graph.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn mistyped_tree_entry_is_rejected(#[case] format: ObjectFormat) {
    let history = history(format);
    let dir = history.source.git_dir();
    let target = git_id(
        dir,
        &["rev-parse", &format!("{}^{{tree}}", history.commits[0])],
        b"",
    );
    let mut tree = b"100644 wrong\0".to_vec();
    tree.extend_from_slice(target.as_bytes());
    let mistyped = git_id(
        dir,
        &["hash-object", "-t", "tree", "-w", "--stdin", "--literally"],
        &tree,
    );
    let commit = git_id(dir, &["commit-tree", &mistyped.to_string()], b"mistyped\n");
    // Git's own walk refuses this graph, so list the objects explicitly.
    let mut list = git(
        dir,
        &["rev-list", "--objects", &history.commits[0].to_string()],
        b"",
    );
    list.extend_from_slice(format!("{commit}\n{mistyped}\n").as_bytes());
    let pack = git(dir, &["pack-objects", "--stdout"], &list);
    assert!(matches!(
        validate(format, pack, &[commit]),
        Err(FetchError::Kind(id)) if id == target
    ));
}

/// A commit whose `tree` header names a blob, or a tag that mislabels its target, is rejected
/// even when a received tree correctly names the same object first. The collective tree proof
/// visits that blob, and every later edge to it must still be checked for its kind.
#[rstest]
#[case::sha1_commit(ObjectFormat::Sha1, "commit")]
#[case::sha256_commit(ObjectFormat::Sha256, "commit")]
#[case::sha1_tag(ObjectFormat::Sha1, "tag")]
#[case::sha256_tag(ObjectFormat::Sha256, "tag")]
fn edge_to_proven_target_still_checks_its_kind(#[case] format: ObjectFormat, #[case] wrong: &str) {
    let history = history(format);
    let dir = history.source.git_dir();
    let blob = git_id(dir, &["hash-object", "-w", "--stdin"], b"named twice\n");
    let tree = git_id(
        dir,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let body = if wrong == "commit" {
        format!(
            "tree {blob}\nauthor A <a@example.com> 0 +0000\ncommitter A <a@example.com> 0 +0000\n\nmistyped\n"
        )
    } else {
        format!("object {blob}\ntype tree\ntag v\ntagger A <a@example.com> 0 +0000\n\nmistyped\n")
    };
    let tip = git_id(
        dir,
        &["hash-object", "-t", wrong, "-w", "--stdin", "--literally"],
        body.as_bytes(),
    );
    let pack = git(
        dir,
        &["pack-objects", "--stdout"],
        format!("{tip}\n{tree}\n{blob}\n").as_bytes(),
    );
    assert!(matches!(
        validate(format, pack, &[tip]),
        Err(FetchError::Kind(id)) if id == blob
    ));
}
