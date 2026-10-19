//! Repacking partial clones made by Git into a promisor pack and an ordinary pack, checked with
//! Git's own fsck, missing-object listing and gc.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::retention::{
    IsolatedRepackReport, MaintenanceIsolation, PartialClonePolicy, RepackLimits, RetentionPolicy,
};
use girt::{ObjectFormat, ObjectId, ObjectKind, Repository};
use rstest::rstest;

/// Runs Git with isolated configuration, fixed identities and dates, never fetching a missing
/// object.
fn git_output(dir: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
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
    child.wait_with_output().unwrap()
}

fn git(dir: &Path, args: &[&str], input: &[u8]) -> String {
    let output = git_output(dir, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct PrivateFixture;

impl MaintenanceIsolation for PrivateFixture {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

struct Clone {
    _root: tempfile::TempDir,
    path: PathBuf,
    /// Objects the local commits added, all loose.
    local: BTreeSet<String>,
}

/// A source with commits on `main` and on `side`, where `side` alone holds `witness`'s blob.
fn source(root: &Path, format: ObjectFormat) -> PathBuf {
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    let format_arg = format!("--object-format={format}");
    git(
        &source,
        &["init", "-q", "--template=", "-b", "main", &format_arg],
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
    git(&source, &["checkout", "-q", "-b", "side"], b"");
    std::fs::write(source.join("witness"), "only the side tree names this\n").unwrap();
    git(&source, &["add", "witness"], b"");
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "side"],
        b"",
    );
    git(&source, &["checkout", "-q", "main"], b"");
    source
}

/// A `blob:none` clone of [`source`], a filtered fetch of one more commit (so two promisor packs),
/// and a local commit on `main` with a new loose blob. With
/// `witness`, the local tree also names the side branch's promised blob, and the side branch is
/// deleted, so only an unreachable promisor tree still promises that blob.
fn partial_clone(format: ObjectFormat, witness: bool) -> Clone {
    partial_clone_with(format, witness, true)
}

/// [`partial_clone`], optionally without the second filtered fetch, leaving one promisor pack.
fn partial_clone_with(format: ObjectFormat, witness: bool, second_fetch: bool) -> Clone {
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path(), format);
    let path = root.path().join("clone");
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
            path.to_str().unwrap(),
        ],
        b"",
    );
    if second_fetch {
        std::fs::write(source.join("a"), "e\n").unwrap();
        git(
            &source,
            &["commit", "-q", "--no-gpg-sign", "-am", "three"],
            b"",
        );
        git(&path, &["fetch", "-q", "origin"], b"");
    }
    let blob = git(&path, &["hash-object", "-w", "--stdin"], b"local\n");
    let mut entries = git(&path, &["ls-tree", "HEAD"], b"");
    entries.push_str(&format!("\n100644 blob {blob}\tlocal\n"));
    if witness {
        let promised = git(&path, &["rev-parse", "origin/side:witness"], b"");
        entries.push_str(&format!("100644 blob {promised}\twitness\n"));
        git(
            &path,
            &["update-ref", "-d", "refs/remotes/origin/side"],
            b"",
        );
    }
    let tree = git(&path, &["mktree", "--missing"], entries.as_bytes());
    let commit = git(
        &path,
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
    git(&path, &["update-ref", "refs/heads/main", &commit], b"");
    Clone {
        _root: root,
        path,
        local: [blob, tree, commit].into(),
    }
}

/// The `?` lines of `git rev-list --objects --all --missing=print`.
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

fn pack_files(clone: &Path, extension: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    paths.sort();
    paths
}

fn pack_objects(pack: &Path) -> BTreeSet<String> {
    git(
        pack.parent().unwrap(),
        &["show-index"],
        &std::fs::read(pack.with_extension("idx")).unwrap(),
    )
    .lines()
    .map(|line| line.split_whitespace().nth(1).unwrap().to_owned())
    .collect()
}

/// Every object in a `.pack` that has a `.promisor` marker.
fn promisor_objects(clone: &Path) -> BTreeSet<String> {
    pack_files(clone, "pack")
        .iter()
        .filter(|pack| pack.with_extension("promisor").exists())
        .flat_map(|pack| pack_objects(pack))
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

fn repack(clone: &Path) -> IsolatedRepackReport {
    Repository::open(clone)
        .unwrap()
        .repack_retained_isolated(
            &mut PrivateFixture,
            &policy(),
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
}

/// `git fsck --strict` succeeds and reports nothing but dangling objects, such as the witness
/// fixture's unreachable side commit.
fn fsck(clone: &Path) {
    let output = git_output(clone, &["fsck", "--strict", "--no-progress"], b"");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && output.stderr.is_empty()
            && stdout.lines().all(|line| line.starts_with("dangling ")),
        "fsck: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn blob_none_clone_round_trips_through_git(
    #[case] format: ObjectFormat,
    #[values(false, true)] witness: bool,
) {
    let clone = partial_clone(format, witness);
    let old_packs = pack_files(&clone.path, "pack");
    assert_eq!(old_packs.len(), 2);
    let old_promisor = promisor_objects(&clone.path);
    let expected_missing = missing(&clone.path);
    let expected_boundaries = boundaries(&clone.path);
    assert!(!expected_missing.is_empty());
    fsck(&clone.path);

    let report = repack(&clone.path);

    let promisor = report.promisor.expect("a promisor pack");
    let ordinary = report.ordinary.expect("an ordinary pack");
    assert!(report.durable);
    assert!(report.retained.is_empty(), "{:?}", report.retained);
    fsck(&clone.path);
    assert_eq!(missing(&clone.path), expected_missing);
    assert_eq!(boundaries(&clone.path), expected_boundaries);

    let markers = pack_files(&clone.path, "promisor");
    assert_eq!(markers.len(), 1, "{markers:?}");
    let new_promisor = markers[0].with_extension("pack");
    assert!(
        new_promisor
            .to_str()
            .unwrap()
            .ends_with(&format!("pack-{}.pack", promisor.written.checksum))
    );
    assert_eq!(std::fs::read(&markers[0]).unwrap(), b"");
    // Every object from the old promisor pack, reachable or not, and nothing local.
    assert_eq!(pack_objects(&new_promisor), old_promisor);
    let ordinary_pack = clone
        .path
        .join(".git/objects/pack")
        .join(format!("pack-{}.pack", ordinary.written.checksum));
    assert_eq!(pack_objects(&ordinary_pack), clone.local);
    // Both old promisor packs retired, each marker after its index and pack.
    for old in &old_packs {
        assert!(!old.exists() && !old.with_extension("promisor").exists());
        let position = |path: &Path| {
            report
                .removed
                .iter()
                .position(|removed| removed.file_name() == path.file_name())
        };
        let marker = position(&old.with_extension("promisor")).unwrap();
        assert!(position(&old.with_extension("idx")).unwrap() < marker);
        assert!(position(old).unwrap() < marker);
    }
    if witness {
        let witness = git(&clone.path, &["rev-parse", "HEAD:witness"], b"");
        assert!(expected_missing.contains(&witness));
    }

    // Git's own gc still works on the result and keeps the same promises.
    git(&clone.path, &["gc", "-q"], b"");
    fsck(&clone.path);
    assert_eq!(missing(&clone.path), expected_missing);
    assert_eq!(boundaries(&clone.path), expected_boundaries);
}

/// A `.promisor` marker with no pack, as an interrupted publication leaves, promises nothing and
/// changes nothing.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn leftover_marker_without_a_pack_is_harmless(#[case] format: ObjectFormat) {
    let clone = partial_clone(format, true);
    let expected_missing = missing(&clone.path);
    let expected_boundaries = boundaries(&clone.path);
    let stray = clone.path.join(format!(
        ".git/objects/pack/pack-{}.promisor",
        "0".repeat(ObjectId::for_blob(format, b"").to_string().len())
    ));
    std::fs::write(&stray, b"").unwrap();

    assert_eq!(boundaries(&clone.path), expected_boundaries);
    fsck(&clone.path);
    let report = repack(&clone.path);
    assert!(report.promisor.is_some());
    assert!(stray.exists());
    assert!(!report.removed.contains(&stray));
    assert_eq!(boundaries(&clone.path), expected_boundaries);
    assert_eq!(missing(&clone.path), expected_missing);
    fsck(&clone.path);
}

/// Plain `repack_retained` keeps refusing a partial clone even with the promised policy.
#[test]
fn plain_repack_still_refuses() {
    let clone = partial_clone(ObjectFormat::Sha1, false);
    let error = Repository::open(&clone.path)
        .unwrap()
        .repack_retained(&policy(), RepackLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        error,
        girt::retention::RepackError::Incomplete(girt::retention::RetentionError::PartialClone)
    ));
}

/// With one promisor pack, copying its entries in order reproduces it byte for byte. The pack is
/// reused under its own name, keeps the marker Git's clone wrote, and nothing retires.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn byte_identical_promisor_pack_is_reused_with_its_marker(#[case] format: ObjectFormat) {
    let clone = partial_clone_with(format, false, false);
    let old = pack_files(&clone.path, "pack");
    assert_eq!(old.len(), 1);
    let marker = old[0].with_extension("promisor");
    let marker_bytes = std::fs::read(&marker).unwrap();
    assert!(
        !marker_bytes.is_empty(),
        "Git's clone lists advertised tips"
    );
    let pack_bytes = std::fs::read(&old[0]).unwrap();
    let expected_missing = missing(&clone.path);

    let report = repack(&clone.path);

    let promisor = report.promisor.expect("a promisor pack");
    assert!(
        old[0]
            .to_str()
            .unwrap()
            .ends_with(&format!("pack-{}.pack", promisor.written.checksum))
    );
    assert_eq!(std::fs::read(&old[0]).unwrap(), pack_bytes);
    assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
    assert!(report.removed.is_empty() && report.retained.is_empty());
    assert_eq!(pack_files(&clone.path, "promisor").len(), 1);
    assert_eq!(missing(&clone.path), expected_missing);
    fsck(&clone.path);
}
