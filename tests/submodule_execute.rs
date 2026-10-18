//! Writing contracts from original isolated Git CLI fixtures.
#[path = "support/layout_git.rs"]
mod oracle;

use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use girt::{Repository, Signature, submodule};

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut options = vec!["-c", "protocol.file.allow=always"];
    options.extend_from_slice(args);
    oracle::git(root, &options, b"")
}
fn person() -> Signature {
    Signature {
        name: b"C. Recorder".to_vec(),
        email: b"committer@example.com".to_vec(),
        seconds: 1_700_000_123,
        offset_minutes: -420,
    }
}
fn definition() -> submodule::Modules {
    submodule::Modules::parse(
        b"[submodule \"math\"]\npath = lib\nurl = ../child.git\nupdate = checkout\n",
    )
    .unwrap()
}

#[test]
fn init_publication_preserves_unrelated_bytes_and_refuses_stale_config_or_locks() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    let config_path = root.join(".git/config");
    let mut original = std::fs::read(&config_path).unwrap();
    original.extend_from_slice(b"# unrelated comment\n[user]\nname = Literal Name\n");
    std::fs::write(&config_path, &original).unwrap();
    let modules = definition();
    let repo = Repository::open(root).unwrap();
    let plan = modules
        .get(b"math")
        .unwrap()
        .plan_init(repo.config(), b"https://example.com/team/parent.git")
        .unwrap();
    std::fs::write(root.join(".git/config.lock"), b"owned elsewhere").unwrap();
    assert!(plan.apply(&repo).is_err());
    assert_eq!(std::fs::read(&config_path).unwrap(), original);
    std::fs::remove_file(root.join(".git/config.lock")).unwrap();
    git(root, &["config", "submodule.math.url", "changed"]);
    let changed = std::fs::read(&config_path).unwrap();
    assert!(matches!(
        plan.apply(&repo),
        Err(submodule::InitError::Changed)
    ));
    assert_eq!(std::fs::read(&config_path).unwrap(), changed);
    assert!(!root.join(".git/config.lock").exists());

    std::fs::write(&config_path, &original).unwrap();
    assert!(plan.apply(&repo).unwrap());
    assert!(std::fs::read(&config_path).unwrap().starts_with(&original));
    assert!(
        repo.config()
            .string("submodule", Some(b"math"), "url")
            .is_none()
    );
    let repo = Repository::open(root).unwrap();
    assert_eq!(
        repo.config().string("submodule", Some(b"math"), "url"),
        Some(b"https://example.com/team/child.git".as_slice())
    );
    let plan = modules
        .get(b"math")
        .unwrap()
        .plan_init(repo.config(), b"ignored")
        .unwrap();
    let bytes = std::fs::read(&config_path).unwrap();
    assert!(!plan.apply(&repo).unwrap());
    assert_eq!(std::fs::read(&config_path).unwrap(), bytes);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    child: PathBuf,
    first: String,
    second: String,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Fixture {
    fn new(format: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("parent");
        let upstream = directory.path().join("upstream");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&upstream).unwrap();
        git(
            &upstream,
            &["init", "-q", &format!("--object-format={format}")],
        );
        std::fs::write(upstream.join("a.txt"), b"first\n").unwrap();
        std::fs::write(
            upstream.join(".gitattributes"),
            b"*.txt text eol=crlf filter=up\n",
        )
        .unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-qm", "first"]);
        let first = String::from_utf8(git(&upstream, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned();
        git(&root, &["init", "-q", &format!("--object-format={format}")]);
        git(
            &root,
            &[
                "submodule",
                "add",
                "--name",
                "math",
                upstream.to_str().unwrap(),
                "lib",
            ],
        );
        git(&root, &["commit", "-qam", "submodule"]);
        std::fs::write(upstream.join("a.txt"), b"second\n").unwrap();
        std::fs::write(upstream.join(".gitattributes"), b"*.txt -text\n").unwrap();
        git(&upstream, &["commit", "-qam", "second"]);
        let second = String::from_utf8(git(&upstream, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned();
        let child = root.join("lib");
        git(&child, &["fetch", "-q", "origin"]);
        git(&child, &["checkout", "-q", "-B", "topic", &second]);
        Self {
            _directory: directory,
            root,
            child,
            first,
            second,
        }
    }
    fn plan(&self) -> (Repository, submodule::UpdatePlan) {
        let repo = Repository::open(&self.root).unwrap();
        let objects = repo.objects().unwrap();
        let modules = submodule::Modules::read(&repo, &objects, submodule::Source::WorktreeOrIndex)
            .unwrap()
            .unwrap();
        let index = repo.read_index(Default::default()).unwrap().unwrap();
        let entry = index
            .entries()
            .iter()
            .find(|entry| entry.path == b"lib")
            .unwrap();
        let plan = modules
            .get(b"math")
            .unwrap()
            .plan_update(repo.config(), entry.id, submodule::Commands::Refuse)
            .unwrap();
        (repo, plan)
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recorded_checkout_matches_git_detaches_head_preserves_branch_and_uses_child_target_filters() {
    for format in ["sha1", "sha256"] {
        let f = Fixture::new(format);
        let expected = Fixture::new(format);
        for child in [&f.child, &expected.child] {
            git(
                child,
                &["config", "filter.up.clean", "tr '[:upper:]' '[:lower:]'"],
            );
            git(
                child,
                &["config", "filter.up.smudge", "tr '[:lower:]' '[:upper:]'"],
            );
            git(child, &["config", "filter.up.required", "true"]);
        }
        let (repo, plan) = f.plan();
        let options = submodule::UpdateOptions::new(person())
            .with_filter_commands(girt::filter::Commands::Run);
        let report = plan.apply(&repo, &options).unwrap();
        git(&expected.root, &["submodule", "update"]);
        assert!(report.head_updated && report.checkout.unwrap().index_published);
        assert_eq!(
            git(&f.child, &["rev-parse", "HEAD"]),
            format!("{}\n", f.first).as_bytes()
        );
        assert!(
            !oracle::attempt(&f.child, &["symbolic-ref", "-q", "HEAD"], b"")
                .status
                .success()
        );
        assert_eq!(
            git(&f.child, &["rev-parse", "topic"]),
            format!("{}\n", f.second).as_bytes()
        );
        assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"FIRST\r\n");
        assert_eq!(
            std::fs::read(f.child.join("a.txt")).unwrap(),
            std::fs::read(expected.child.join("a.txt")).unwrap()
        );
        assert!(git(&f.root, &["status", "--porcelain=v2"]).is_empty());
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stale_gitlinks_dirty_files_locked_head_and_cancellation_do_not_publish_checkout() {
    let f = Fixture::new("sha1");
    let (repo, plan) = f.plan();
    let options = submodule::UpdateOptions::new(person());
    assert!(matches!(
        plan.apply_with(&repo, &options, &AtomicBool::new(true)),
        Err(submodule::UpdateError::Cancelled)
    ));
    let marker = f.child.join(".git");
    let child = Repository::open(&f.child).unwrap();
    std::fs::write(child.git_dir().join("HEAD.lock"), b"locked").unwrap();
    assert!(plan.apply(&repo, &options).is_err());
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"second\n");
    std::fs::remove_file(child.git_dir().join("HEAD.lock")).unwrap();
    std::fs::write(f.child.join("a.txt"), b"dirty\n").unwrap();
    assert!(matches!(
        plan.apply(&repo, &options),
        Err(submodule::UpdateError::Checkout(_))
    ));
    assert_eq!(
        git(&f.child, &["symbolic-ref", "--short", "HEAD"]),
        b"topic\n"
    );
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"dirty\n");
    assert!(marker.exists());
    git(
        &f.root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{},lib", f.second),
        ],
    );
    assert!(matches!(
        plan.apply(&repo, &options),
        Err(submodule::UpdateError::Changed)
    ));
}

#[test]
fn none_and_cancellation_do_not_open_a_missing_child() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    let repo = Repository::open(root).unwrap();
    let modules = definition();
    let config =
        girt::Config::parse(b"[submodule \"math\"]\nurl = explicit\nupdate = none\n").unwrap();
    let recorded = girt::ObjectId::for_blob(repo.object_format(), b"missing commit");
    let plan = modules
        .get(b"math")
        .unwrap()
        .plan_update(&config, recorded, submodule::Commands::Refuse)
        .unwrap();
    let options = submodule::UpdateOptions::new(person());
    let report = plan.apply(&repo, &options).unwrap();
    assert!(!report.head_updated && report.checkout.is_none());
    assert!(!root.join("lib").exists());
    assert!(matches!(
        plan.apply_with(&repo, &options, &AtomicBool::new(true)),
        Err(submodule::UpdateError::Cancelled)
    ));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let config = girt::Config::parse(b"[submodule \"math\"]\nurl = explicit\n").unwrap();
        let plan = modules
            .get(b"math")
            .unwrap()
            .plan_update(&config, recorded, submodule::Commands::Refuse)
            .unwrap();
        assert!(matches!(
            plan.apply(&repo, &options),
            Err(submodule::UpdateError::UnsupportedPlatform)
        ));
    }
}

#[test]
fn init_refresh_keeps_caller_selected_local_sources() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("parent");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    let extra = directory.path().join("extra");
    std::fs::write(
        &extra,
        b"[submodule \"math\"]\nurl = chosen\nactive = false\n",
    )
    .unwrap();
    let inputs =
        girt::config::ConfigInputs::default().with_files(vec![girt::config::ConfigFile::new(
            extra,
            girt::config::ConfigScope::Local,
        )]);
    let repo = Repository::open_with_config(&root, &inputs).unwrap();
    let modules = definition();
    let plan = modules
        .get(b"math")
        .unwrap()
        .plan_init(repo.config(), b"unused")
        .unwrap();
    assert!(plan.apply(&repo).unwrap());
    let fresh = Repository::open_with_config(&root, &inputs).unwrap();
    assert_eq!(
        fresh.config().string("submodule", Some(b"math"), "url"),
        Some(b"chosen".as_slice())
    );
    assert_eq!(
        fresh
            .config()
            .boolean("submodule", Some(b"math"), "active")
            .unwrap(),
        Some(true)
    );
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn matching_head_retains_branch_attachment_and_dirty_content_as_git_does() {
    let f = Fixture::new("sha1");
    git(
        &f.root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{},lib", f.second),
        ],
    );
    std::fs::write(f.child.join("a.txt"), b"dirty\n").unwrap();
    let (repo, plan) = f.plan();
    let report = plan
        .apply(&repo, &submodule::UpdateOptions::new(person()))
        .unwrap();
    assert!(!report.head_updated && report.checkout.is_none());
    git(&f.root, &["submodule", "update"]);
    assert_eq!(
        git(&f.child, &["symbolic-ref", "--short", "HEAD"]),
        b"topic\n"
    );
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"dirty\n");
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn filter_permission_bounds_and_parent_local_settings_do_not_bypass_checkout_safety() {
    let f = Fixture::new("sha1");
    git(
        &f.child,
        &["config", "filter.up.smudge", "tr '[:lower:]' '[:upper:]'"],
    );
    git(&f.child, &["config", "filter.up.required", "true"]);
    let (repo, plan) = f.plan();
    let refused = submodule::UpdateOptions::new(person());
    assert!(matches!(
        plan.apply(&repo, &refused),
        Err(submodule::UpdateError::Checkout(_))
    ));
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"second\n");
    let limited = submodule::UpdateOptions::new(person())
        .with_filter_commands(girt::filter::Commands::Run)
        .with_limits(girt::checkout::Limits::default().with_max_worktree_bytes(1));
    assert!(matches!(
        plan.apply(&repo, &limited),
        Err(submodule::UpdateError::Checkout(_))
    ));
    assert_eq!(
        git(&f.child, &["symbolic-ref", "--short", "HEAD"]),
        b"topic\n"
    );
    git(&f.child, &["config", "--remove-section", "filter.up"]);
    git(&f.root, &["config", "filter.up.smudge", "false"]);
    git(&f.root, &["config", "filter.up.required", "true"]);
    let (repo, plan) = f.plan();
    plan.apply(&repo, &refused).unwrap();
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"first\r\n");
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn missing_child_target_and_symlink_are_refused_without_transport() {
    let f = Fixture::new("sha1");
    let (repo, plan) = f.plan();
    let options = submodule::UpdateOptions::new(person());
    let moved = f.root.parent().unwrap().join("saved-child");
    std::fs::rename(&f.child, &moved).unwrap();
    assert!(matches!(
        plan.apply(&repo, &options),
        Err(submodule::UpdateError::MissingCheckout)
    ));
    std::os::unix::fs::symlink(&moved, &f.child).unwrap();
    assert!(plan.apply(&repo, &options).is_err());
    std::fs::remove_file(&f.child).unwrap();
    std::fs::rename(&moved, &f.child).unwrap();
    let absent = girt::ObjectId::for_blob(repo.object_format(), b"absent");
    git(
        &f.root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{absent},lib"),
        ],
    );
    let (repo, plan) = f.plan();
    let error = plan.apply(&repo, &options).unwrap_err();
    let submodule::UpdateError::Prepare(source) = error else {
        panic!("expected preparation error");
    };
    assert!(source.is::<girt::ReadCommitError>());
    assert_eq!(
        git(&f.child, &["symbolic-ref", "--short", "HEAD"]),
        b"topic\n"
    );
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"second\n");
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn update_inherits_selected_global_config_and_captured_xdg_attributes() {
    let f = Fixture::new("sha1");
    let global = f.root.parent().unwrap().join("global");
    std::fs::write(&global, b"[filter \"up\"]\nclean = tr '[:upper:]' '[:lower:]'\nsmudge = tr '[:lower:]' '[:upper:]'\nrequired = true\n").unwrap();
    let xdg = f.root.parent().unwrap().join("xdg");
    std::fs::create_dir_all(xdg.join("git")).unwrap();
    // An empty target attribute file leaves this conversion entirely to the captured XDG source.
    std::fs::write(
        xdg.join("git/attributes"),
        b"a.txt text eol=crlf filter=up\n",
    )
    .unwrap();
    std::fs::write(f.child.join("a.txt"), b"first\n").unwrap();
    std::fs::write(f.child.join(".gitattributes"), b"").unwrap();
    git(&f.child, &["add", "."]);
    git(
        &f.child,
        &["commit", "-qm", "target with global attributes"],
    );
    let target = String::from_utf8(git(&f.child, &["rev-parse", "HEAD"])).unwrap();
    git(&f.child, &["checkout", "-q", &f.second]);
    git(
        &f.root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{},lib", target.trim()),
        ],
    );
    let inputs =
        girt::config::ConfigInputs::default().with_files(vec![girt::config::ConfigFile::new(
            global,
            girt::config::ConfigScope::Global,
        )]);
    let repo = Repository::open_with_config(&f.root, &inputs).unwrap();
    let modules = definition();
    let recorded = repo
        .read_index(Default::default())
        .unwrap()
        .unwrap()
        .entries()
        .iter()
        .find(|entry| entry.path == b"lib")
        .unwrap()
        .id;
    let plan = modules
        .get(b"math")
        .unwrap()
        .plan_update(repo.config(), recorded, submodule::Commands::Refuse)
        .unwrap();
    let environment =
        girt::workflow::Environment::default().with("XDG_CONFIG_HOME", xdg.as_os_str());
    // This is B's clean representation under the newly selected global filter. B disables text
    // conversion but still inherits the global filter, so its worktree is uppercase with LF.
    std::fs::write(f.child.join("a.txt"), b"SECOND\n").unwrap();
    let options = submodule::UpdateOptions::new(person())
        .with_environment(environment)
        .with_filter_commands(girt::filter::Commands::Run);
    plan.apply(&repo, &options).unwrap();
    assert_eq!(std::fs::read(f.child.join("a.txt")).unwrap(), b"FIRST\r\n");
}

#[test]
fn noncheckout_update_methods_are_typed_refusals_even_for_an_authorized_command() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    let repo = Repository::open(root).unwrap();
    let modules = definition();
    let options = submodule::UpdateOptions::new(person());
    let id = girt::ObjectId::for_blob(repo.object_format(), b"absent");
    for method in ["rebase", "merge", "!exit 67"] {
        let config = girt::Config::parse(
            format!("[submodule \"math\"]\nurl = unused\nupdate = {method}\n").as_bytes(),
        )
        .unwrap();
        let config = girt::Config::resolve(
            &girt::config::ConfigInputs::default().with_command(Some(config)),
        )
        .unwrap();
        let plan = modules
            .get(b"math")
            .unwrap()
            .plan_update(&config, id, submodule::Commands::Run)
            .unwrap();
        assert!(matches!(
            plan.apply(&repo, &options),
            Err(submodule::UpdateError::UnsupportedMethod)
        ));
    }
    assert!(!root.join("lib").exists());
}

#[test]
fn init_refresh_preserves_command_worktree_overrides_and_cwd_layout() {
    for explicit in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let cwd = directory.path().join("cwd");
        std::fs::create_dir(&cwd).unwrap();
        let storage = directory.path().join("metadata.git");
        git(
            directory.path(),
            &["init", "-q", "--bare", storage.to_str().unwrap()],
        );
        git(
            directory.path(),
            &[
                "--git-dir",
                storage.to_str().unwrap(),
                "config",
                "core.bare",
                "false",
            ],
        );
        let location = girt::RepositoryLocation::at_git_dir(&storage).unwrap();
        let worktree = if explicit { &root } else { &cwd };
        let parent = location
            .read_metadata_for_command(
                &Default::default(),
                &cwd,
                explicit.then_some(root.as_path()),
            )
            .unwrap()
            .open_storage()
            .unwrap();
        assert_eq!(
            parent.worktree(),
            Some(std::fs::canonicalize(worktree).unwrap().as_path())
        );
        let modules = definition();
        let plan = modules
            .get(b"math")
            .unwrap()
            .plan_init(parent.config(), b"https://example.com/team/parent.git")
            .unwrap();
        assert!(plan.apply(&parent).unwrap());
        assert!(!worktree.join(".git").exists());
        let bytes = std::fs::read(storage.join("config")).unwrap();
        assert_eq!(
            girt::Config::parse(&bytes)
                .unwrap()
                .string("submodule", Some(b"math"), "url"),
            Some(b"https://example.com/team/child.git".as_slice())
        );
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn child_filter_environments_match_git_for_one_shot_and_process_drivers() {
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};
    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
    }
    let names = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIRT_FILTER_ENV_PROBE",
    ];
    for process in [false, true] {
        let mut results = Vec::new();
        for girt in [false, true] {
            let f = Fixture::new("sha1");
            let parent = Repository::open(&f.root).unwrap();
            let child = Repository::open(&f.child).unwrap();
            let capture = f.root.parent().unwrap().join("captured-environment");
            let script = f.root.parent().unwrap().join("filter.sh");
            let mut text = String::from("{\n");
            for name in names {
                text.push_str(&format!(
                    "printf '%s=%s\\n' '{name}' \"${{{name}-unset}}\"\n"
                ));
            }
            text.push_str(&format!("}} > {}\n", quote(&capture)));
            if process {
                let filter =
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/filter-process.pl");
                text.push_str(&format!(
                    "exec perl {} {}\n",
                    quote(&filter),
                    quote(&f.root.parent().unwrap().join("process-log"))
                ));
            } else {
                text.push_str("cat\n");
            }
            std::fs::write(&script, text).unwrap();
            let command = format!("sh {}", quote(&script));
            git(&f.child, &["config", "filter.up.clean", "cat"]);
            git(
                &f.child,
                &[
                    "config",
                    if process {
                        "filter.up.process"
                    } else {
                        "filter.up.smudge"
                    },
                    &command,
                ],
            );
            git(&f.child, &["config", "filter.up.required", "true"]);
            let mut values: Vec<(&str, OsString)> = ["GIT_DIR", "GIT_COMMON_DIR"]
                .into_iter()
                .map(|name| (name, parent.git_dir().as_os_str().to_owned()))
                .collect();
            values.extend([
                ("GIT_WORK_TREE", f.root.as_os_str().to_owned()),
                (
                    "GIT_INDEX_FILE",
                    parent.git_dir().join("index").into_os_string(),
                ),
                (
                    "GIT_OBJECT_DIRECTORY",
                    parent.object_dir().as_os_str().to_owned(),
                ),
                (
                    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                    parent.object_dir().as_os_str().to_owned(),
                ),
                (
                    "GIT_CONFIG",
                    parent.common_dir().join("config").into_os_string(),
                ),
                (
                    "GIT_GRAFT_FILE",
                    f.root
                        .parent()
                        .unwrap()
                        .join("absent-grafts")
                        .into_os_string(),
                ),
                (
                    "GIT_SHALLOW_FILE",
                    f.root
                        .parent()
                        .unwrap()
                        .join("absent-shallow")
                        .into_os_string(),
                ),
                (
                    "PATH",
                    std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
                ),
                ("GIT_CONFIG_NOSYSTEM", OsString::from("1")),
                (
                    "GIT_CONFIG_GLOBAL",
                    f.root
                        .parent()
                        .unwrap()
                        .join("absent-global")
                        .into_os_string(),
                ),
            ]);
            values.extend(
                [
                    ("GIT_IMPLICIT_WORK_TREE", "1"),
                    ("GIT_NO_REPLACE_OBJECTS", "1"),
                    ("GIT_REPLACE_REF_BASE", "refs/replace/sentinel/"),
                    ("GIT_PREFIX", "sentinel/"),
                    ("GIT_CONFIG_PARAMETERS", "'user.email=explicit@example.com'"),
                    ("GIT_CONFIG_COUNT", "1"),
                    ("GIT_CONFIG_KEY_0", "user.name"),
                    ("GIT_CONFIG_VALUE_0", "Explicit"),
                    ("GIRT_FILTER_ENV_PROBE", "explicit"),
                ]
                .map(|(name, value)| (name, OsString::from(value))),
            );
            if girt {
                let (_, plan) = f.plan();
                let environment = values.iter().fold(
                    girt::workflow::Environment::default(),
                    |environment, (name, value)| environment.with(*name, value.clone()),
                );
                let options = submodule::UpdateOptions::new(person())
                    .with_environment(environment)
                    .with_filter_commands(girt::filter::Commands::Run);
                plan.apply(&parent, &options).unwrap();
            } else {
                let variables: Vec<(&str, &OsStr)> = values
                    .iter()
                    .map(|(name, value)| (*name, value.as_os_str()))
                    .collect();
                let output =
                    oracle::attempt_with_env(&f.root, &["submodule", "update"], b"", &variables);
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let captured = std::fs::read_to_string(&capture).unwrap();
            let mut fields: BTreeMap<String, String> = captured
                .lines()
                .map(|line| {
                    let (name, value) = line.split_once('=').unwrap();
                    (name.to_owned(), value.to_owned())
                })
                .collect();
            let git_dir = fields.remove("GIT_DIR").unwrap();
            assert_eq!(std::fs::canonicalize(git_dir).unwrap(), child.git_dir());
            assert_eq!(fields.get("GIRT_FILTER_ENV_PROBE").unwrap(), "explicit");
            assert_eq!(fields.get("GIT_PREFIX").unwrap(), "");
            results.push(fields);
        }
        assert_eq!(results[0], results[1], "process={process}");
    }
}
