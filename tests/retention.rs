//! Retention observations against independently created Git objects and histories.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::retention::{RetentionOutcome, RetentionPolicy};
use girt::{ObjectId, Repository};

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
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
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

fn fixture_format(format: &str) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let first = git(root.path(), &["commit-tree", &tree], b"one\n");
    let second = git(root.path(), &["commit-tree", &tree, "-p", &first], b"two\n");
    git(root.path(), &["update-ref", "refs/heads/main", &first], b"");
    let log = root.path().join("logs/refs/heads/deleted");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(
        log,
        format!(
            "{} {second} A <a@example.com> 1700000000 +0000\tupdate\n",
            ObjectId::null(if format == "sha1" {
                girt::ObjectFormat::Sha1
            } else {
                girt::ObjectFormat::Sha256
            })
        ),
    )
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, first.parse().unwrap(), second.parse().unwrap())
}

fn fixture() -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    fixture_format("sha1")
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn deleted_reflog_and_git_closure_are_retained(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert_eq!(plan.outcome, RetentionOutcome::Complete);
    assert!(plan.roots.contains(&first));
    assert!(plan.roots.contains(&second));
    let expected: std::collections::BTreeSet<ObjectId> = git(
        root.path(),
        &[
            "rev-list",
            "--objects",
            &first.to_string(),
            &second.to_string(),
        ],
        b"",
    )
    .lines()
    .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
    .collect();
    assert_eq!(plan.reachable, expected);
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
}

#[test]
fn damaged_history_and_missing_object_cannot_complete() {
    let (root, repo, _, second) = fixture();
    let log = root.path().join("logs/refs/heads/deleted");
    fs::write(
        &log,
        format!(
            "{} {second} incomplete",
            ObjectId::null(repo.object_format())
        ),
    )
    .unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&second));
    fs::remove_file(log).unwrap();
    let missing: ObjectId = "1111111111111111111111111111111111111111".parse().unwrap();
    let policy = RetentionPolicy {
        heads: vec![missing],
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&missing));
}

#[test]
fn expiry_candidates_do_not_remove_live_or_reflog_observation() {
    let (_root, repo, first, second) = fixture();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_unreachable_before: Some(1700000001),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.roots.contains(&second));
    assert!(plan.reachable.contains(&second));
    assert!(plan.required.contains(&first));
    assert!(!plan.required.contains(&second));
    let name = girt::refs::RefName::new(b"refs/heads/deleted").unwrap();
    assert_eq!(plan.reflog_expiry_candidates[&name], [1]);
}

#[test]
fn detached_linked_head_and_private_index_are_roots() {
    let (root, repo, _first, second) = fixture();
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    let checkout = root.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &second.to_string(),
        ],
        b"",
    );
    let blob = git(
        &checkout,
        &["hash-object", "-w", "--stdin"],
        b"staged data\n",
    );
    git(
        &checkout,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},staged.txt"),
        ],
        b"",
    );
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
    assert!(plan.strong_roots.contains(&blob.parse().unwrap()));
}

#[test]
fn bounded_and_cancelled_scans_report_incomplete() {
    let (_root, repo, _, _) = fixture();
    let policy = RetentionPolicy {
        max_entries: 0,
        ..Default::default()
    };
    assert!(
        !repo
            .plan_retention(&policy, &AtomicBool::new(false))
            .is_complete()
    );
    let cancel = AtomicBool::new(true);
    assert!(
        !repo
            .plan_retention(&RetentionPolicy::default(), &cancel)
            .is_complete()
    );
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn reftable_history_survives_reference_deletion_without_log_edit(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--ref-format=reftable",
            &format!("--object-format={format}"),
            "--template=",
            ".",
        ],
        b"",
    );
    git(
        root.path(),
        &["config", "core.logAllRefUpdates", "true"],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let tip: ObjectId = git(root.path(), &["commit-tree", &tree], b"topic\n")
        .parse()
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "create",
            "refs/heads/topic",
            &tip.to_string(),
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let name = girt::refs::RefName::new(b"refs/heads/topic").unwrap();
    repo.references()
        .unwrap()
        .delete_without_reflog(
            &name,
            girt::refs::Expected::Value(girt::refs::Target::Direct(tip)),
        )
        .unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.roots.contains(&tip));
    assert!(plan.reachable.contains(&tip));
}

#[test]
fn pseudoref_roots_and_recent_objects_hook_policy_are_explicit() {
    let (root, repo, _first, second) = fixture();
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    fs::write(root.path().join("ORIG_HEAD"), format!("{second}\n")).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
    git(
        root.path(),
        &["config", "gc.recentObjectsHook", "false"],
        b"",
    );
    let configured = Repository::open(root.path()).unwrap();
    let plan = configured.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn aggregate_reflog_budget_keeps_recovered_candidates() {
    let (_root, repo, _first, second) = fixture();
    let policy = RetentionPolicy {
        max_reflog_bytes: 0,
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&second));
}

#[test]
fn kept_pack_is_reported_for_later_repacking() {
    let (root, repo, _first, _second) = fixture();
    git(root.path(), &["repack", "-ad"], b"");
    let pack = fs::read_dir(root.path().join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .unwrap();
    fs::write(pack.with_extension("keep"), b"").unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(
        plan.protected_packs
            .contains(&fs::canonicalize(pack).unwrap())
    );
}

#[test]
fn alternate_objects_are_read_but_not_owned() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let borrowed = root.path().join("borrowed");
    git(
        root.path(),
        &["init", "--bare", "--template=", primary.to_str().unwrap()],
        b"",
    );
    git(
        root.path(),
        &["init", "--bare", "--template=", borrowed.to_str().unwrap()],
        b"",
    );
    let blob: ObjectId = git(&borrowed, &["hash-object", "-w", "--stdin"], b"alternate\n")
        .parse()
        .unwrap();
    fs::write(
        primary.join("objects/info/alternates"),
        format!("{}\n", borrowed.join("objects").display()),
    )
    .unwrap();
    let repo = Repository::open(&primary).unwrap();
    let policy = RetentionPolicy {
        heads: vec![blob],
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.required.contains(&blob));
    assert_eq!(
        plan.alternate_stores,
        [fs::canonicalize(borrowed.join("objects")).unwrap()]
    );
}

#[test]
fn invalid_worktree_registration_blocks_completion() {
    let (root, repo, _first, _second) = fixture();
    fs::create_dir_all(root.path().join("worktrees")).unwrap();
    fs::write(root.path().join("worktrees/bad"), b"not a registration").unwrap();
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn unavailable_checkout_keeps_private_head() {
    let (root, repo, _first, second) = fixture();
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    let checkout = root.path().join("unavailable");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &second.to_string(),
        ],
        b"",
    );
    fs::remove_file(checkout.join(".git")).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
}

#[test]
fn declared_shallow_boundary_does_not_require_missing_parent() {
    let (root, _repo, first, second) = fixture();
    git(root.path(), &["update-ref", "-d", "refs/heads/main"], b"");
    fs::write(root.path().join("shallow"), format!("{second}\n")).unwrap();
    let hex = first.to_string();
    fs::remove_file(root.path().join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.reachable.contains(&second));
    assert!(!plan.reachable.contains(&first));
}
