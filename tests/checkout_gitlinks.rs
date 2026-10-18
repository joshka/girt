//! Initial checkout preserves external commit links without entering child repositories.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/layout_git.rs"]
mod oracle;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use std::path::Path;

    use girt::config::ConfigInputs;
    use girt::{Repository, Signature, submodule, workflow};

    use crate::oracle;
    fn git(root: &Path, args: &[&str]) -> Vec<u8> {
        let mut all = vec!["-c", "protocol.file.allow=always"];
        all.extend_from_slice(args);
        oracle::git(root, &all, b"")
    }
    #[test]
    fn ordinary_clone_and_new_child_keep_gitlinks_as_empty_directories() {
        for format in ["sha1", "sha256"] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            let leaf = root.join("leaf");
            let middle = root.join("middle");
            let top = root.join("top");
            for path in [&leaf, &middle, &top] {
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
                git(path, &["add", "."]);
                git(path, &["commit", "-qm", "initial"]);
            }
            for (parent, child) in [(&middle, &leaf), (&top, &middle)] {
                git(
                    parent,
                    &[
                        "submodule",
                        "add",
                        "--name",
                        "math",
                        &format!("file://{}", child.display()),
                        "vendor/lib",
                    ],
                );
                git(parent, &["commit", "-qam", "submodule"]);
            }
            let environment = workflow::Environment::default()
                .with_current_dir(root)
                .with("GIT_ALLOW_PROTOCOL", "file");
            let outcome = workflow::clone_with(
                &format!("file://{}", middle.display()),
                root.join("copy"),
                &workflow::CloneOptions::default(),
                &ConfigInputs::default(),
                &environment,
                &mut workflow::NoCallbacks,
            )
            .unwrap();
            assert!(outcome.checkout.unwrap().index_published);
            git(
                root,
                &[
                    "clone",
                    "-q",
                    &format!("file://{}", middle.display()),
                    root.join("comparison").to_str().unwrap(),
                ],
            );
            assert_eq!(
                git(&root.join("copy"), &["ls-files", "--stage"]),
                git(&root.join("comparison"), &["ls-files", "--stage"])
            );
            assert!(
                std::fs::read_dir(root.join("copy/vendor/lib"))
                    .unwrap()
                    .next()
                    .is_none()
            );
            assert!(git(&root.join("copy"), &["status", "--porcelain"]).is_empty());

            git(
                root,
                &[
                    "clone",
                    "-q",
                    top.to_str().unwrap(),
                    root.join("parent").to_str().unwrap(),
                ],
            );
            let parent = Repository::open(root.join("parent")).unwrap();
            let modules = submodule::Modules::read(
                &parent,
                &parent.objects().unwrap(),
                submodule::Source::Worktree,
            )
            .unwrap()
            .unwrap();
            let definition = modules.get(b"math").unwrap();
            definition
                .plan_init(parent.config(), top.to_str().unwrap().as_bytes())
                .unwrap()
                .apply(&parent)
                .unwrap();
            let parent = Repository::open(root.join("parent")).unwrap();
            let index = parent.read_index(Default::default()).unwrap().unwrap();
            let recorded = index
                .entries()
                .iter()
                .find(|e| e.path == b"vendor/lib")
                .unwrap()
                .id;
            let plan = definition
                .plan_update(parent.config(), recorded, submodule::Commands::Refuse)
                .unwrap();
            let person = Signature {
                name: b"Writer".to_vec(),
                email: b"a@example.com".to_vec(),
                seconds: 1_700_000_000,
                offset_minutes: 0,
            };
            plan.populate(
                &parent,
                &submodule::UpdateOptions::new(person).with_environment(environment),
                &mut workflow::NoCallbacks,
            )
            .unwrap();
            let child = root.join("parent/vendor/lib");
            assert!(
                std::fs::read_dir(child.join("vendor/lib"))
                    .unwrap()
                    .next()
                    .is_none()
            );
            assert_eq!(
                git(&child, &["ls-files", "--stage"]),
                git(&root.join("comparison"), &["ls-files", "--stage"])
            );
            assert!(git(&root.join("parent"), &["status", "--porcelain"]).is_empty());
        }
    }
}
