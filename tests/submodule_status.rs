//! Original local-file fixtures; Git CLI observations are the only behavioral oracle.
#![cfg(any(target_os = "linux", target_os = "macos"))]

#[path = "support/layout_git.rs"]
mod oracle;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use girt::filter::Commands;
use girt::status::{Baseline, Limits, UntrackedFiles};
use girt::{Repository, submodule};

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut complete = vec!["-c", "protocol.file.allow=always"];
    complete.extend_from_slice(args);
    oracle::git(root, &complete, b"")
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    sub: PathBuf,
}

impl Fixture {
    fn new(format: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("parent");
        let child = directory.path().join("child");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&child).unwrap();
        let format = format!("--object-format={format}");
        git(&child, &["init", "-q", &format]);
        std::fs::write(child.join("a.txt"), b"hello\n").unwrap();
        git(&child, &["add", "a.txt"]);
        git(&child, &["commit", "-qm", "initial"]);
        git(&root, &["init", "-q", &format]);
        let url = format!("file://{}", child.display());
        git(
            &root,
            &["submodule", "add", "--name", "library", &url, "vendor/lib"],
        );
        git(&root, &["commit", "-qam", "submodule"]);
        let sub = root.join("vendor/lib");
        Self {
            _directory: directory,
            root,
            sub,
        }
    }

    fn report(&self) -> girt::status::Report {
        let repo = Repository::open(&self.root).unwrap();
        let rules = repo.ignore_rules(&Default::default()).unwrap();
        let mut filters = repo.filters(&Default::default(), Commands::Refuse).unwrap();
        repo.status_with_filters(
            Baseline::Head,
            UntrackedFiles::All,
            &rules,
            &girt::pathspec::Pathspec::all(),
            &mut filters,
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn checkout(&self) -> submodule::Checkout {
        submodule::status(&Repository::open(&self.root).unwrap())
            .unwrap()
            .remove(0)
    }

    fn git_prefix(&self) -> (char, String) {
        let bytes = git(&self.root, &["submodule", "status"]);
        let text = String::from_utf8(bytes).unwrap();
        let id = text[1..].split(' ').next().unwrap().to_owned();
        (text.chars().next().unwrap(), id)
    }

    fn git_dirty(&self) -> bool {
        let bytes = git(&self.root, &["status", "--porcelain=v2"]);
        bytes
            .split(|b| *b == b'\n')
            .any(|line| line.ends_with(b" vendor/lib"))
    }
}

#[test]
fn listing_and_parent_dirtiness_match_git_in_both_formats() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new(format);
        let checkout = f.checkout();
        assert_eq!(checkout.name.as_deref(), Some(b"library".as_slice()));
        assert_eq!(
            (checkout.prefix(), checkout.display_id().to_string()),
            f.git_prefix()
        );
        assert!(!checkout.is_modified());
        let child = Repository::open(&f.sub).unwrap();
        assert!(child.git_dir().ends_with(".git/modules/library"));
        assert_eq!(child.worktree().unwrap(), f.sub.canonicalize().unwrap());
        assert!(
            std::fs::read(f.sub.join(".git"))
                .unwrap()
                .starts_with(b"gitdir: ")
        );

        std::fs::write(f.sub.join("scratch"), b"untracked").unwrap();
        let report = f.report();
        assert!(report.unchecked_gitlinks.is_empty());
        assert_eq!(report.unstaged[0].path, b"vendor/lib");
        assert!(!report.submodules[0].tracked_dirty);
        assert!(report.submodules[0].untracked_dirty);
        assert_eq!(report.submodules[0].prefix(), ' ');
        assert!(f.git_dirty());
        std::fs::remove_file(f.sub.join("scratch")).unwrap();
        std::fs::write(f.sub.join("a.txt"), b"changed\n").unwrap();
        let checkout = f.checkout();
        assert!(checkout.tracked_dirty && !checkout.untracked_dirty);
        assert!(checkout.is_modified() && f.git_dirty());
        git(&f.sub, &["add", "a.txt"]);
        assert!(f.checkout().tracked_dirty);
        git(&f.sub, &["commit", "-qm", "next"]);
        let checkout = f.checkout();
        assert!(!checkout.tracked_dirty);
        assert_eq!(
            (checkout.prefix(), checkout.display_id().to_string()),
            f.git_prefix()
        );
        assert_eq!(checkout.prefix(), '+');
        assert!(checkout.is_modified() && f.git_dirty());
        git(&f.root, &["config", "submodule.library.active", "false"]);
        let checkout = f.checkout();
        assert_eq!(
            (checkout.prefix(), checkout.display_id().to_string()),
            f.git_prefix()
        );
        assert_eq!(checkout.prefix(), '-');
        assert!(checkout.is_modified());
    }
}

#[test]
fn missing_and_conflicted_checkouts_match_git_prefixes() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new(format);
        let recorded = f.checkout().recorded;
        git(&f.root, &["submodule", "deinit", "-f", "--", "vendor/lib"]);
        let checkout = f.checkout();
        assert_eq!(
            (checkout.prefix(), checkout.display_id().to_string()),
            f.git_prefix()
        );
        assert!(!checkout.is_modified() && !f.git_dirty());
        git(&f.root, &["update-index", "--force-remove", "vendor/lib"]);
        let entries = format!("160000 {recorded} 2\tvendor/lib\n160000 {recorded} 3\tvendor/lib\n");
        oracle::git(
            &f.root,
            &["update-index", "--index-info"],
            entries.as_bytes(),
        );
        let checkout = f.checkout();
        assert!(checkout.conflicted);
        assert_eq!(
            (checkout.prefix(), checkout.display_id().to_string()),
            f.git_prefix()
        );
        let report = f.report();
        assert_eq!(report.unmerged.len(), 2);
        assert!(report.unstaged.is_empty());
    }
}

#[test]
fn ignore_precedence_and_visibility_match_git() {
    let f = Fixture::new("sha1");
    std::fs::write(f.sub.join("a.txt"), b"changed\n").unwrap();
    std::fs::write(f.sub.join("scratch"), b"untracked").unwrap();
    for (diff, module, local, tracked, untracked) in [
        ("all", None, None, false, false),
        ("all", Some("none"), None, true, true),
        ("none", Some("all"), Some("none"), true, true),
        ("none", Some("none"), Some("all"), false, false),
        ("none", None, Some("dirty"), false, false),
        ("none", None, Some("untracked"), true, false),
    ] {
        git(&f.root, &["config", "diff.ignoreSubmodules", diff]);
        let _ = oracle::attempt(
            &f.root,
            &["config", "--unset", "submodule.library.ignore"],
            b"",
        );
        let _ = oracle::attempt(
            &f.root,
            &[
                "config",
                "-f",
                ".gitmodules",
                "--unset",
                "submodule.library.ignore",
            ],
            b"",
        );
        if let Some(value) = module {
            git(
                &f.root,
                &[
                    "config",
                    "-f",
                    ".gitmodules",
                    "submodule.library.ignore",
                    value,
                ],
            );
        }
        if let Some(value) = local {
            git(&f.root, &["config", "submodule.library.ignore", value]);
        }
        let report = f.report();
        let checkout = f.checkout();
        assert_eq!(
            (checkout.tracked_dirty, checkout.untracked_dirty),
            (tracked, untracked),
            "{diff} {module:?} {local:?}"
        );
        assert_eq!(checkout.is_modified(), f.git_dirty());
        assert_eq!(
            report.unstaged.iter().any(|c| c.path == b"vendor/lib"),
            f.git_dirty()
        );
    }
}

#[test]
fn inspection_is_bounded_and_pathspec_avoids_unselected_gitlinks() {
    let f = Fixture::new("sha1");
    let repo = Repository::open(&f.root).unwrap();
    for limits in [
        Limits::default().with_max_submodules(0),
        Limits::default().with_max_submodule_depth(0),
    ] {
        assert!(matches!(
            submodule::status_with(&repo, Commands::Refuse, limits, &AtomicBool::new(false)),
            Err(submodule::StatusError::Limit(_))
        ));
    }
    assert!(matches!(
        submodule::status_with(
            &repo,
            Commands::Run,
            Limits::default(),
            &AtomicBool::new(true)
        ),
        Err(submodule::StatusError::Cancelled)
    ));
    let paths = girt::pathspec::Pathspec::parse([".gitmodules"], &Default::default()).unwrap();
    let rules = repo.ignore_rules(&Default::default()).unwrap();
    let report = repo
        .status_with(
            Baseline::Head,
            UntrackedFiles::All,
            &rules,
            &paths,
            Limits::default().with_max_submodules(0),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.submodules.is_empty());
    let raw = repo
        .raw_status(Baseline::Head, girt::status::Untracked::Omit)
        .unwrap();
    assert_eq!(raw.unchecked_gitlinks.len(), 1);
    assert!(raw.submodules.is_empty());
}

fn global_git(root: &Path, global: &Path, args: &[&str]) -> Vec<u8> {
    global_git_env(root, global, args, &[])
}

fn global_git_env(
    root: &Path,
    global: &Path,
    args: &[&str],
    extra: &[(&str, &std::ffi::OsStr)],
) -> Vec<u8> {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let result = command
        .current_dir(root)
        .env("HOME", root.join("absent-home"))
        .env("XDG_CONFIG_HOME", root.join("absent-xdg"))
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_AUTHOR_NAME", "A. Writer")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0530")
        .env("GIT_COMMITTER_NAME", "C. Recorder")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000123 -0700")
        .envs(extra.iter().copied())
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn configured_report(
    root: &Path,
    inputs: &girt::config::ConfigInputs,
    commands: Commands,
) -> Result<girt::status::Report, girt::status::Error> {
    let repo = Repository::open_with_config(root, inputs).unwrap();
    let rules = repo.ignore_rules(&Default::default()).unwrap();
    let mut filters = repo.filters(&Default::default(), commands).unwrap();
    repo.status_with_filters(
        Baseline::Head,
        UntrackedFiles::All,
        &rules,
        &girt::pathspec::Pathspec::all(),
        &mut filters,
        Limits::default(),
        &AtomicBool::new(false),
    )
}

#[test]
fn nonlocal_config_is_inherited_without_parent_local_filter_settings() {
    use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
    for format in ["sha1", "sha256"] {
        let f = Fixture::new(format);
        std::fs::write(f.sub.join(".gitattributes"), b"a.txt filter=lower\n").unwrap();
        git(&f.sub, &["add", ".gitattributes"]);
        git(&f.sub, &["commit", "-qm", "attributes"]);
        git(&f.root, &["add", "vendor/lib"]);
        git(&f.root, &["commit", "-qm", "record attributes"]);
        let global = f.root.parent().unwrap().join("global-config");
        std::fs::write(
            &global,
            b"[filter \"lower\"]\nclean = cat\nrequired = true\n[includeIf \"gitdir:**/modules/library\"]\npath = child-config\n",
        )
        .unwrap();
        std::fs::write(
            f.root.parent().unwrap().join("child-config"),
            b"[filter \"lower\"]\nclean = tr '[:upper:]' '[:lower:]'\n",
        )
        .unwrap();
        // These parent-local values must never enter the child's effective config.
        git(&f.root, &["config", "core.autocrlf", "false"]);
        git(&f.root, &["config", "filter.lower.clean", "cat"]);
        // Establish same-size checkout statistics before changing content through the filter.
        global_git(&f.sub, &global, &["checkout-index", "-f", "-u", "--all"]);
        std::fs::write(f.sub.join("a.txt"), b"HELLO\n").unwrap();
        let inputs = ConfigInputs::default()
            .with_files(vec![ConfigFile::new(global.clone(), ConfigScope::Global)]);
        let observed = global_git(&f.root, &global, &["status", "--porcelain=v2"]);
        assert!(
            observed.is_empty(),
            "parent={} child={} config={}",
            String::from_utf8_lossy(&observed),
            String::from_utf8_lossy(&global_git(&f.sub, &global, &["status", "--porcelain=v2"])),
            String::from_utf8_lossy(&global_git(
                &f.sub,
                &global,
                &["config", "--list", "--show-origin"]
            ))
        );
        let report = configured_report(&f.root, &inputs, Commands::Run).unwrap();
        assert!(report.unstaged.is_empty());
        assert!(!report.submodules[0].is_modified());
        assert!(matches!(
            configured_report(&f.root, &inputs, Commands::Refuse),
            Err(girt::status::Error::Submodules(_))
        ));
        // A tighter caller-selected output limit also applies to child filter commands.
        let limited_repo = Repository::open_with_config(&f.root, &inputs).unwrap();
        let rules = limited_repo.ignore_rules(&Default::default()).unwrap();
        let attributes = girt::attributes::Attributes::new(
            girt::attributes::Case::Sensitive,
            Default::default(),
        );
        let drivers = girt::filter::Drivers::default()
            .with_commands(Commands::Run)
            .with_limits(girt::filter::Limits::default().with_max_output_bytes(1));
        let mut filters =
            girt::filter::Pipeline::from_attributes(attributes, Default::default(), drivers);
        assert!(matches!(
            limited_repo.status_with_filters(
                Baseline::Head,
                UntrackedFiles::All,
                &rules,
                &girt::pathspec::Pathspec::all(),
                &mut filters,
                Limits::default(),
                &AtomicBool::new(false)
            ),
            Err(girt::status::Error::Submodules(_))
        ));
        // Child-local settings override the inherited global layer, as Git does.
        git(&f.sub, &["config", "filter.lower.clean", "cat"]);
        assert!(!global_git(&f.root, &global, &["status", "--porcelain=v2"]).is_empty());
        assert!(
            configured_report(&f.root, &inputs, Commands::Run)
                .unwrap()
                .submodules[0]
                .tracked_dirty
        );
        // Explicit command/environment layers survive recursion and override child-local config.
        let runtime =
            girt::Config::parse(b"[filter \"lower\"]\nclean = tr '[:upper:]' '[:lower:]'\n")
                .unwrap();
        for inherited in [
            inputs.clone().with_command(Some(runtime.clone())),
            inputs.clone().with_environment(Some(runtime.clone())),
        ] {
            assert!(
                configured_report(&f.root, &inherited, Commands::Run)
                    .unwrap()
                    .unstaged
                    .is_empty()
            );
        }
        assert!(
            global_git(
                &f.root,
                &global,
                &[
                    "-c",
                    "filter.lower.clean=tr '[:upper:]' '[:lower:]'",
                    "status",
                    "--porcelain=v2"
                ]
            )
            .is_empty()
        );
    }
}

#[test]
fn nested_untracked_content_keeps_its_kind_and_budgets_are_shared() {
    let f = Fixture::new("sha1");
    let grand = f.root.parent().unwrap().join("grand");
    std::fs::create_dir(&grand).unwrap();
    git(&grand, &["init", "-q"]);
    std::fs::write(grand.join("g"), b"grandchild\n").unwrap();
    git(&grand, &["add", "g"]);
    git(&grand, &["commit", "-qm", "grandchild"]);
    let url = format!("file://{}", grand.display());
    git(
        &f.sub,
        &["submodule", "add", "--name", "grandchild", &url, "nested"],
    );
    git(&f.sub, &["commit", "-qam", "nested submodule"]);
    git(&f.root, &["add", "vendor/lib"]);
    git(&f.root, &["commit", "-qm", "record nested submodule"]);
    std::fs::write(f.sub.join("nested/scratch"), b"untracked").unwrap();
    let checkout = f.checkout();
    assert!(!checkout.tracked_dirty && checkout.untracked_dirty);
    let git_status = git(&f.root, &["status", "--porcelain=v2"]);
    assert!(String::from_utf8_lossy(&git_status).contains("S..U"));
    let repo = Repository::open(&f.root).unwrap();
    assert!(matches!(
        submodule::status_with(
            &repo,
            Commands::Refuse,
            Limits::default().with_max_submodules(1),
            &AtomicBool::new(false)
        ),
        Err(submodule::StatusError::AtPath { .. })
    ));
    git(
        &f.root,
        &["config", "submodule.library.ignore", "untracked"],
    );
    assert!(!f.checkout().is_modified() && !f.git_dirty());
}

#[test]
fn symlink_ancestors_are_refused_before_opening_nested_metadata() {
    let f = Fixture::new("sha1");
    let moved = f.root.parent().unwrap().join("moved");
    std::fs::rename(f.root.join("vendor"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, f.root.join("vendor")).unwrap();
    assert!(submodule::status(&Repository::open(&f.root).unwrap()).is_err());
}

#[test]
fn staged_gitlink_changes_under_ignore_all_match_git() {
    let f = Fixture::new("sha1");
    std::fs::write(f.sub.join("a.txt"), b"next\n").unwrap();
    git(&f.sub, &["commit", "-qam", "next"]);
    git(&f.root, &["add", "vendor/lib"]);
    git(&f.root, &["config", "submodule.library.ignore", "all"]);
    let report = f.report();
    assert_eq!(
        report
            .staged
            .iter()
            .any(|change| change.path == b"vendor/lib"),
        f.git_dirty()
    );
}

#[test]
fn broken_populated_metadata_is_not_reported_as_uninitialized() {
    let f = Fixture::new("sha1");
    std::fs::write(
        f.sub.join(".git"),
        b"gitdir: /nonexistent-girt-submodule-directory\n",
    )
    .unwrap();
    assert!(submodule::status(&Repository::open(&f.root).unwrap()).is_err());
}

#[test]
fn ignore_all_does_not_open_the_nested_repository_in_parent_status() {
    let f = Fixture::new("sha1");
    git(&f.root, &["config", "submodule.library.ignore", "all"]);
    std::fs::write(f.sub.join(".git"), b"not a git file\n").unwrap();
    let report = f.report();
    assert!(report.submodules.is_empty());
    assert_eq!(report.unchecked_gitlinks.len(), 1);
    assert!(report.unstaged.is_empty());
    assert!(!f.git_dirty());
    assert!(submodule::status(&Repository::open(&f.root).unwrap()).is_err());
}

#[test]
fn captured_xdg_attributes_and_ignores_reach_nested_status() {
    use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
    let f = Fixture::new("sha1");
    let xdg = f.root.parent().unwrap().join("custom-xdg");
    std::fs::create_dir_all(xdg.join("git")).unwrap();
    std::fs::write(xdg.join("git/attributes"), b"*.txt filter=lower\n").unwrap();
    std::fs::write(xdg.join("git/ignore"), b"*.scratch\n").unwrap();
    let global = f.root.parent().unwrap().join("global");
    std::fs::write(
        &global,
        b"[filter \"lower\"]\nclean = tr '[:upper:]' '[:lower:]'\nrequired = true\n",
    )
    .unwrap();
    let environment = girt::workflow::Environment::default()
        .with("XDG_CONFIG_HOME", xdg.as_os_str())
        .with("GIT_ATTR_NOSYSTEM", "1");
    let inputs = ConfigInputs::default()
        .with_files(vec![ConfigFile::new(global.clone(), ConfigScope::Global)]);
    std::fs::write(f.sub.join("a.txt"), b"HELLO\n").unwrap();
    std::fs::write(f.sub.join("ignored.scratch"), b"ignored").unwrap();
    assert!(
        global_git_env(
            &f.root,
            &global,
            &["status", "--porcelain=v2"],
            &[("XDG_CONFIG_HOME", xdg.as_os_str())]
        )
        .is_empty()
    );
    let repo = Repository::open_with_config(&f.root, &inputs).unwrap();
    let rules = repo.ignore_rules(&environment).unwrap();
    let mut filters = repo.filters(&environment, Commands::Run).unwrap();
    let report = repo
        .status_with_filters(
            Baseline::Head,
            UntrackedFiles::All,
            &rules,
            &girt::pathspec::Pathspec::all(),
            &mut filters,
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(!report.submodules[0].is_modified());
    assert!(report.unstaged.is_empty());
    // The unfiltered entry point retains the ignore loader's environment too.
    std::fs::write(f.sub.join("a.txt"), b"hello\n").unwrap();
    let repo = Repository::open(&f.root).unwrap();
    let rules = repo.ignore_rules(&environment).unwrap();
    assert!(
        !repo
            .status(Baseline::Head, UntrackedFiles::All, &rules)
            .unwrap()
            .submodules[0]
            .is_modified()
    );
}

#[test]
fn regular_file_replacing_a_gitlink_is_a_filtered_type_change() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new(format);
        std::fs::rename(&f.sub, f.root.parent().unwrap().join("saved-checkout")).unwrap();
        std::fs::write(f.root.join(".gitattributes"), b"vendor/lib text eol=lf\n").unwrap();
        std::fs::write(&f.sub, b"replacement\r\n").unwrap();
        let report = f.report();
        let change = report
            .unstaged
            .iter()
            .find(|change| change.path == b"vendor/lib")
            .unwrap();
        assert_eq!(change.status(), Some(girt::ChangeStatus::TypeChanged));
        let girt::status::Change::Modified(value) = change.change else {
            panic!("expected type change");
        };
        let repo = Repository::open(&f.root).unwrap();
        assert_eq!(
            value.id,
            girt::ObjectId::for_blob(repo.object_format(), b"replacement\n")
        );
        assert!(
            String::from_utf8_lossy(&git(&f.root, &["status", "--porcelain=v2"]))
                .contains(".T S...")
        );
        assert_eq!(f.checkout().prefix(), '-');
        git(&f.root, &["config", "submodule.library.ignore", "all"]);
        assert!(
            !String::from_utf8_lossy(&git(&f.root, &["status", "--porcelain=v2"]))
                .contains("vendor/lib")
        );
        assert!(
            !f.report()
                .unstaged
                .iter()
                .any(|change| change.path == b"vendor/lib")
        );
    }
}

#[test]
fn ignored_gitlinks_stay_unchecked_in_index_order_without_spending_inspection_budget() {
    let f = Fixture::new("sha1");
    let child = f.root.parent().unwrap().join("child");
    let url = format!("file://{}", child.display());
    git(
        &f.root,
        &["submodule", "add", "--name", "other", &url, "vendor/other"],
    );
    git(&f.root, &["commit", "-qam", "second submodule"]);
    git(&f.root, &["config", "submodule.library.ignore", "all"]);
    std::fs::write(f.root.join("vendor/other/a.txt"), b"changed\n").unwrap();
    let repo = Repository::open(&f.root).unwrap();
    let raw = repo
        .raw_status(Baseline::Head, girt::status::Untracked::Omit)
        .unwrap();
    let rules = repo.ignore_rules(&Default::default()).unwrap();
    let report = repo
        .status_with(
            Baseline::Head,
            UntrackedFiles::All,
            &rules,
            &girt::pathspec::Pathspec::all(),
            Limits::default().with_max_submodules(1),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        report.unchecked_gitlinks,
        vec![raw.unchecked_gitlinks[0].clone()]
    );
    assert_eq!(report.submodules.len(), 1);
    assert_eq!(report.submodules[0].path, b"vendor/other");
}

#[test]
fn unborn_child_is_an_explicit_known_difference() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new(format);
        std::fs::rename(&f.sub, f.root.parent().unwrap().join("saved-checkout")).unwrap();
        std::fs::create_dir(&f.sub).unwrap();
        git(
            &f.sub,
            &["init", "-q", &format!("--object-format={format}")],
        );
        assert!(!f.git_dirty());
        assert_eq!(f.git_prefix().0, ' ');
        assert!(matches!(
            submodule::status(&Repository::open(&f.root).unwrap()),
            Err(submodule::StatusError::Configuration {
                reason: "populated submodule HEAD is unborn",
                ..
            })
        ));
    }
}

#[test]
fn symlink_replacements_are_refused_as_git_refuses_them() {
    let f = Fixture::new("sha1");
    let moved = f.root.parent().unwrap().join("saved-checkout");
    std::fs::rename(&f.sub, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &f.sub).unwrap();
    for args in [
        vec!["status", "--porcelain=v2"],
        vec!["submodule", "status"],
    ] {
        let output = oracle::attempt(&f.root, &args, b"");
        assert_eq!(output.status.code(), Some(128));
    }
    assert!(submodule::status(&Repository::open(&f.root).unwrap()).is_err());
}
