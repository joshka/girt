//! Population/layout contracts from isolated Git executable fixtures.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/layout_git.rs"]
mod oracle;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use std::path::{Path, PathBuf};

    use girt::submodule::{self, Commands, Modules, Source, UpdateOptions};
    use girt::workflow::{Environment, NoCallbacks};
    use girt::{Repository, Signature};

    use crate::oracle;

    fn git(path: &Path, args: &[&str]) -> String {
        let mut all = vec!["-c", "protocol.file.allow=always"];
        all.extend_from_slice(args);
        String::from_utf8(oracle::git(path, &all, b""))
            .unwrap()
            .trim()
            .to_owned()
    }
    struct Fixture {
        _directory: tempfile::TempDir,
        child: PathBuf,
        parent: PathBuf,
        comparison: PathBuf,
    }
    impl Fixture {
        fn new(format: &str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let child = root.join("child");
            let source = root.join("source");
            let parent = root.join("parent");
            let comparison = root.join("comparison");
            for path in [&child, &source] {
                std::fs::create_dir(path).unwrap();
                git(
                    path,
                    &[
                        "init",
                        "-q",
                        "-b",
                        "main",
                        &format!("--object-format={format}"),
                    ],
                );
            }
            std::fs::write(child.join("file"), b"one\n").unwrap();
            git(&child, &["add", "."]);
            git(&child, &["commit", "-qm", "one"]);
            git(
                &source,
                &[
                    "submodule",
                    "add",
                    "--name",
                    "math",
                    &format!("file://{}", child.display()),
                    "vendor/lib",
                ],
            );
            git(&source, &["commit", "-qam", "parent"]);
            for path in [&parent, &comparison] {
                git(
                    root,
                    &[
                        "clone",
                        "-q",
                        source.to_str().unwrap(),
                        path.to_str().unwrap(),
                    ],
                );
            }
            Self {
                _directory: directory,
                child,
                parent,
                comparison,
            }
        }
        fn populate(&self) -> Result<submodule::PopulateReport, submodule::PopulateError> {
            let parent = Repository::open(&self.parent).unwrap();
            let modules =
                Modules::read(&parent, &parent.objects().unwrap(), Source::WorktreeOrIndex)
                    .unwrap()
                    .unwrap();
            let definition = modules.get(b"math").unwrap();
            definition
                .plan_init(parent.config(), self.parent.to_str().unwrap().as_bytes())
                .unwrap()
                .apply(&parent)
                .unwrap();
            let parent = Repository::open(&self.parent).unwrap();
            let recorded = parent
                .read_index(Default::default())
                .unwrap()
                .unwrap()
                .entries()
                .iter()
                .find(|e| e.path == b"vendor/lib")
                .unwrap()
                .id;
            let plan = definition
                .plan_update(parent.config(), recorded, Commands::Refuse)
                .unwrap();
            let person = Signature {
                name: b"Writer".to_vec(),
                email: b"a@example.com".to_vec(),
                seconds: 1_700_000_000,
                offset_minutes: 0,
            };
            let options = UpdateOptions::new(person)
                .with_environment(Environment::default().with("GIT_ALLOW_PROTOCOL", "file"));
            plan.populate(&parent, &options, &mut NoCallbacks)
        }
    }

    #[test]
    fn absent_child_layout_and_detached_checkout_match_git_in_both_formats() {
        for format in ["sha1", "sha256"] {
            let fixture = Fixture::new(format);
            git(&fixture.comparison, &["submodule", "update", "--init"]);
            let report = fixture.populate().unwrap();
            assert!(report.cloned.is_some());
            assert!(report.gitfile_written);
            assert!(report.update.unwrap().head_updated);
            let child = fixture.parent.join("vendor/lib");
            assert_eq!(std::fs::read(child.join("file")).unwrap(), b"one\n");
            assert_eq!(
                git(&child, &["rev-parse", "HEAD"]),
                git(
                    &fixture.comparison.join("vendor/lib"),
                    &["rev-parse", "HEAD"]
                )
            );
            let repo = Repository::open(&child).unwrap();
            assert_eq!(
                repo.git_dir(),
                fixture
                    .parent
                    .join(".git/modules/math")
                    .canonicalize()
                    .unwrap()
            );
            assert!(repo.worktree().is_some());
            assert!(matches!(
                repo.references()
                    .unwrap()
                    .read(&girt::refs::RefName::HEAD)
                    .unwrap(),
                Some(girt::refs::Target::Direct(_))
            ));
            assert!(git(&fixture.parent, &["status", "--porcelain"]).is_empty());
            assert!(fixture.populate().unwrap().cloned.is_none());
        }
    }

    #[test]
    fn missing_commit_is_fetched_without_moving_the_child_branch() {
        let fixture = Fixture::new("sha1");
        fixture.populate().unwrap();
        let child = fixture.parent.join("vendor/lib");
        let branch = git(&child, &["rev-parse", "refs/heads/main"]);
        std::fs::write(fixture.child.join("file"), b"two\n").unwrap();
        git(&fixture.child, &["commit", "-qam", "two"]);
        let target = git(&fixture.child, &["rev-parse", "HEAD"]);
        git(
            &fixture.parent,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{target},vendor/lib"),
            ],
        );
        let report = fixture.populate().unwrap();
        assert!(report.fetched.is_some());
        assert_eq!(git(&child, &["rev-parse", "HEAD"]), target);
        assert_eq!(git(&child, &["rev-parse", "refs/heads/main"]), branch);
        assert_eq!(std::fs::read(child.join("file")).unwrap(), b"two\n");
    }

    #[test]
    fn nonempty_child_and_existing_modules_destination_are_never_replaced() {
        for modules_collision in [false, true] {
            let fixture = Fixture::new("sha1");
            let path = if modules_collision {
                fixture.parent.join(".git/modules/math")
            } else {
                fixture.parent.join("vendor/lib")
            };
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("sentinel"), b"keep").unwrap();
            assert!(fixture.populate().is_err());
            assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"keep");
            assert!(!fixture.parent.join("vendor/lib/.git").exists());
        }
    }
    #[test]
    fn symlinked_checkout_or_modules_storage_is_never_followed() {
        use std::os::unix::fs::symlink;
        for storage in [false, true] {
            let fixture = Fixture::new("sha1");
            let elsewhere = fixture.child.join("untouched");
            std::fs::create_dir(&elsewhere).unwrap();
            let selected = if storage {
                fixture.parent.join(".git/modules")
            } else {
                fixture.parent.join("vendor/lib")
            };
            if selected.exists() {
                std::fs::remove_dir(&selected).unwrap();
            }
            symlink(&elsewhere, &selected).unwrap();
            assert!(fixture.populate().is_err());
            assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
        }
    }

    #[test]
    fn fetched_objects_remain_reported_when_dirty_checkout_refuses() {
        let fixture = Fixture::new("sha1");
        fixture.populate().unwrap();
        std::fs::write(fixture.parent.join("vendor/lib/file"), b"local edit\n").unwrap();
        std::fs::write(fixture.child.join("file"), b"two\n").unwrap();
        git(&fixture.child, &["commit", "-qam", "two"]);
        let target = git(&fixture.child, &["rev-parse", "HEAD"]);
        git(
            &fixture.parent,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{target},vendor/lib"),
            ],
        );
        let error = fixture.populate().unwrap_err();
        assert!(error.report.fetched.is_some());
        assert!(matches!(
            error.source,
            submodule::PopulateFailure::Update(_)
        ));
        assert_eq!(
            std::fs::read(fixture.parent.join("vendor/lib/file")).unwrap(),
            b"local edit\n"
        );
    }

    #[test]
    fn initial_checkout_uses_target_attributes_and_retains_layout_on_failure() {
        for fail in [false, true] {
            let fixture = Fixture::new("sha1");
            let attributes = if fail {
                b"file filter=required\n".as_slice()
            } else {
                b"file text eol=crlf\n".as_slice()
            };
            std::fs::write(fixture.child.join(".gitattributes"), attributes).unwrap();
            git(&fixture.child, &["add", "."]);
            git(&fixture.child, &["commit", "-qm", "attributes"]);
            let target = git(&fixture.child, &["rev-parse", "HEAD"]);
            git(
                &fixture.parent,
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("160000,{target},vendor/lib"),
                ],
            );
            if fail {
                // Exhausting the commit-read budget after clone exercises retained layout phases.
                let parent = Repository::open(&fixture.parent).unwrap();
                let modules =
                    Modules::read(&parent, &parent.objects().unwrap(), Source::WorktreeOrIndex)
                        .unwrap()
                        .unwrap();
                let definition = modules.get(b"math").unwrap();
                definition
                    .plan_init(parent.config(), b"ignored")
                    .unwrap()
                    .apply(&parent)
                    .unwrap();
                let parent = Repository::open(&fixture.parent).unwrap();
                let plan = definition
                    .plan_update(parent.config(), target.parse().unwrap(), Commands::Refuse)
                    .unwrap();
                let mut limits = girt::checkout::Limits::default();
                limits.max_object_bytes = 1;
                let options = UpdateOptions::new(Signature {
                    name: b"Writer".to_vec(),
                    email: b"a@example.com".to_vec(),
                    seconds: 1_700_000_000,
                    offset_minutes: 0,
                })
                .with_environment(Environment::default().with("GIT_ALLOW_PROTOCOL", "file"))
                .with_limits(limits);
                let error = plan
                    .populate(&parent, &options, &mut NoCallbacks)
                    .unwrap_err();
                assert!(error.report.cloned.is_some());
                assert!(error.report.worktree_configured);
                assert!(error.report.gitfile_written);
                assert!(fixture.parent.join("vendor/lib/.git").is_file());
            } else {
                fixture.populate().unwrap();
                assert_eq!(
                    std::fs::read(fixture.parent.join("vendor/lib/file")).unwrap(),
                    b"one\r\n"
                );
            }
        }
    }
    #[test]
    fn shallow_recommendations_are_typed_refusals_before_clone() {
        for configured in [false, true] {
            let fixture = Fixture::new("sha1");
            if configured {
                git(
                    &fixture.parent,
                    &["config", "submodule.math.shallow", "true"],
                );
            } else {
                git(
                    &fixture.parent,
                    &[
                        "config",
                        "-f",
                        ".gitmodules",
                        "submodule.math.shallow",
                        "true",
                    ],
                );
            }
            let error = fixture.populate().unwrap_err();
            assert!(matches!(error.source, submodule::PopulateFailure::Shallow));
            assert!(error.report.cloned.is_none());
            assert!(!fixture.parent.join(".git/modules").exists());
        }
    }

    #[test]
    fn unadvertised_missing_target_is_never_replaced_with_the_remote_tip() {
        let fixture = Fixture::new("sha1");
        fixture.populate().unwrap();
        let old = git(&fixture.parent.join("vendor/lib"), &["rev-parse", "HEAD"]);
        git(
            &fixture.parent,
            &[
                "update-index",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,vendor/lib",
            ],
        );
        let error = fixture.populate().unwrap_err();
        assert!(matches!(
            error.source,
            submodule::PopulateFailure::MissingCommit(_)
        ));
        assert!(error.report.fetched.is_some());
        assert_eq!(
            git(&fixture.parent.join("vendor/lib"), &["rev-parse", "HEAD"]),
            old
        );
    }
}

#[test]
fn none_never_starts_transport_and_unsupported_platform_is_explicit() {
    use girt::{InitKind, ObjectFormat, Repository, Signature, submodule, workflow};
    let directory = tempfile::tempdir().unwrap();
    let parent = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("parent"),
        InitKind::Worktree,
    )
    .unwrap();
    let modules = submodule::Modules::parse(
        b"[submodule \"math\"]\npath=lib\nurl=https://invalid.example/math\nupdate=none\n",
    )
    .unwrap();
    let definition = modules.get(b"math").unwrap();
    definition
        .plan_init(parent.config(), b"unused")
        .unwrap()
        .apply(&parent)
        .unwrap();
    let parent = Repository::open(parent.worktree().unwrap()).unwrap();
    let plan = definition
        .plan_update(
            parent.config(),
            "1111111111111111111111111111111111111111".parse().unwrap(),
            submodule::Commands::Refuse,
        )
        .unwrap();
    let options = submodule::UpdateOptions::new(Signature {
        name: b"Writer".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    });
    assert!(
        plan.populate(&parent, &options, &mut workflow::NoCallbacks)
            .unwrap()
            .update
            .is_some()
    );
    assert!(!parent.common_dir().join("modules").exists());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let mut edit = parent.edit_config(1024 * 1024).unwrap();
        edit.document_mut()
            .append("submodule", Some(b"math"), "update", b"checkout")
            .unwrap();
        edit.commit().unwrap();
        let parent = Repository::open(parent.worktree().unwrap()).unwrap();
        let plan = definition
            .plan_update(
                parent.config(),
                plan.recorded(),
                submodule::Commands::Refuse,
            )
            .unwrap();
        assert!(matches!(
            plan.populate(&parent, &options, &mut workflow::NoCallbacks)
                .unwrap_err()
                .source,
            submodule::PopulateFailure::Update(submodule::UpdateError::UnsupportedPlatform)
        ));
    }
}
