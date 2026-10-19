//! Partial clones made by Git: promised objects, walks over them, fsck and commit-graphs.
#![cfg(unix)]
#[path = "support/apply_git.rs"]
mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use girt::fsck::ObjectFinding;
use girt::{
    ListLimits, MissingObjects, ObjectFormat, ObjectId, ObjectKind, ObjectWalkError,
    ObjectWalkLimits, ReadLimits, Repository,
};
use rstest::rstest;
use support::git;

struct PartialClone {
    _root: tempfile::TempDir,
    clone: PathBuf,
    tip: ObjectId,
}

fn oid(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

/// Two commits with a nested file, cloned with `--filter=blob:none` so every blob is missing.
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
    git(&source, &["add", "."], b"");
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "one"],
        b"",
    );
    std::fs::write(source.join("a"), "c\n").unwrap();
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-am", "two"],
        b"",
    );
    let tip = oid(&git(&source, &["rev-parse", "HEAD"], b""));
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
    PartialClone {
        _root: root,
        clone,
        tip,
    }
}

/// Runs Git under the same isolation as `support::git_output`, never fetching a missing object.
fn git_offline(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
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
        .unwrap()
}

fn git_missing(dir: &Path) -> BTreeSet<ObjectId> {
    let output = git_offline(dir, &["rev-list", "--objects", "--all", "--missing=print"]);
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .map(|id| id.parse().unwrap())
        .collect()
}

fn promisor_marker(clone: &Path) -> PathBuf {
    std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "promisor"))
        .unwrap()
}

fn walk(
    repository: &Repository,
    tip: ObjectId,
    missing: MissingObjects,
) -> Result<girt::ObjectWalk, ObjectWalkError> {
    repository.objects().unwrap().walk_objects(
        &[tip],
        missing,
        ObjectWalkLimits::default(),
        &AtomicBool::new(false),
    )
}

fn missing_findings(report: &girt::fsck::Report) -> BTreeSet<ObjectId> {
    report
        .findings
        .iter()
        .filter_map(|finding| match finding {
            ObjectFinding::Missing { id } => Some(*id),
            _ => None,
        })
        .collect()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn promised_blobs_are_classified_and_allowed(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let expected = git_missing(&partial.clone);
    assert_eq!(expected.len(), 3);
    let repository = Repository::open(&partial.clone).unwrap();
    assert!(repository.is_partial_clone());

    let objects = repository.objects().unwrap();
    let promised = objects
        .promised_objects(
            ListLimits::default(),
            ReadLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    for id in &expected {
        assert!(promised.contains(*id));
        assert!(!objects.contains(*id).unwrap());
    }

    assert!(matches!(
        walk(&repository, partial.tip, MissingObjects::Error),
        Err(ObjectWalkError::Missing {
            kind: ObjectKind::Blob,
            ..
        })
    ));
    for policy in [MissingObjects::AllowPromisor, MissingObjects::AllowAny] {
        let listed = walk(&repository, partial.tip, policy).unwrap();
        let missing: BTreeSet<_> = listed.missing.iter().map(|(id, _)| *id).collect();
        assert_eq!(missing, expected);
        assert!(
            listed
                .missing
                .iter()
                .all(|(_, kind)| *kind == ObjectKind::Blob)
        );
        // Two commits, two root trees and one shared subtree.
        assert_eq!(listed.objects.len(), 5);
    }

    assert!(git_offline(&partial.clone, &["fsck"]).status.success());
    let report = repository.fsck().unwrap();
    assert!(!report.has_errors(), "{:?}", report.findings);

    // Older clones record the promisor in an extension; fsck still completes.
    git(
        &partial.clone,
        &["config", "core.repositoryFormatVersion", "1"],
        b"",
    );
    git(
        &partial.clone,
        &["config", "extensions.partialClone", "origin"],
        b"",
    );
    let repository = Repository::open(&partial.clone).unwrap();
    assert!(repository.is_partial_clone());
    let report = repository.fsck().unwrap();
    assert!(!report.has_errors(), "{:?}", report.findings);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn missing_blobs_without_promisor_marker_are_errors(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let expected = git_missing(&partial.clone);
    std::fs::remove_file(promisor_marker(&partial.clone)).unwrap();

    let git_fsck = git_offline(&partial.clone, &["fsck"]);
    assert!(!git_fsck.status.success());
    let repository = Repository::open(&partial.clone).unwrap();
    let report = repository.fsck().unwrap();
    assert!(report.has_errors());
    assert_eq!(missing_findings(&report), expected);
    assert!(matches!(
        walk(&repository, partial.tip, MissingObjects::AllowPromisor),
        Err(ObjectWalkError::Missing { .. })
    ));
    let listed = walk(&repository, partial.tip, MissingObjects::AllowAny).unwrap();
    assert_eq!(listed.missing.len(), expected.len());
}

#[test]
fn unpromised_missing_object_beside_promised_ones_is_an_error() {
    let partial = partial_clone(ObjectFormat::Sha1);
    let absent = ObjectId::for_blob(ObjectFormat::Sha1, b"never stored\n");
    let tree = git(
        &partial.clone,
        &["mktree", "--missing"],
        format!("100644 blob {absent}\tz\n").as_bytes(),
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim().to_owned();
    let commit = oid(&git(
        &partial.clone,
        &["commit-tree", "--no-gpg-sign", &tree, "-m", "x"],
        b"",
    ));
    git(
        &partial.clone,
        &["update-ref", "refs/heads/x", &commit.to_string()],
        b"",
    );

    assert!(!git_offline(&partial.clone, &["fsck"]).status.success());
    let repository = Repository::open(&partial.clone).unwrap();
    let report = repository.fsck().unwrap();
    assert_eq!(missing_findings(&report), BTreeSet::from([absent]));
    assert!(matches!(
        walk(&repository, commit, MissingObjects::AllowPromisor),
        Err(ObjectWalkError::Missing { id, .. }) if id == absent
    ));
    let listed = walk(&repository, commit, MissingObjects::AllowAny).unwrap();
    assert_eq!(listed.missing, [(absent, ObjectKind::Blob)]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn commit_graph_writes_without_missing_blobs(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let repository = Repository::open(&partial.clone).unwrap();
    let report = repository
        .write_commit_graph(&[partial.tip], girt::commit_graph::GraphWriteMode::Full)
        .unwrap();
    assert_eq!(report.commits_written, 2);
    let verify = git_offline(&partial.clone, &["commit-graph", "verify"]);
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

#[test]
fn promisor_marker_without_partial_clone_config_promises_nothing() {
    let partial = partial_clone(ObjectFormat::Sha1);
    let expected = git_missing(&partial.clone);
    // Either setting alone makes `origin` a promisor remote for Git.
    for key in ["remote.origin.promisor", "remote.origin.partialclonefilter"] {
        git(&partial.clone, &["config", "--unset", key], b"");
    }
    assert!(promisor_marker(&partial.clone).exists());

    assert!(!git_offline(&partial.clone, &["fsck"]).status.success());
    let rev_list = git_offline(
        &partial.clone,
        &["rev-list", "--objects", "--all", "--missing=allow-promisor"],
    );
    assert!(!rev_list.status.success());

    let repository = Repository::open(&partial.clone).unwrap();
    assert!(!repository.is_partial_clone());
    let report = repository.fsck().unwrap();
    assert_eq!(missing_findings(&report), expected);
    let promised = repository
        .objects()
        .unwrap()
        .promised_objects(
            ListLimits::default(),
            ReadLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(promised.is_empty());
    assert!(matches!(
        walk(&repository, partial.tip, MissingObjects::AllowPromisor),
        Err(ObjectWalkError::Missing { .. })
    ));
}

#[test]
fn promised_missing_parent_stops_walks_but_not_fsck() {
    let partial = partial_clone(ObjectFormat::Sha1);
    let tree = git(&partial.clone, &["rev-parse", "HEAD^{tree}"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim().to_owned();
    let parent = ObjectId::for_blob(ObjectFormat::Sha1, b"stands in for an absent parent");
    let body = format!(
        "tree {tree}\nparent {parent}\nauthor a <a@x> 946684800 +0000\n\
         committer a <a@x> 946684800 +0000\n\nchild\n"
    );
    let child = oid(&git(
        &partial.clone,
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        body.as_bytes(),
    ));
    let pack = git(
        &partial.clone,
        &["pack-objects", "-q", ".git/objects/pack/pack"],
        format!("{child}\n").as_bytes(),
    );
    let pack = std::str::from_utf8(&pack).unwrap().trim().to_owned();
    std::fs::write(
        partial
            .clone
            .join(format!(".git/objects/pack/pack-{pack}.promisor")),
        "",
    )
    .unwrap();
    git(&partial.clone, &["prune-packed"], b"");
    git(
        &partial.clone,
        &["update-ref", "refs/heads/child", &child.to_string()],
        b"",
    );

    // Git forgives the promised parent in fsck but not in `--missing=allow-promisor`.
    assert!(git_offline(&partial.clone, &["fsck"]).status.success());
    let rev_list = git_offline(
        &partial.clone,
        &["rev-list", "--objects", "--all", "--missing=allow-promisor"],
    );
    assert!(!rev_list.status.success());
    let repository = Repository::open(&partial.clone).unwrap();
    let report = repository.fsck().unwrap();
    assert!(!report.has_errors(), "{:?}", report.findings);
    assert!(matches!(
        walk(&repository, child, MissingObjects::AllowPromisor),
        Err(ObjectWalkError::Missing { id, kind: ObjectKind::Commit }) if id == parent
    ));
    let listed = walk(&repository, child, MissingObjects::AllowAny).unwrap();
    assert!(listed.missing.contains(&(parent, ObjectKind::Commit)));
}

#[test]
fn tiny_limits_stop_walks_and_promised_sets() {
    let partial = partial_clone(ObjectFormat::Sha1);
    let repository = Repository::open(&partial.clone).unwrap();
    let objects = repository.objects().unwrap();
    let not_cancelled = AtomicBool::new(false);
    for max_objects in [0, 1, 2] {
        assert!(matches!(
            objects.walk_objects(
                &[partial.tip],
                MissingObjects::AllowAny,
                ObjectWalkLimits::default().with_max_objects(max_objects),
                &not_cancelled,
            ),
            Err(ObjectWalkError::Limit)
        ));
    }
    assert!(matches!(
        objects.walk_objects(
            &[partial.tip],
            MissingObjects::AllowAny,
            ObjectWalkLimits::default(),
            &AtomicBool::new(true),
        ),
        Err(ObjectWalkError::Read(girt::ObjectReadError::Cancelled))
    ));
    for max_entries in [0, 1] {
        assert!(matches!(
            objects.promised_objects(
                ListLimits::default().with_max_entries(max_entries),
                ReadLimits::default(),
                &not_cancelled,
            ),
            Err(girt::ObjectReadError::Limit(_))
        ));
    }
    assert!(matches!(
        objects.promised_objects(
            ListLimits::default(),
            ReadLimits::default(),
            &AtomicBool::new(true),
        ),
        Err(girt::ObjectReadError::Cancelled)
    ));
}

/// Retention refuses a partial clone that Git marks only with `remote.origin.promisor`, without
/// `extensions.partialClone`, because its object inventory is incomplete.
#[test]
fn retention_refuses_promisor_remote_without_extension() {
    let partial = partial_clone(ObjectFormat::Sha1);
    let repository = Repository::open(&partial.clone).unwrap();
    assert!(
        repository
            .config()
            .value("extensions", None, "partialClone")
            .is_none()
    );
    let plan = repository.plan_retention(
        &girt::retention::RetentionPolicy::trusted().with_heads(vec![partial.tip]),
        &AtomicBool::new(false),
    );
    assert!(
        matches!(
            plan.outcome,
            girt::retention::RetentionOutcome::Incomplete(
                girt::retention::RetentionError::PartialClone
            )
        ),
        "{:?}",
        plan.outcome
    );
}

fn promisor_markers(clone: &Path) -> usize {
    std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "promisor")
        })
        .count()
}

/// Fetching promised blobs explicitly from Git's `file://` promisor stores them in a new promisor
/// pack, and Git reads them without a lazy fetch of its own.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn missing_blobs_fetch_explicitly(#[case] format: ObjectFormat) {
    use girt::workflow::{self, Environment, NoCallbacks};

    let partial = partial_clone(format);
    let missing: Vec<ObjectId> = git_missing(&partial.clone).into_iter().collect();
    let repository = Repository::open(&partial.clone).unwrap();
    assert_eq!(workflow::promisor_remotes(&repository), ["origin"]);

    let (first, rest) = missing.split_first().unwrap();
    let report = workflow::fetch_missing(
        &repository,
        "origin",
        &[*first, *first],
        &Environment::default(),
        &mut NoCallbacks,
    )
    .unwrap();
    assert_eq!(report.fetched, [*first]);
    assert!(report.already_present.is_empty());
    assert_eq!(promisor_markers(&partial.clone), 2);
    assert_eq!(
        git_missing(&partial.clone),
        rest.iter().copied().collect::<BTreeSet<_>>()
    );
    let shown = git_offline(&partial.clone, &["cat-file", "-p", &first.to_string()]);
    assert!(shown.status.success());

    // Objects already stored are reported rather than requested.
    let repository = Repository::open(&partial.clone).unwrap();
    let report = workflow::fetch_promised(
        &repository,
        &missing,
        &Environment::default(),
        &mut NoCallbacks,
    )
    .unwrap();
    assert_eq!(report.already_present, [*first]);
    assert_eq!(report.fetched, rest);
    assert!(git_missing(&partial.clone).is_empty());
    let objects = Repository::open(&partial.clone).unwrap().objects().unwrap();
    for id in &missing {
        assert_eq!(objects.read(*id).unwrap().unwrap().kind(), ObjectKind::Blob);
    }

    let fsck = git_offline(&partial.clone, &["fsck", "--strict"]);
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    // Git can repack the promisor packs together and still sees every object.
    let repack = git_offline(&partial.clone, &["repack", "-a", "-d", "-q"]);
    assert!(
        repack.status.success(),
        "{}",
        String::from_utf8_lossy(&repack.stderr)
    );
    assert!(
        git_offline(&partial.clone, &["fsck", "--strict"])
            .status
            .success()
    );
    assert!(git_missing(&partial.clone).is_empty());
}

/// Like Git's lazy fetch, promisor remotes are tried in configuration order with the
/// `extensions.partialClone` remote last, and a failing remote falls through to the next.
#[test]
fn promisor_remotes_are_tried_in_git_order() {
    use girt::workflow::{self, Environment, NoCallbacks, TransferError};

    let partial = partial_clone(ObjectFormat::Sha1);
    let missing: Vec<ObjectId> = git_missing(&partial.clone).into_iter().collect();
    let absent = partial.clone.with_file_name("absent");
    for (name, value) in [
        ("extensions.partialClone", "origin"),
        ("remote.broken.url", absent.to_str().unwrap()),
        ("remote.broken.promisor", "true"),
        ("remote.plain.url", absent.to_str().unwrap()),
    ] {
        git(&partial.clone, &["config", name, value], b"");
    }
    let repository = Repository::open(&partial.clone).unwrap();
    assert_eq!(
        workflow::promisor_remotes(&repository),
        ["broken", "origin"]
    );

    assert!(matches!(
        workflow::fetch_missing(
            &repository,
            "plain",
            &missing,
            &Environment::default(),
            &mut NoCallbacks,
        ),
        Err(TransferError::NotPromisor(name)) if name == "plain"
    ));
    assert!(
        workflow::fetch_missing(
            &repository,
            "broken",
            &missing,
            &Environment::default(),
            &mut NoCallbacks,
        )
        .is_err()
    );
    let report = workflow::fetch_promised(
        &repository,
        &missing,
        &Environment::default(),
        &mut NoCallbacks,
    )
    .unwrap();
    assert_eq!(report.fetched, missing);
    assert!(git_missing(&partial.clone).is_empty());
}
