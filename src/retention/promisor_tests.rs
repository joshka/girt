//! Fault injection for the isolated partial-clone repack, against clones made by Git.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use super::promisor::{Checkpoint, IsolatedRepackFailure, IsolatedRepackReport, RetainReason};
use super::{PartialClonePolicy, RepackLimits, RetentionPolicy, RetireCause};
use crate::pack::publish::Step;
use crate::{ObjectFormat, ObjectId, ObjectKind, Repository};

fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "2001-09-17T00:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2001-09-17T00:00:00+0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A `blob:none` clone of two commits, a filtered fetch of a third (two promisor packs), and one
/// local commit whose new blob and tree are loose.
fn partial_clone(format: ObjectFormat) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let format_arg = format!("--object-format={format}");
    git(
        &source,
        &["init", "-q", "--template=", "-b", "main", &format_arg],
        b"",
    );
    git(&source, &["config", "uploadpack.allowFilter", "true"], b"");
    std::fs::write(source.join("a"), "a\n").unwrap();
    git(&source, &["add", "."], b"");
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "one"],
        b"",
    );
    std::fs::write(source.join("a"), "b\n").unwrap();
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-am", "two"],
        b"",
    );
    let clone = root.path().join("clone");
    let url = format!("file://{}", source.display());
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--template=",
            "--no-local",
            "--filter=blob:none",
            "--no-checkout",
            &url,
            clone.to_str().unwrap(),
        ],
        b"",
    );
    std::fs::write(source.join("a"), "c\n").unwrap();
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-am", "three"],
        b"",
    );
    git(&clone, &["fetch", "-q", "origin"], b"");
    let blob = git(&clone, &["hash-object", "-w", "--stdin"], b"local\n");
    let listing = git(&clone, &["ls-tree", "HEAD"], b"");
    let tree = git(
        &clone,
        &["mktree", "--missing"],
        format!("{listing}\n100644 blob {blob}\tlocal\n").as_bytes(),
    );
    let commit = git(
        &clone,
        &[
            "commit-tree",
            "--no-gpg-sign",
            &tree,
            "-p",
            "HEAD",
            "-m",
            "local",
        ],
        b"",
    );
    git(&clone, &["update-ref", "refs/heads/main", &commit], b"");
    (root, clone)
}

fn missing(clone: &Path) -> BTreeSet<String> {
    git(
        clone,
        &["rev-list", "--objects", "--all", "--missing=print"],
        b"",
    )
    .lines()
    .filter_map(|line| line.strip_prefix('?'))
    .map(str::to_owned)
    .collect()
}

/// Every object stored in a promisor pack.
fn promisor_objects(clone: &Path) -> BTreeSet<ObjectId> {
    let objects = Repository::open(clone).unwrap().objects().unwrap();
    objects
        .packs()
        .filter(|pack| pack.is_promisor())
        .flat_map(|pack| pack.ids().collect::<Vec<_>>())
        .collect()
}

fn policy() -> RetentionPolicy {
    // A future cutoff makes the fixture's fresh packs retirable in this private repository.
    RetentionPolicy::default()
        .with_partial_clone(PartialClonePolicy::Promised)
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(3600))
}

fn boundaries(clone: &Path) -> BTreeMap<ObjectId, ObjectKind> {
    let plan = Repository::open(clone)
        .unwrap()
        .plan_retention(&policy(), &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    plan.promised_boundaries
}

fn run(
    clone: &Path,
    checkpoint: &mut dyn FnMut(Checkpoint) -> io::Result<()>,
) -> Result<IsolatedRepackReport, IsolatedRepackFailure> {
    Repository::open(clone).unwrap().repack_isolated_at(
        &policy(),
        RepackLimits::default(),
        &AtomicBool::new(false),
        checkpoint,
    )
}

fn files(clone: &Path, extension: &str) -> Vec<PathBuf> {
    std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect()
}

/// After a fault at each visible change, the clone keeps every promise and every promisor object,
/// and a retry converges on one promisor pack.
#[rstest::rstest]
#[case::marker(Checkpoint::Promisor(Step::Marker))]
#[case::promisor_pack(Checkpoint::Promisor(Step::Pack))]
#[case::promisor_index(Checkpoint::Promisor(Step::Index))]
#[case::ordinary_pack(Checkpoint::Ordinary(Step::Pack))]
#[case::ordinary_index(Checkpoint::Ordinary(Step::Index))]
#[case::old_index(Checkpoint::RemoveIndex)]
#[case::old_pack(Checkpoint::RemovePack)]
#[case::old_marker(Checkpoint::RemoveMarker)]
fn fault_keeps_promises_and_retry_converges(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] fault: Checkpoint,
) {
    let (_root, clone) = partial_clone(format);
    let expected_missing = missing(&clone);
    let expected_boundaries = boundaries(&clone);
    let promisor = promisor_objects(&clone);
    assert!(!promisor.is_empty());

    let mut reached = false;
    let failure = run(&clone, &mut |at| {
        if at == fault {
            reached = true;
            Err(io::Error::other("injected fault"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(reached, "{fault:?} was never reached");
    assert!(
        matches!(failure.cause, RetireCause::Io(_) | RetireCause::Repack(_)),
        "{failure:?}"
    );

    let check = |clone: &Path| {
        assert_eq!(boundaries(clone), expected_boundaries);
        assert_eq!(missing(clone), expected_missing);
        let kept = promisor_objects(clone);
        assert!(
            promisor.is_subset(&kept),
            "a promisor object lost its promisor pack"
        );
        git(clone, &["fsck", "--strict", "--no-progress"], b"");
    };
    check(&clone);

    let report = run(&clone, &mut |_| Ok(())).unwrap();
    assert!(report.promisor.is_some());
    assert!(report.retained.is_empty(), "{:?}", report.retained);
    check(&clone);
    assert_eq!(promisor_objects(&clone), promisor);
    // A marker left without its pack by the fault is harmless; only one pair is a promisor pack.
    let promisor_packs: Vec<_> = files(&clone, "promisor")
        .into_iter()
        .filter(|marker| marker.with_extension("idx").exists())
        .collect();
    assert_eq!(promisor_packs.len(), 1, "{promisor_packs:?}");
}

/// A partial clone whose only promisor pack is empty has nothing to copy, so no new promisor pack
/// can prove coverage and the old one stays.
#[test]
fn empty_promisor_pack_without_coverage_is_retained() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("r");
    git(
        root.path(),
        &["init", "-q", "--template=", repo.to_str().unwrap()],
        b"",
    );
    git(&repo, &["config", "remote.origin.promisor", "true"], b"");
    git(
        &repo,
        &["config", "remote.origin.url", "file:///absent"],
        b"",
    );
    let name = git(
        &repo,
        &["pack-objects", "-q", ".git/objects/pack/pack"],
        b"",
    );
    let marker = repo.join(format!(".git/objects/pack/pack-{name}.promisor"));
    std::fs::write(&marker, b"").unwrap();
    let blob = git(&repo, &["hash-object", "-w", "--stdin"], b"local\n");
    git(&repo, &["update-ref", "refs/tags/local", &blob], b"");

    let report = run(&repo, &mut |_| Ok(())).unwrap();
    assert!(report.promisor.is_none());
    assert_eq!(report.ordinary.map(|pack| pack.written.objects), Some(1));
    assert_eq!(report.retained.len(), 1);
    assert_eq!(
        report.retained[0].reason,
        RetainReason::NoDurablePromisorCoverage
    );
    assert!(report.removed.is_empty());
    assert!(marker.exists());
    assert!(marker.with_extension("pack").exists());
}

/// A `.keep` file protects an old promisor pack; its objects are still copied.
#[test]
fn kept_promisor_pack_is_retained() {
    let (_root, clone) = partial_clone(ObjectFormat::Sha1);
    let mut old = files(&clone, "pack");
    old.sort();
    assert_eq!(old.len(), 2);
    std::fs::write(old[0].with_extension("keep"), b"").unwrap();
    let report = run(&clone, &mut |_| Ok(())).unwrap();
    assert!(report.promisor.is_some());
    assert_eq!(report.retained.len(), 1);
    assert_eq!(report.retained[0].pack.file_name(), old[0].file_name());
    assert_eq!(report.retained[0].reason, RetainReason::Protected);
    assert!(old[0].exists() && old[0].with_extension("promisor").exists());
    assert!(!old[1].exists() && !old[1].with_extension("promisor").exists());
}

/// The default policy refuses a partial clone before any change.
#[test]
fn refuse_policy_changes_nothing() {
    let (_root, clone) = partial_clone(ObjectFormat::Sha1);
    let before: Vec<_> = std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let failure = Repository::open(&clone)
        .unwrap()
        .repack_isolated_at(
            &RetentionPolicy::default(),
            RepackLimits::default(),
            &AtomicBool::new(false),
            &mut |_| Ok(()),
        )
        .unwrap_err();
    assert!(
        matches!(
            failure.cause,
            RetireCause::Incomplete(super::RetentionError::PartialClone)
        ),
        "{failure:?}"
    );
    assert!(failure.report.promisor.is_none() && failure.report.ordinary.is_none());
    let after: Vec<_> = std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(before.len(), after.len());
}

/// A failed directory sync leaves its name visible; the failure lists exactly the visible names
/// and marks the one whose durability is unknown. After the final sync fails, the pair is visible
/// and the report names its leg.
#[rstest::rstest]
#[case::marker_sync(Step::SyncMarker, &["promisor"])]
#[case::pack_sync(Step::SyncPack, &["promisor", "pack"])]
#[case::final_sync(Step::SyncIndex, &["promisor", "pack", "idx"])]
fn failed_sync_reports_visible_names_and_uncertain_one(
    #[case] fault: Step,
    #[case] visible: &[&str],
) {
    let (_root, clone) = partial_clone(ObjectFormat::Sha1);
    let failure = run(&clone, &mut |at| {
        if at == Checkpoint::Promisor(fault) {
            Err(io::Error::other("injected sync failure"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    let report = &failure.report;
    assert_eq!(report.visible.len(), visible.len(), "{:?}", report.visible);
    for (path, extension) in report.visible.iter().zip(visible) {
        assert_eq!(path.extension().unwrap(), *extension);
        assert!(path.exists(), "{} is not visible", path.display());
    }
    assert_eq!(failure.uncertain.as_ref(), report.visible.last());
    assert!(report.ordinary.is_none() && report.removed.is_empty() && !report.durable);
    let pair_visible = fault == Step::SyncIndex;
    assert_eq!(report.promisor.is_some(), pair_visible);
    if let Some(promisor) = &report.promisor {
        let name = format!("pack-{}.idx", promisor.written.checksum);
        assert!(report.visible[2].ends_with(name));
    }
}

/// A new promisor pack appearing, or the partial-clone configuration going away, between
/// publication and retirement stops before anything old is unlinked.
#[rstest::rstest]
#[case::new_promisor_pack(true)]
#[case::config_changed(false)]
fn change_after_publication_unlinks_nothing(#[case] new_pack: bool) {
    let (_root, clone) = partial_clone(ObjectFormat::Sha1);
    let old: Vec<_> = files(&clone, "pack");
    let mut injected = None;
    let failure = run(&clone, &mut |at| {
        if at == Checkpoint::Published {
            if new_pack {
                let blob = git(&clone, &["hash-object", "-w", "--stdin"], b"late\n");
                let name = git(
                    &clone,
                    &["pack-objects", "-q", ".git/objects/pack/pack"],
                    format!("{blob}\n").as_bytes(),
                );
                let marker = clone.join(format!(".git/objects/pack/pack-{name}.promisor"));
                std::fs::write(&marker, b"").unwrap();
                injected = Some(marker);
            } else {
                git(
                    &clone,
                    &["config", "--unset", "remote.origin.promisor"],
                    b"",
                );
                git(
                    &clone,
                    &["config", "--unset", "remote.origin.partialclonefilter"],
                    b"",
                );
            }
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(failure.cause, RetireCause::Changed), "{failure:?}");
    assert!(failure.report.promisor.is_some());
    assert!(failure.report.removed.is_empty());
    for pack in &old {
        assert!(pack.exists() && pack.with_extension("idx").exists());
        assert!(pack.with_extension("promisor").exists());
    }
    if let Some(marker) = injected {
        assert!(marker.exists() && marker.with_extension("pack").exists());
    }
}
