use super::*;
use crate::submodule::{Commands, Modules};

struct Fixture {
    _directory: tempfile::TempDir,
    parent: Repository,
    child: Repository,
    plan: UpdatePlan,
    options: UpdateOptions,
    second: String,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("parent");
        std::fs::create_dir(&root).unwrap();
        let git = |root: &std::path::Path, args: &[&str]| super::oracle::git(root, args, b"");
        git(&root, &["init", "-q", "-b", "main"]);
        let path = root.join("lib");
        std::fs::create_dir(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        std::fs::write(path.join("a"), b"first\n").unwrap();
        git(&path, &["add", "a"]);
        git(&path, &["commit", "-qm", "first"]);
        let first = String::from_utf8(git(&path, &["rev-parse", "HEAD"])).unwrap();
        git(
            &root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},lib", first.trim()),
            ],
        );
        git(&root, &["config", "submodule.math.url", "unused"]);
        std::fs::write(path.join("a"), b"second\n").unwrap();
        git(&path, &["commit", "-qam", "second"]);
        let second = String::from_utf8(git(&path, &["rev-parse", "HEAD"])).unwrap();
        let parent = Repository::open(&root).unwrap();
        let child = Repository::open(&path).unwrap();
        let modules = Modules::parse(b"[submodule \"math\"]\npath = lib\nurl = unused\n").unwrap();
        let recorded = parent
            .read_index(Default::default())
            .unwrap()
            .unwrap()
            .entries()[0]
            .id;
        let plan = modules
            .get(b"math")
            .unwrap()
            .plan_update(parent.config(), recorded, Commands::Refuse)
            .unwrap();
        let options = UpdateOptions::new(Signature {
            name: b"C".to_vec(),
            email: b"c@example.com".to_vec(),
            seconds: 1_700_000_123,
            offset_minutes: 0,
        });
        Self {
            _directory: directory,
            parent,
            child,
            plan,
            options,
            second,
        }
    }
    fn assert_checkout_and_branch(&self, report: &checkout::Report) {
        assert!(report.index_published);
        assert_eq!(
            std::fs::read(self.child.worktree().unwrap().join("a")).unwrap(),
            b"first\n"
        );
        let branch = self
            .child
            .references()
            .unwrap()
            .read(&crate::refs::RefName::branch("main").unwrap())
            .unwrap();
        assert_eq!(
            branch,
            Some(crate::refs::Target::Direct(
                self.second.trim().parse().unwrap()
            ))
        );
    }
}

#[test]
fn cancellation_after_checkout_preserves_completed_effects_and_old_head() {
    let fixture = Fixture::new();
    let cancel = AtomicBool::new(false);
    let error = fixture
        .plan
        .checkout_with(&fixture.parent, &fixture.options, &cancel, || {
            cancel.store(true, Ordering::Relaxed)
        })
        .unwrap_err();
    let UpdateError::AfterCheckout { checkout, source } = error else {
        panic!("expected completed checkout");
    };
    assert!(matches!(*source, UpdateError::Cancelled));
    fixture.assert_checkout_and_branch(&checkout);
    assert_eq!(
        fixture.child.head().unwrap().id.unwrap().to_string(),
        fixture.second.trim()
    );
}

#[test]
fn parent_index_change_after_checkout_is_reported_without_head_publication() {
    let fixture = Fixture::new();
    let error = fixture
        .plan
        .checkout_with(
            &fixture.parent,
            &fixture.options,
            &AtomicBool::new(false),
            || {
                super::oracle::git(
                    fixture.parent.worktree().unwrap(),
                    &[
                        "update-index",
                        "--cacheinfo",
                        &format!("160000,{},lib", fixture.second.trim()),
                    ],
                    b"",
                );
            },
        )
        .unwrap_err();
    let UpdateError::AfterCheckout { checkout, source } = error else {
        panic!("expected completed checkout");
    };
    assert!(matches!(*source, UpdateError::Changed));
    fixture.assert_checkout_and_branch(&checkout);
    assert_eq!(
        fixture.child.head().unwrap().id.unwrap().to_string(),
        fixture.second.trim()
    );
}

#[test]
fn head_publication_failure_keeps_checkout_and_transaction_error() {
    let fixture = Fixture::new();
    let error = fixture
        .plan
        .checkout_with(
            &fixture.parent,
            &fixture.options,
            &AtomicBool::new(false),
            || {
                // A deterministic storage fault after checkout, outside the cooperating-writer
                // contract.
                let head = fixture.child.git_dir().join("HEAD");
                std::fs::remove_file(&head).unwrap();
                std::fs::create_dir(&head).unwrap();
            },
        )
        .unwrap_err();
    let UpdateError::AfterCheckout { checkout, source } = error else {
        panic!("expected completed checkout");
    };
    let UpdateError::Prepare(source) = *source else {
        panic!("expected publication cause");
    };
    assert!(source.is::<crate::refs::TransactionError>());
    fixture.assert_checkout_and_branch(&checkout);
}

#[test]
fn stored_head_append_locks_the_old_symbolic_branch_through_checkout() {
    let fixture = Fixture::new();
    let report = fixture
        .plan
        .checkout_with(
            &fixture.parent,
            &fixture.options,
            &AtomicBool::new(false),
            || {
                assert!(fixture.child.git_dir().join("HEAD.lock").exists());
                assert!(
                    fixture
                        .child
                        .git_dir()
                        .join("refs/heads/main.lock")
                        .exists()
                );
                let output = super::oracle::attempt(
                    fixture.child.worktree().unwrap(),
                    &[
                        "update-ref",
                        "refs/heads/main",
                        &fixture.plan.recorded().to_string(),
                    ],
                    b"",
                );
                assert!(!output.status.success());
            },
        )
        .unwrap();
    fixture.assert_checkout_and_branch(report.checkout.as_ref().unwrap());
    assert!(report.head_updated);
}
