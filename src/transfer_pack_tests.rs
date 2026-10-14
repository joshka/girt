//! Configured-remote fetch packs references the way `git fetch`'s automatic maintenance does.
//!
//! Every expected layout comes from running the Git executable on a twin repository with the same
//! configuration. No upstream implementation or test source is used.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

fn git(directory: &Path, args: &[&str]) -> String {
    let output = git_output(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git_output(directory: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Pack Author")
        .env("GIT_AUTHOR_EMAIL", "pack@example.com")
        .env("GIT_COMMITTER_NAME", "Pack Author")
        .env("GIT_COMMITTER_EMAIL", "pack@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        // Run Git's maintenance in the foreground so its effects are complete on return.
        .args(["-c", "maintenance.autoDetach=false"])
        .args(args)
        .output()
        .unwrap()
}

const SPECS: [&str; 2] = [
    "+refs/heads/*:refs/remotes/origin/*",
    "+refs/tags/*:refs/tags/*",
];

/// An origin advertising exactly `refs` branches and annotated tags, `main` included, and two
/// empty bare destinations configured identically: one for girt and one for Git.
struct Fixture {
    _root: tempfile::TempDir,
    origin: PathBuf,
    girt: PathBuf,
    oracle: PathBuf,
}

fn fixture(format: &str, refs: usize, config: &[(&str, &str)]) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(
        &origin,
        &[
            "init",
            "-q",
            &format!("--object-format={format}"),
            "-b",
            "main",
            ".",
        ],
    );
    git(&origin, &["config", "maintenance.auto", "false"]);
    let mut advertised = 0;
    for n in 0.. {
        std::fs::write(origin.join("file"), format!("{n}\n")).unwrap();
        git(&origin, &["add", "file"]);
        git(&origin, &["commit", "-q", "-m", &format!("c{n}")]);
        if n == 0 {
            advertised += 1;
        }
        if advertised < refs {
            git(&origin, &["tag", "-a", "-m", "t", &format!("v{n}")]);
            advertised += 1;
        }
        if advertised < refs {
            git(&origin, &["branch", &format!("b{n}")]);
            advertised += 1;
        }
        if advertised == refs {
            break;
        }
    }
    let destination = |name: &str| {
        git(
            root.path(),
            &[
                "init",
                "-q",
                "--bare",
                &format!("--object-format={format}"),
                name,
            ],
        );
        let path = root.path().join(name);
        git(
            &path,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        for (key, value) in config {
            git(&path, &["config", key, value]);
        }
        path
    };
    Fixture {
        girt: destination("girt.git"),
        oracle: destination("oracle.git"),
        origin,
        _root: root,
    }
}

fn girt_fetch(path: &Path) -> Result<FetchOutcome, TransferError> {
    Repository::open(path).unwrap().fetch(
        "origin",
        &FetchOptions::new(SPECS),
        &Environment::from_process(),
        &mut NoCallbacks,
    )
}

/// Runs `git fetch`, returning whether it exited successfully.
fn git_fetch(path: &Path) -> bool {
    let mut args = vec!["fetch", "-q", "origin"];
    args.extend(SPECS);
    git_output(path, &args).status.success()
}

/// Loose reference files and the `packed-refs` bytes: the layout Git and girt must agree on.
fn layout(git_dir: &Path) -> (Vec<String>, Option<Vec<u8>>) {
    let mut loose = Vec::new();
    let mut pending = vec![git_dir.join("refs")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path.strip_prefix(git_dir).unwrap();
                loose.push(name.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    loose.sort();
    (loose, std::fs::read(git_dir.join("packed-refs")).ok())
}

/// Asserts girt stores what Git stores: the same `packed-refs` bytes and reference values
/// everywhere, and the same loose files on Unix. Off Unix girt deliberately keeps every loose copy
/// of a packed reference (roadmap B08), so each packed name must still have its loose file there.
fn assert_same_layout(girt: &Path, oracle: &Path) {
    let (loose, packed_refs) = layout(girt);
    let (oracle_loose, oracle_packed_refs) = layout(oracle);
    assert_eq!(packed_refs, oracle_packed_refs);
    let refs = |dir: &Path| git(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    assert_eq!(refs(girt), refs(oracle));
    if cfg!(unix) {
        assert_eq!(loose, oracle_loose);
    } else {
        let packed_names = String::from_utf8(packed_refs.unwrap_or_default()).unwrap();
        let records = packed_names
            .lines()
            .filter(|line| !line.starts_with(['#', '^']));
        for name in records.filter_map(|line| line.split_once(' ')) {
            assert!(
                loose.iter().any(|loose| loose == name.1),
                "{} not retained",
                name.1
            );
        }
    }
}

/// A fetch leaves the same loose files and byte-identical `packed-refs` as `git fetch` with the
/// same configuration: packed from Git's automatic threshold of 16 loose references, loose below
/// it, and loose whenever configuration disables Git's automatic pack-refs task.
#[rstest::rstest]
#[case::many_sha1("sha1", 41, &[], true)]
#[case::many_sha256("sha256", 41, &[], true)]
#[case::at_threshold("sha1", 16, &[], true)]
#[case::below_threshold("sha1", 15, &[], false)]
#[case::maintenance_off("sha1", 41, &[("maintenance.auto", "false")], false)]
#[case::gc_auto_zero("sha1", 41, &[("gc.auto", "0")], false)]
#[case::gc_auto_negative("sha1", 41, &[("gc.auto", "-1")], false)]
#[case::maintenance_overrides_gc(
    "sha1", 41, &[("gc.auto", "0"), ("maintenance.auto", "true")], true
)]
#[case::task_disabled("sha1", 41, &[("maintenance.pack-refs.enabled", "false")], false)]
#[case::gc_strategy("sha1", 41, &[("maintenance.strategy", "gc")], false)]
#[case::incremental_strategy("sha1", 41, &[("maintenance.strategy", "incremental")], false)]
#[case::geometric_strategy("sha1", 41, &[("maintenance.strategy", "geometric")], true)]
#[case::task_enabled_by_strategy_override(
    "sha1",
    41,
    &[("maintenance.strategy", "incremental"), ("maintenance.pack-refs.enabled", "true")],
    true
)]
#[case::commit_graph_on("sha1", 41, &[("fetch.writeCommitGraph", "true")], true)]
#[case::commit_graph_off("sha1", 41, &[("fetch.writeCommitGraph", "false")], true)]
fn fetch_packs_references_like_git(
    #[case] format: &str,
    #[case] refs: usize,
    #[case] config: &[(&str, &str)],
    #[case] packed: bool,
) {
    let fixture = fixture(format, refs, config);
    let outcome = girt_fetch(&fixture.girt).unwrap();
    assert_eq!(outcome.updated.len(), refs);
    assert!(git_fetch(&fixture.oracle));

    assert_same_layout(&fixture.girt, &fixture.oracle);
    assert_eq!(layout(&fixture.girt).1.is_some(), packed);
    git(&fixture.girt, &["fsck", "--strict", "--no-dangling"]);
}

/// Invalid or unknown maintenance configuration disables the task: Git's maintenance fails after
/// its fetch has updated the references, leaving them loose, and girt leaves them loose too. Git
/// 2.56 rejects `maintenance.strategy=none` as unknown, even with the task enabled explicitly.
///
/// Unlike `git fetch`, whose exit status becomes 128 for an invalid `maintenance.auto` or
/// `gc.auto`, girt's fetch still succeeds: its references were updated.
#[rstest::rstest]
#[case::maintenance_auto(&[("maintenance.auto", "junk")], false)]
#[case::gc_auto(&[("gc.auto", "junk")], false)]
#[case::task_enabled(&[("maintenance.pack-refs.enabled", "junk")], true)]
#[case::strategy(&[("maintenance.strategy", "unknown")], true)]
#[case::strategy_with_task(
    &[("maintenance.strategy", "unknown"), ("maintenance.pack-refs.enabled", "true")], true
)]
#[case::none_strategy_with_task(
    &[("maintenance.strategy", "none"), ("maintenance.pack-refs.enabled", "true")], true
)]
fn invalid_maintenance_configuration_leaves_references_loose(
    #[case] config: &[(&str, &str)],
    #[case] git_succeeds: bool,
) {
    let fixture = fixture("sha1", 41, config);
    girt_fetch(&fixture.girt).unwrap();
    assert_eq!(git_fetch(&fixture.oracle), git_succeeds);

    assert_same_layout(&fixture.girt, &fixture.oracle);
    let (loose, packed_refs) = layout(&fixture.girt);
    assert_eq!(packed_refs, None);
    assert_eq!(loose.len(), 41);
}

thread_local! {
    /// Runs inside [`Repository::fetch`] after its reference transaction commits and before the
    /// automatic pack.
    pub(super) static BEFORE_AUTO_PACK: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// A `packed-refs.lock` taken by another process after the fetch's references are updated makes
/// packing fail, but the fetch succeeds with every reference updated and the lock untouched, as
/// `git fetch` exits successfully when its pack-refs task cannot lock.
#[test]
fn fetch_succeeds_when_packing_cannot_lock() {
    let fixture = fixture("sha1", 41, &[]);
    for git_dir in [&fixture.girt, &fixture.oracle] {
        let lock = git_dir.join("packed-refs.lock");
        if git_dir == &fixture.oracle {
            std::fs::write(&lock, b"held").unwrap();
            assert!(git_fetch(git_dir));
        } else {
            BEFORE_AUTO_PACK.with(|hook| {
                hook.set(Some(Box::new(move || {
                    std::fs::write(lock, b"held").unwrap()
                })))
            });
            let outcome = girt_fetch(git_dir).unwrap();
            assert_eq!(outcome.updated.len(), 41);
        }
    }
    let hook_ran = BEFORE_AUTO_PACK.with(|hook| hook.take().is_none());
    assert!(hook_ran);

    for git_dir in [&fixture.girt, &fixture.oracle] {
        assert_eq!(
            std::fs::read(git_dir.join("packed-refs.lock")).unwrap(),
            b"held"
        );
        assert_eq!(layout(git_dir).0.len(), 41);
    }
    assert_same_layout(&fixture.girt, &fixture.oracle);
}

/// A held `packed-refs.lock` makes the public task fail with no effects.
#[test]
fn held_packed_refs_lock_leaves_references_loose() {
    let fixture = fixture("sha1", 41, &[("maintenance.auto", "false")]);
    girt_fetch(&fixture.girt).unwrap();
    git(&fixture.girt, &["config", "maintenance.auto", "true"]);
    let lock = fixture.girt.join("packed-refs.lock");
    std::fs::write(&lock, b"held").unwrap();
    let before = layout(&fixture.girt);

    let repository = Repository::open(&fixture.girt).unwrap();
    let objects = repository.objects(crate::PackLimits::trusted()).unwrap();
    let result = repository.auto_pack_references(&objects);
    assert!(
        matches!(
            result,
            Err(crate::refs::PackRefsError::Reference(
                crate::refs::ReferenceError::Locked(_)
            ))
        ),
        "{result:?}"
    );
    assert_eq!(std::fs::read(&lock).unwrap(), b"held");
    assert_eq!(layout(&fixture.girt), before);
    assert_eq!(before.0.len(), 41);
}

/// After other writes, the public task packs like `git maintenance run --auto`, including
/// references the fetch did not write, and honors the same configuration.
#[rstest::rstest]
#[case::default(&[], true)]
#[case::disabled(&[("maintenance.auto", "false")], false)]
fn auto_pack_references_matches_git_maintenance(
    #[case] config: &[(&str, &str)],
    #[case] packed: bool,
) {
    let fixture = fixture("sha1", 5, config);
    let head = git(&fixture.origin, &["rev-parse", "HEAD"]);
    for destination in [&fixture.girt, &fixture.oracle] {
        git(
            destination,
            &["fetch", "-q", "--no-auto-maintenance", "origin", SPECS[0]],
        );
        for n in 0..30 {
            git(
                destination,
                &["update-ref", &format!("refs/jj/keep/k{n}"), head.trim()],
            );
        }
    }
    let repository = Repository::open(&fixture.girt).unwrap();
    let objects = repository.objects(crate::PackLimits::trusted()).unwrap();
    let report = repository.auto_pack_references(&objects).unwrap();
    // A no-op fetch runs Git's automatic maintenance, gated by the same configuration.
    git(&fixture.oracle, &["fetch", "-q", "origin", SPECS[0]]);

    assert_eq!(report.is_some_and(|report| report.rewritten), packed);
    assert_same_layout(&fixture.girt, &fixture.oracle);
}
