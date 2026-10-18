//! Recursive clone contracts from original isolated Git CLI fixtures.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/layout_git.rs"]
mod oracle;

fn options() -> girt::submodule::RecursionOptions {
    let person = girt::Signature {
        name: b"Writer".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let update = girt::submodule::UpdateOptions::new(person).with_environment(
        girt::workflow::Environment::default().with("GIT_ALLOW_PROTOCOL", "file"),
    );
    girt::submodule::RecursionOptions::new(update)
}

#[test]
fn recursion_requires_a_checked_out_superproject_before_transport() {
    use girt::config::ConfigInputs;
    use girt::workflow;
    let directory = tempfile::tempdir().unwrap();
    let options = workflow::CloneOptions::default()
        .with_checkout(false)
        .with_submodules(Some(options()));
    let error = workflow::clone_with(
        "https://invalid.example/parent",
        directory.path().join("copy"),
        &options,
        &ConfigInputs::default(),
        &workflow::Environment::default(),
        &mut workflow::NoCallbacks,
    )
    .unwrap_err();
    assert!(matches!(error, workflow::CloneError::SubmoduleLayout));
    assert!(!directory.path().join("copy").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use std::path::{Path, PathBuf};

    use girt::config::ConfigInputs;
    use girt::{submodule, workflow};

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
        directory: tempfile::TempDir,
        parent: PathBuf,
    }
    impl Fixture {
        fn new(format: &str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let leaf = root.join("leaf");
            let child = root.join("child");
            let parent = root.join("parent");
            for path in [&leaf, &child, &parent] {
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
                std::fs::write(path.join("file"), b"content\n").unwrap();
                if path == &leaf {
                    std::fs::write(path.join(".gitattributes"), b"file text eol=crlf\n").unwrap();
                }
                git(path, &["add", "."]);
                git(path, &["commit", "-qm", "initial"]);
            }
            for (repo, url, name, path) in [
                (&child, &leaf, "inner", "nested/leaf"),
                (&parent, &child, "outer", "vendor/lib"),
            ] {
                git(
                    repo,
                    &[
                        "submodule",
                        "add",
                        "--name",
                        name,
                        &format!("file://{}", url.display()),
                        path,
                    ],
                );
                let relative = format!("../{}", url.file_name().unwrap().to_str().unwrap());
                git(
                    repo,
                    &[
                        "config",
                        "-f",
                        ".gitmodules",
                        &format!("submodule.{name}.url"),
                        &relative,
                    ],
                );
                git(repo, &["add", "."]);
                git(repo, &["commit", "-qm", "nested"]);
            }
            Self { directory, parent }
        }
        fn clone(
            &self,
            options: submodule::RecursionOptions,
        ) -> Result<workflow::CloneOutcome, workflow::CloneError> {
            self.clone_with_inputs(options, &ConfigInputs::default())
        }
        fn clone_with_inputs(
            &self,
            options: submodule::RecursionOptions,
            inputs: &ConfigInputs,
        ) -> Result<workflow::CloneOutcome, workflow::CloneError> {
            let root = self.directory.path();
            workflow::clone_with(
                &format!("file://{}", self.parent.display()),
                root.join("copy"),
                &workflow::CloneOptions::default().with_submodules(Some(options)),
                inputs,
                &workflow::Environment::default()
                    .with_current_dir(root)
                    .with("GIT_ALLOW_PROTOCOL", "file"),
                &mut workflow::NoCallbacks,
            )
        }
    }
    #[test]
    fn recursive_clone_layout_and_commits_match_git_in_both_formats() {
        for format in ["sha1", "sha256"] {
            let fixture = Fixture::new(format);
            let root = fixture.directory.path();
            git(
                root,
                &[
                    "clone",
                    "-q",
                    "--recurse-submodules",
                    &format!("file://{}", fixture.parent.display()),
                    root.join("comparison").to_str().unwrap(),
                ],
            );
            let outcome = fixture.clone(super::options()).unwrap();
            let report = outcome.submodules.unwrap();
            assert_eq!(report.entries.len(), 2);
            for path in ["vendor/lib", "vendor/lib/nested/leaf"] {
                assert_eq!(
                    git(&root.join("copy").join(path), &["rev-parse", "HEAD"]),
                    git(&root.join("comparison").join(path), &["rev-parse", "HEAD"])
                );
                assert_eq!(
                    std::fs::read(root.join("copy").join(path).join("file")).unwrap(),
                    std::fs::read(root.join("comparison").join(path).join("file")).unwrap()
                );
            }
            let leaf = girt::Repository::open(root.join("copy/vendor/lib/nested/leaf")).unwrap();
            assert_eq!(
                leaf.git_dir(),
                root.join("copy/.git/modules/outer/modules/inner")
                    .canonicalize()
                    .unwrap()
            );
            assert!(git(&root.join("copy"), &["status", "--porcelain"]).is_empty());
        }
    }
    #[test]
    fn depth_and_count_limits_keep_completed_superproject_and_child_effects() {
        for depth in [true, false] {
            let fixture = Fixture::new("sha1");
            let mut options = super::options();
            if depth {
                options.max_depth = 1;
            } else {
                options.max_submodules = 1;
            }
            let error = fixture.clone(options).unwrap_err();
            let workflow::CloneError::Submodules { report, source } = error else {
                panic!("unexpected failure")
            };
            assert!(report.checkout.is_some());
            assert_eq!(source.report.entries.len(), 1);
            assert!(matches!(
                source.source,
                submodule::RecursiveFailure::Limit(_)
            ));
            assert!(
                fixture
                    .directory
                    .path()
                    .join("copy/vendor/lib/.git")
                    .is_file()
            );
            assert!(
                !fixture
                    .directory
                    .path()
                    .join("copy/vendor/lib/nested/leaf/.git")
                    .exists()
            );
        }
    }
    #[test]
    fn none_update_skips_checkout_and_descent_as_git_does() {
        let fixture = Fixture::new("sha1");
        git(
            &fixture.parent,
            &[
                "config",
                "-f",
                ".gitmodules",
                "submodule.outer.update",
                "none",
            ],
        );
        git(&fixture.parent, &["commit", "-qam", "none"]);
        git(
            fixture.directory.path(),
            &[
                "clone",
                "-q",
                "--recurse-submodules",
                &format!("file://{}", fixture.parent.display()),
                fixture
                    .directory
                    .path()
                    .join("comparison")
                    .to_str()
                    .unwrap(),
            ],
        );
        let outcome = fixture.clone(super::options()).unwrap();
        assert_eq!(outcome.submodules.unwrap().entries.len(), 1);
        assert!(
            !fixture
                .directory
                .path()
                .join("comparison/vendor/lib/.git")
                .exists()
        );
        assert!(
            !fixture
                .directory
                .path()
                .join("copy/vendor/lib/.git")
                .exists()
        );
    }
    #[test]
    fn explicit_activation_is_honored_without_hidden_clone_config() {
        for config in [
            b"[submodule \"outer\"]\nactive=false\n".as_slice(),
            b"[submodule]\nactive=elsewhere\n".as_slice(),
        ] {
            let fixture = Fixture::new("sha1");
            let inputs =
                ConfigInputs::default().with_command(Some(girt::Config::parse(config).unwrap()));
            let outcome = fixture
                .clone_with_inputs(super::options(), &inputs)
                .unwrap();
            assert!(outcome.submodules.unwrap().entries.is_empty());
            assert!(
                !fixture
                    .directory
                    .path()
                    .join("copy/vendor/lib/.git")
                    .exists()
            );
            let local = std::fs::read(fixture.directory.path().join("copy/.git/config")).unwrap();
            assert!(!String::from_utf8_lossy(&local).contains("[submodule"));
        }
    }

    #[test]
    fn nested_failure_retains_completed_child_and_the_failing_path() {
        let fixture = Fixture::new("sha1");
        // The outer URL is absolute and valid; nested relative URLs still resolve from it.
        // Deny the inner child through a captured protocol allowlist that only permits file,
        // by making its metadata request HTTPS.
        let child = fixture.directory.path().join("child");
        git(
            &child,
            &[
                "config",
                "-f",
                ".gitmodules",
                "submodule.inner.url",
                "https://invalid.example/leaf",
            ],
        );
        git(&child, &["commit", "-qam", "inner URL"]);
        let id = git(&child, &["rev-parse", "HEAD"]);
        git(
            &fixture.parent,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{},vendor/lib", id.trim()),
            ],
        );
        git(&fixture.parent, &["commit", "-qm", "record child"]);
        let error = fixture.clone(super::options()).unwrap_err();
        let workflow::CloneError::Submodules { report, source } = error else {
            panic!("unexpected error")
        };
        assert!(report.checkout.is_some());
        assert_eq!(source.path, b"vendor/lib/nested/leaf");
        assert_eq!(source.report.entries.len(), 2);
        assert!(source.report.entries[0].population.is_some());
        assert!(source.report.entries[1].initialized);
        assert!(matches!(
            source.source,
            submodule::RecursiveFailure::Populate(_)
        ));
        assert!(
            fixture
                .directory
                .path()
                .join("copy/vendor/lib/.git")
                .is_file()
        );
        assert!(
            !fixture
                .directory
                .path()
                .join("copy/vendor/lib/nested/leaf/.git")
                .exists()
        );
    }
}
