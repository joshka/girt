//! Original Git CLI fixtures for reftable repository/backend interoperability.
use std::path::Path;
use std::process::Command;

use girt::refs::{Backend, Expected, RefEdit, RefName, Reflog, Target};
use girt::{ObjectFormat, ObjectId, Repository, Signature};
use rstest::rstest;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    git_with_date(root, args, None)
}
fn git_with_date(root: &Path, args: &[&str], date: Option<&str>) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "a@b")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "a@b")
        .envs(date.map(|value| ("GIT_COMMITTER_DATE", value)))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn fixture(format: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--ref-format=reftable",
            "--initial-branch=main",
            &format!("--object-format={format}"),
        ],
    );
    git(root.path(), &["commit", "--allow-empty", "-m", "first"]);
    root
}
fn name(bytes: &[u8]) -> RefName {
    RefName::new(bytes).unwrap()
}
fn tip(root: &Path, format: ObjectFormat) -> ObjectId {
    let bytes = git(root, &["rev-parse", "HEAD"]);
    ObjectId::from_hex(format, String::from_utf8(bytes).unwrap().trim()).unwrap()
}
fn log() -> Reflog {
    Reflog::Append {
        committer: Signature {
            name: b"Girt".to_vec(),
            email: b"girt@example.invalid".to_vec(),
            seconds: 1234567890,
            offset_minutes: -480,
        },
        message: b"original publication".to_vec(),
    }
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn reads_nonempty_repository_and_publishes_conditional_refs(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.reference_backend(), Backend::Reftable);
    let refs = repo.references().unwrap();
    let id = tip(root.path(), format);
    assert_eq!(refs.resolve_with(&name(b"HEAD"), 32).unwrap().id, Some(id));
    assert_eq!(refs.list().unwrap().len(), 1);
    refs.transaction(&[RefEdit::new(
        name(b"refs/tags/published"),
        Some(Target::Direct(id)),
        Expected::Absent,
        log(),
    )])
    .unwrap();
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/tags/published"]),
        format!("{id}\n").as_bytes()
    );
    assert_eq!(
        git(
            root.path(),
            &["reflog", "show", "--format=%gn %gs", "refs/tags/published"]
        ),
        b"Girt original publication\n"
    );
    assert_eq!(
        refs.reflog(&name(b"refs/tags/published")).unwrap().unwrap()[0].new,
        id
    );
    git(root.path(), &["refs", "verify"]);
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn conditional_mismatch_preserves_refs_and_logs(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let before = std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let refs = repo.references().unwrap();
    let id = tip(root.path(), format);
    assert!(
        refs.update_without_reflog(
            &name(b"refs/heads/main"),
            Target::Direct(id),
            Expected::Absent
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
    assert!(!repo.git_dir().join("reftable/tables.list.lock").exists());
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn resolved_head_logging_and_log_deletion(#[case] spelling: &str, #[case] format: ObjectFormat) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let refs = repo.references().unwrap();
    let id = tip(root.path(), format);
    refs.transaction(&[{
        let input = RefEdit::new(
            name(b"HEAD"),
            Some(Target::Direct(id)),
            Expected::Value(Target::Direct(id)),
            log(),
        );
        input.with_dereference(girt::refs::Dereference::Follow)
    }])
    .unwrap();
    assert_eq!(
        refs.reflog(&name(b"HEAD"))
            .unwrap()
            .unwrap()
            .last()
            .unwrap()
            .message,
        b"original publication"
    );
    assert_eq!(
        refs.reflog(&name(b"refs/heads/main"))
            .unwrap()
            .unwrap()
            .last()
            .unwrap()
            .message,
        b"original publication"
    );
    refs.transaction(&[RefEdit::new(
        name(b"refs/heads/main"),
        None,
        Expected::Exists,
        Reflog::Delete,
    )])
    .unwrap();
    assert!(refs.read(&name(b"refs/heads/main")).unwrap().is_none());
    assert!(refs.reflog(&name(b"refs/heads/main")).unwrap().is_none());
    git(root.path(), &["reflog", "exists", "HEAD"]);
    git(root.path(), &["refs", "verify"]);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn initializes_git_usable_reftable_repository(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("new"),
        girt::InitKind::Worktree,
        Backend::Reftable,
    )
    .unwrap();
    assert_eq!(
        git(repo.worktree().unwrap(), &["symbolic-ref", "HEAD"]),
        b"refs/heads/main\n"
    );
    git(
        repo.worktree().unwrap(),
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Git after girt initialization",
        ],
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .resolve_with(&name(b"HEAD"), 32)
            .unwrap()
            .id,
        Some(tip(repo.worktree().unwrap(), format))
    );
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn linked_worktree_routes_head_and_branch_logs(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let linked = root.path().join("linked");
    git(
        root.path(),
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let repo = Repository::open(&linked).unwrap();
    let id = tip(&linked, format);
    let original_head = git(&linked, &["rev-parse", "ORIG_HEAD"]);
    let refs = repo.references().unwrap();
    let before = git(root.path(), &["reflog", "show", "--format=%gs", "HEAD"]);
    refs.transaction(&[{
        let input = RefEdit::new(
            name(b"HEAD"),
            Some(Target::Direct(id)),
            Expected::Exists,
            log(),
        );
        input.with_dereference(girt::refs::Dereference::Follow)
    }])
    .unwrap();
    assert_eq!(
        git(root.path(), &["reflog", "show", "--format=%gs", "HEAD"]),
        before
    );
    assert!(
        git(&linked, &["reflog", "show", "--format=%gs", "HEAD"])
            .starts_with(b"original publication\n")
    );
    assert!(
        git(
            &linked,
            &["reflog", "show", "--format=%gs", "refs/heads/linked"]
        )
        .starts_with(b"original publication\n")
    );
    assert_eq!(refs.resolve_with(&name(b"HEAD"), 32).unwrap().id, Some(id));
    girt::refs::reftable::compact(
        &repo.git_dir().join("reftable"),
        format,
        girt::refs::reftable::StackLimits::default(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(git(&linked, &["rev-parse", "ORIG_HEAD"]), original_head);
    git(&linked, &["refs", "verify"]);
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn colocation_detaches_head_after_index_publication(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), format);
    let edit = repo
        .edit_colocation(girt::index::Limits::default())
        .unwrap();
    edit.commit(Target::Direct(id), Expected::Exists, log())
        .unwrap();
    assert_eq!(
        repo.references().unwrap().read(&name(b"HEAD")).unwrap(),
        Some(Target::Direct(id))
    );
    assert_eq!(
        git(root.path(), &["rev-parse", "HEAD"]),
        format!("{id}\n").as_bytes()
    );
    git(root.path(), &["refs", "verify"]);
}

#[test]
fn resolves_branch_conditional_config_from_real_reftable_head() {
    let root = fixture("sha1");
    std::fs::write(
        root.path().join(".git/branch-config"),
        b"[test]\nvalue = actual-branch\n",
    )
    .unwrap();
    git(
        root.path(),
        &["config", "includeIf.onbranch:main.path", "branch-config"],
    );
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.config().value("test", None, "value"),
        Some(Some(b"actual-branch".as_slice()))
    );
}

#[test]
fn cancellation_preserves_stack_before_transaction() {
    let root = fixture("sha1");
    let repo = Repository::open(root.path()).unwrap();
    let before = std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let edit = RefEdit::new(name(b"HEAD"), None, Expected::Any, Reflog::Preserve);
    assert!(matches!(
        repo.references()
            .unwrap()
            .transaction_controlled(&[edit], &std::sync::atomic::AtomicBool::new(true)),
        Err(girt::refs::TransactionError::Prepare {
            source: girt::refs::ReferenceError::Cancelled,
            ..
        })
    ));
    assert_eq!(
        std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
}

#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn refuses_stale_expected_value_after_git_publication(
    #[case] spelling: &str,
    #[case] format: ObjectFormat,
) {
    let root = fixture(spelling);
    let old = tip(root.path(), format);
    let repo = Repository::open(root.path()).unwrap();
    git(root.path(), &["commit", "--allow-empty", "-m", "new tip"]);
    let current = tip(root.path(), format);
    let refs = repo.references().unwrap();
    assert!(matches!(
        refs.update_without_reflog(
            &name(b"refs/heads/main"),
            Target::Direct(old),
            Expected::Value(Target::Direct(old))
        ),
        Err(girt::refs::ReferenceError::Mismatch { .. })
    ));
    assert_eq!(
        refs.resolve_with(&name(b"HEAD"), 32).unwrap().id,
        Some(current)
    );
}

#[rstest]
#[case::any(Expected::Any, true)]
#[case::exists(Expected::Exists, true)]
#[case::absent(Expected::Absent, false)]
#[case::same(Expected::Value(Target::Direct(ObjectId::Sha1([1; 20]))), true)]
#[case::different(Expected::Value(Target::Direct(ObjectId::Sha1([3; 20]))), false)]
#[case::absent_or_same(Expected::AbsentOr(Target::Direct(ObjectId::Sha1([1; 20]))), true)]
#[case::absent_or_different(Expected::AbsentOr(Target::Direct(ObjectId::Sha1([3; 20]))), false)]
fn preserves_expected_value_predicates(#[case] expected: Expected, #[case] succeeds: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        girt::InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let refs = repo.references().unwrap();
    let name = name(b"refs/tags/condition");
    refs.update_without_reflog(
        &name,
        Target::Direct(ObjectId::Sha1([1; 20])),
        Expected::Absent,
    )
    .unwrap();
    assert_eq!(
        refs.update_without_reflog(&name, Target::Direct(ObjectId::Sha1([2; 20])), expected)
            .is_ok(),
        succeeds
    );
}

#[test]
fn rejected_result_budget_leaves_the_stack_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        girt::InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let before = std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap();
    let refs = repo
        .references()
        .unwrap()
        .with_reftable_limits(girt::refs::reftable::StackLimits::default().with_tables(1));
    assert!(matches!(
        refs.update_without_reflog(
            &name(b"refs/tags/new"),
            Target::Direct(ObjectId::Sha1([1; 20])),
            Expected::Absent
        ),
        Err(girt::refs::ReferenceError::Reftable(
            girt::refs::reftable::Error::Limit(_)
        ))
    ));
    assert_eq!(
        std::fs::read(repo.git_dir().join("reftable/tables.list")).unwrap(),
        before
    );
}

#[rstest]
#[case::utc("+0000", "+0000", 0)]
#[case::west("-0700", "-0700", -420)]
#[case::east_half_hour("+0530", "+0530", 330)]
#[case::west_half_hour("-0330", "-0330", -210)]
#[case::east_quarter_hour("+0015", "+0015", 15)]
#[case::west_quarter_hour("-0015", "-0015", -15)]
#[case::positive_boundary("+2359", "+2359", 1439)]
#[case::negative_boundary("-2359", "-2359", -1439)]
#[case::positive_noncanonical_input("+0060", "+0100", 60)]
#[case::negative_noncanonical_input("-0060", "-0100", -60)]
fn reflog_timezones_interoperate_in_both_directions(
    #[values("sha1", "sha256")] spelling: &str,
    #[case] input_offset: &str,
    #[case] offset: &str,
    #[case] minutes: i16,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), repo.object_format());
    git_with_date(
        root.path(),
        &[
            "update-ref",
            "--create-reflog",
            "-m",
            "timezone-oracle",
            "refs/heads/from-git",
            &id.to_string(),
        ],
        Some(&format!("@1700000123 {input_offset}")),
    );
    let refs = repo.references().unwrap();
    let observed = refs.reflog(&name(b"refs/heads/from-git")).unwrap().unwrap();
    assert_eq!(observed[0].committer.offset_minutes, minutes);
    assert_eq!(observed[0].committer.seconds, 1700000123);
    refs.transaction(&[RefEdit::new(
        name(b"refs/heads/from-girt"),
        Some(Target::Direct(id)),
        Expected::Absent,
        Reflog::Append {
            committer: Signature {
                name: b"Girt".to_vec(),
                email: b"girt@example.invalid".to_vec(),
                seconds: 1700000123,
                offset_minutes: minutes,
            },
            message: b"timezone-oracle".to_vec(),
        },
    )])
    .unwrap();
    assert_eq!(
        git(
            root.path(),
            &[
                "reflog",
                "show",
                "--format=%gD",
                "--date=raw",
                "refs/heads/from-girt"
            ]
        ),
        format!("refs/heads/from-girt@{{1700000123 {offset}}}\n").as_bytes(),
    );
}

fn table_names(directory: &Path) -> Vec<String> {
    std::fs::read_to_string(directory.join("tables.list"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Whether `name` has the form Git writes: `0x{min:012x}-0x{max:012x}-{suffix:08x}.ref`.
fn is_git_table_name(name: &str) -> bool {
    let hex = |part: &str, len: usize| {
        part.len() == len && part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    let Some(stem) = name.strip_suffix(".ref") else {
        return false;
    };
    let parts: Vec<_> = stem.split('-').collect();
    matches!(
        parts.as_slice(),
        [min, max, suffix]
            if min.strip_prefix("0x").is_some_and(|min| hex(min, 12))
                && max.strip_prefix("0x").is_some_and(|max| hex(max, 12))
                && hex(suffix, 8)
    )
}

fn empty_commit(root: &Path, format: ObjectFormat) -> ObjectId {
    let tree = git(root, &["write-tree"]);
    let tree = String::from_utf8(tree).unwrap();
    let commit = git(root, &["commit-tree", tree.trim(), "-m", "girt tables"]);
    ObjectId::from_hex(format, String::from_utf8(commit).unwrap().trim()).unwrap()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn strict_fsck_accepts_girt_table_names(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("new"),
        girt::InitKind::Worktree,
        Backend::Reftable,
    )
    .unwrap();
    let worktree = repo.worktree().unwrap().to_owned();
    let directory = repo.git_dir().join("reftable");
    let id = empty_commit(&worktree, format);
    let refs = repo.references().unwrap();
    for branch in [&b"refs/heads/main"[..], b"refs/heads/other"] {
        refs.transaction(&[RefEdit::new(
            name(branch),
            Some(Target::Direct(id)),
            Expected::Absent,
            log(),
        )])
        .unwrap();
    }
    let linked = repo
        .create_orphan_worktree(root.path().join("linked"), &name(b"refs/heads/linked"), 4)
        .unwrap();
    let linked_names = table_names(&linked.git_dir().join("reftable"));
    assert_eq!(linked_names.len(), 1);
    assert!(is_git_table_name(&linked_names[0]), "{linked_names:?}");
    let names = table_names(&directory);
    assert_eq!(names.len(), 3);
    assert!(
        names.iter().all(|name| is_git_table_name(name)),
        "{names:?}"
    );
    assert!(names[0].starts_with("0x000000000001-0x000000000001-"));
    git(&worktree, &["fsck", "--strict", "--no-dangling"]);

    girt::refs::reftable::compact(
        &directory,
        format,
        girt::refs::reftable::StackLimits::default(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let names = table_names(&directory);
    assert_eq!(names.len(), 1);
    assert!(names[0].starts_with("0x000000000001-0x000000000003-"));
    assert!(is_git_table_name(&names[0]), "{names:?}");
    git(&worktree, &["fsck", "--strict", "--no-dangling"]);
}

/// Earlier girt versions named the initial table `initial.ref` and later tables with tempfile's
/// alphanumeric suffixes. Git reads those stacks, and girt keeps reading and extending them.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn reads_and_extends_tables_with_earlier_girt_names(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("new"),
        girt::InitKind::Worktree,
        Backend::Reftable,
    )
    .unwrap();
    let worktree = repo.worktree().unwrap().to_owned();
    let directory = repo.git_dir().join("reftable");
    let id = empty_commit(&worktree, format);
    repo.references()
        .unwrap()
        .transaction(&[RefEdit::new(
            name(b"refs/heads/main"),
            Some(Target::Direct(id)),
            Expected::Absent,
            log(),
        )])
        .unwrap();
    let legacy = ["initial.ref", "0x000000000002-0x000000000002-5TMi7y.ref"];
    for (current, earlier) in table_names(&directory).iter().zip(legacy) {
        std::fs::rename(directory.join(current), directory.join(earlier)).unwrap();
    }
    std::fs::write(directory.join("tables.list"), legacy.join("\n") + "\n").unwrap();

    let repo = Repository::open(&worktree).unwrap();
    let refs = repo.references().unwrap();
    assert_eq!(refs.resolve_with(&name(b"HEAD"), 32).unwrap().id, Some(id));
    assert_eq!(
        refs.reflog(&name(b"refs/heads/main")).unwrap().unwrap()[0].new,
        id
    );
    refs.transaction(&[RefEdit::new(
        name(b"refs/tags/extended"),
        Some(Target::Direct(id)),
        Expected::Absent,
        log(),
    )])
    .unwrap();
    let names = table_names(&directory);
    assert_eq!(names[..2], legacy);
    assert!(is_git_table_name(&names[2]), "{names:?}");
    assert_eq!(
        git(&worktree, &["rev-parse", "refs/tags/extended"]),
        format!("{id}\n").as_bytes()
    );

    girt::refs::reftable::compact(
        &directory,
        format,
        girt::refs::reftable::StackLimits::default(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert!(!directory.join(legacy[0]).exists());
    git(&worktree, &["fsck", "--strict", "--no-dangling"]);
}

/// Reftable retains distinct case spellings even on case-insensitive filesystems.
#[rstest]
#[case::sha1("sha1", ObjectFormat::Sha1)]
#[case::sha256("sha256", ObjectFormat::Sha256)]
fn case_distinct_reference_names_match_git(#[case] spelling: &str, #[case] format: ObjectFormat) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), format);
    let refs = repo.references().unwrap();
    for reference in [b"refs/tags/Audit".as_slice(), b"refs/tags/audit".as_slice()] {
        refs.transaction(&[RefEdit::new(
            name(reference),
            Some(Target::Direct(id)),
            Expected::Absent,
            log(),
        )])
        .unwrap();
    }
    for reference in ["refs/tags/Audit", "refs/tags/audit"] {
        assert_eq!(
            git(root.path(), &["rev-parse", "--verify", reference]),
            format!("{id}\n").as_bytes()
        );
        assert_eq!(
            refs.resolve_with(&name(reference.as_bytes()), 32)
                .unwrap()
                .id,
            Some(id)
        );
    }
    git(root.path(), &["refs", "verify"]);
}

#[rstest]
#[case::child_of_existing("refs/tags/a/b", "refs/tags/a/b/c", true)]
#[case::grandchild_of_existing("refs/tags/a", "refs/tags/a/b/c", true)]
#[case::parent_of_existing("refs/tags/a/b/c", "refs/tags/a/b", true)]
#[case::grandparent_of_existing("refs/tags/a/b/c", "refs/tags/a", true)]
#[case::byte_prefix_sibling_existing("refs/tags/a/b-c", "refs/tags/a/b", false)]
#[case::byte_prefix_sibling_new("refs/tags/a/b", "refs/tags/a/b-c", false)]
#[case::dot_sibling_existing("refs/tags/a.b/c", "refs/tags/a", false)]
#[case::digit_sibling_existing("refs/tags/a0", "refs/tags/a", false)]
#[case::shared_directory("refs/tags/a/b", "refs/tags/a/c", false)]
fn namespace_conflicts_follow_path_prefixes(
    #[values("sha1", "sha256")] spelling: &str,
    #[values(false, true)] controlled: bool,
    #[values(false, true)] in_batch: bool,
    #[case] existing: &str,
    #[case] new: &str,
    #[case] conflict: bool,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), repo.object_format());
    if !in_batch {
        git(root.path(), &["update-ref", existing, &id.to_string()]);
    }
    // Neighbours ensure the range probe must distinguish a byte prefix from a path prefix.
    git(
        root.path(),
        &["update-ref", "refs/tags/a00", &id.to_string()],
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/z", &id.to_string()],
    );
    let before = git(
        root.path(),
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let refs = repo.references().unwrap();
    let mut edits = Vec::new();
    if in_batch {
        edits.push(RefEdit::create(
            name(existing.as_bytes()),
            id,
            Reflog::Preserve,
        ));
    }
    edits.push(RefEdit::create(name(new.as_bytes()), id, Reflog::Preserve));
    let result = if controlled {
        refs.transaction_controlled(&edits, &std::sync::atomic::AtomicBool::new(false))
    } else {
        refs.transaction(&edits)
    };
    if conflict {
        assert!(
            matches!(
                result,
                Err(girt::refs::TransactionError::Prepare {
                    operation: Some(_),
                    source: girt::refs::ReferenceError::Conflict(_),
                })
            ),
            "{result:?}"
        );
        assert_eq!(
            git(
                root.path(),
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            before
        );
    } else {
        result.unwrap();
        assert_eq!(
            refs.read(&name(new.as_bytes())).unwrap(),
            Some(Target::Direct(id))
        );
    }
    let listing: String = refs
        .list()
        .unwrap()
        .into_iter()
        .map(|entry| {
            let Target::Direct(id) = entry.target else {
                panic!("unexpected symbolic ref")
            };
            format!("{} {id}\n", String::from_utf8_lossy(entry.name.as_bytes()))
        })
        .collect();
    assert_eq!(
        listing.as_bytes(),
        git(
            root.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        )
    );
}

#[rstest]
fn deleting_child_does_not_allow_parent_in_same_transaction(
    #[values("sha1", "sha256")] spelling: &str,
    #[values(false, true)] controlled: bool,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), repo.object_format());
    git(
        root.path(),
        &["update-ref", "refs/tags/a/b", &id.to_string()],
    );
    let before = git(root.path(), &["for-each-ref"]);
    let edits = [
        RefEdit::delete(name(b"refs/tags/a/b"), id, Reflog::Preserve),
        RefEdit::create(name(b"refs/tags/a"), id, Reflog::Preserve),
    ];
    let refs = repo.references().unwrap();
    let result = if controlled {
        refs.transaction_controlled(&edits, &std::sync::atomic::AtomicBool::new(false))
    } else {
        refs.transaction(&edits)
    };
    assert!(
        matches!(
            result,
            Err(girt::refs::TransactionError::Prepare {
                operation: Some(1),
                source: girt::refs::ReferenceError::Conflict(_),
            })
        ),
        "{result:?}"
    );
    assert_eq!(git(root.path(), &["for-each-ref"]), before);
}

#[rstest]
fn symbolic_reflog_dependency_reserves_its_namespace(
    #[values("sha1", "sha256")] spelling: &str,
    #[values(false, true)] controlled: bool,
) {
    let root = fixture(spelling);
    let repo = Repository::open(root.path()).unwrap();
    let id = tip(root.path(), repo.object_format());
    let before = git(root.path(), &["for-each-ref"]);
    // The unborn target enters `used` through reflog resolution, without entering `values`.
    let edits = [
        RefEdit::new(
            name(b"refs/tags/alias"),
            Some(Target::Symbolic(name(b"refs/tags/a/b"))),
            Expected::Absent,
            log(),
        ),
        RefEdit::create(name(b"refs/tags/a"), id, Reflog::Preserve),
    ];
    let refs = repo.references().unwrap();
    let result = if controlled {
        refs.transaction_controlled(&edits, &std::sync::atomic::AtomicBool::new(false))
    } else {
        refs.transaction(&edits)
    };
    assert!(
        matches!(
            result,
            Err(girt::refs::TransactionError::Prepare {
                operation: Some(1),
                source: girt::refs::ReferenceError::Conflict(_),
            })
        ),
        "{result:?}"
    );
    assert_eq!(git(root.path(), &["for-each-ref"]), before);
}
