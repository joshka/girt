//! Original fixtures and isolated Git executable observations; no upstream source inputs.
#[path = "support/layout_git.rs"]
mod oracle;

use std::path::Path;

use girt::config::{ConfigFile, ConfigInputs, ConfigScope, Document};
use girt::submodule::{Commands, Modules, PlanError, Update, UpdateMethod};
use girt::{Config, ObjectFormat, ObjectId};

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    oracle::git(root, args, b"")
}

#[test]
fn initialization_assignments_match_git_and_preserve_existing_settings() {
    for format in ["sha1", "sha256"] {
        for method in [
            None,
            Some("checkout"),
            Some("rebase"),
            Some("merge"),
            Some("none"),
        ] {
            for existing in [
                b"[submodule \"math\"]\nurl = chosen\nactive = false\n".as_slice(),
                b"[submodule]\nactive = vendor/math\n".as_slice(),
                b"[submodule \"math\"]\nactive = true\n",
                b"".as_slice(),
                b"[submodule \"math\"]\nurl = chosen\n",
                b"[submodule \"math\"]\nactive = false\n",
                b"[submodule \"math\"]\nurl = chosen\nupdate = rebase\n",
            ] {
                let directory = tempfile::tempdir().unwrap();
                let root = directory.path();
                git(root, &["init", "-q", &format!("--object-format={format}")]);
                git(
                    root,
                    &[
                        "config",
                        "remote.origin.url",
                        "https://example.com/team/parent.git",
                    ],
                );
                std::fs::write(root.join("a"), b"a").unwrap();
                git(root, &["add", "a"]);
                git(root, &["commit", "-qm", "initial"]);
                let id = String::from_utf8(git(root, &["rev-parse", "HEAD"])).unwrap();
                git(
                    root,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{},vendor/math", id.trim()),
                    ],
                );
                let mut metadata =
                    b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\n".to_vec();
                if let Some(method) = method {
                    metadata.extend_from_slice(format!("update = {method}\n").as_bytes());
                }
                std::fs::write(root.join(".gitmodules"), &metadata).unwrap();
                let config_path = root.join(".git/config");
                let mut original = std::fs::read(&config_path).unwrap();
                original.extend_from_slice(existing);
                std::fs::write(&config_path, &original).unwrap();

                let modules = Modules::parse(&metadata).unwrap();
                let definition = modules.get(b"math").unwrap();
                let plan = definition
                    .plan_init(
                        &Config::parse(&original).unwrap(),
                        b"https://example.com/team/parent.git",
                    )
                    .unwrap();
                let mut document = Document::parse(&original).unwrap();
                for (key, value) in plan.settings() {
                    document
                        .append("submodule", Some(plan.name()), key, value)
                        .unwrap();
                }
                git(root, &["submodule", "init"]);
                let expected = Config::parse(&std::fs::read(&config_path).unwrap()).unwrap();

                assert_eq!(plan.path(), b"vendor/math");
                for key in ["active", "url", "update"] {
                    assert_eq!(
                        document.config().value("submodule", Some(b"math"), key),
                        expected.value("submodule", Some(b"math"), key),
                        "{format}, {method:?}, {existing:?}, {key}"
                    );
                }
                assert!(
                    definition
                        .plan_init(document.config(), b"ignored")
                        .unwrap()
                        .settings()
                        .next()
                        .is_none()
                );
            }
        }
    }
}

fn recorded() -> ObjectId {
    ObjectId::for_blob(ObjectFormat::Sha1, b"caller selected ID")
}

#[test]
fn update_policy_preserves_target_and_requires_initialization() {
    let modules =
        Modules::parse(b"[submodule \"math\"]\npath = vendor/math\nupdate = merge\n").unwrap();
    let definition = modules.get(b"math").unwrap();
    assert!(matches!(
        definition.plan_update(&Config::parse(b"").unwrap(), recorded(), Commands::Refuse),
        Err(PlanError::Uninitialized)
    ));
    for (method, expected) in [
        ("checkout", Update::Checkout),
        ("rebase", Update::Rebase),
        ("merge", Update::Merge),
        ("none", Update::None),
    ] {
        let config = Config::parse(
            format!("[submodule \"math\"]\nurl = ../literal.git\nupdate = {method}\n").as_bytes(),
        )
        .unwrap();
        let plan = definition
            .plan_update(&config, recorded(), Commands::Refuse)
            .unwrap();
        assert_eq!(plan.name(), b"math");
        assert_eq!(plan.path(), b"vendor/math");
        assert_eq!(plan.url(), b"../literal.git");
        assert_eq!(plan.recorded(), recorded());
        assert_eq!(plan.method(), &UpdateMethod::BuiltIn(expected));
    }
    let config = Config::parse(b"[submodule \"math\"]\nurl = initialized\n").unwrap();
    assert_eq!(
        definition
            .plan_update(&config, recorded(), Commands::Refuse)
            .unwrap()
            .method(),
        &UpdateMethod::BuiltIn(Update::Merge)
    );
    for invalid in [
        b"url\n".as_slice(),
        b"url =\n",
        b"url = -unsafe\n",
        b"url = good\nupdate = bad\n",
        b"url = good\nupdate\n",
    ] {
        let config = Config::parse(&[b"[submodule \"math\"]\n", invalid].concat()).unwrap();
        assert!(matches!(
            definition.plan_update(&config, recorded(), Commands::Run),
            Err(PlanError::InvalidSetting(_))
        ));
    }
}

#[test]
fn command_origin_is_independent_of_permission_and_metadata_commands_are_never_accepted() {
    assert!(
        Modules::parse(b"[submodule \"math\"]\npath = lib\nurl = url\nupdate = !echo command\n")
            .is_err()
    );
    let modules = Modules::parse(b"[submodule \"math\"]\npath = lib\nurl = url\n").unwrap();
    let definition = modules.get(b"math").unwrap();
    let bytes = b"[submodule \"math\"]\nurl = initialized\nupdate = !echo command\n";
    let config = Config::parse(bytes).unwrap();
    assert!(matches!(
        definition.plan_update(&config, recorded(), Commands::Run),
        Err(PlanError::UntrustedCommand)
    ));

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config");
    std::fs::write(&path, bytes).unwrap();
    for scope in [
        ConfigScope::System,
        ConfigScope::Global,
        ConfigScope::Local,
        ConfigScope::Worktree,
    ] {
        let inputs = ConfigInputs::default().with_files(vec![ConfigFile::new(path.clone(), scope)]);
        let config = Config::resolve(&inputs).unwrap();
        assert!(matches!(
            definition.plan_update(&config, recorded(), Commands::Refuse),
            Err(PlanError::CommandRefused)
        ));
        assert_eq!(
            definition
                .plan_update(&config, recorded(), Commands::Run)
                .unwrap()
                .method(),
            &UpdateMethod::Command(b"echo command".to_vec())
        );
    }
    for inputs in [
        ConfigInputs::default().with_command(Some(Config::parse(bytes).unwrap())),
        ConfigInputs::default().with_environment(Some(Config::parse(bytes).unwrap())),
    ] {
        let config = Config::resolve(&inputs).unwrap();
        assert!(matches!(
            definition
                .plan_update(&config, recorded(), Commands::Run)
                .unwrap()
                .method(),
            UpdateMethod::Command(_)
        ));
    }
}

#[test]
fn planning_refuses_unsafe_names_and_paths_without_touching_files() {
    for metadata in [
        b"[submodule \"../escape\"]\npath = safe\nurl = url\n".as_slice(),
        b"[submodule \"safe\"]\npath = ../escape\nurl = url\n",
    ] {
        let modules = Modules::parse(metadata).unwrap();
        let definition = &modules.definitions()[0];
        assert!(matches!(
            definition.plan_init(&Config::parse(b"").unwrap(), b"remote"),
            Err(PlanError::Definition(_))
        ));
        assert!(matches!(
            definition.plan_update(&Config::parse(b"").unwrap(), recorded(), Commands::Run),
            Err(PlanError::Definition(_))
        ));
    }
}

// Git's own shell runs these commands on Unix and Git for Windows. The global file is isolated;
// no ambient Git config or inherited Git environment reaches the oracle.
fn user_git(root: &Path, global: &Path, args: &[&str], environment_command: Option<&str>) {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_AUTHOR_NAME", "A. Writer")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C. Recorder")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0530")
        .env("GIT_COMMITTER_DATE", "@1700000123 -0700");
    if let Some(value) = environment_command {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "submodule.math.update")
            .env("GIT_CONFIG_VALUE_0", value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn git_runs_user_configured_commands_but_refuses_metadata_commands() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("parent");
    let child = directory.path().join("child");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&child).unwrap();
    git(&child, &["init", "-q"]);
    std::fs::write(child.join("a"), b"original").unwrap();
    git(&child, &["add", "a"]);
    git(&child, &["commit", "-qm", "initial"]);
    git(&root, &["init", "-q"]);
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--name",
            "math",
            child.to_str().unwrap(),
            "lib",
        ],
    );
    git(&root, &["commit", "-qam", "submodule"]);
    let global = directory.path().join("global");
    let command = "!echo executed > command-marker";

    for scope in ["global", "local", "command", "environment"] {
        std::fs::write(&global, b"").unwrap();
        let _ = oracle::attempt(
            &root,
            &["config", "--unset-all", "submodule.math.update"],
            b"",
        );
        match scope {
            "global" => std::fs::write(
                &global,
                format!("[submodule \"math\"]\nupdate = {command}\n"),
            )
            .unwrap(),
            "local" => {
                git(&root, &["config", "submodule.math.update", command]);
            }
            _ => {}
        }
        let args = if scope == "command" {
            vec![
                "-c",
                "submodule.math.update=!echo executed > command-marker",
                "submodule",
                "update",
                "--force",
            ]
        } else {
            vec!["submodule", "update", "--force"]
        };
        user_git(
            &root,
            &global,
            &args,
            (scope == "environment").then_some(command),
        );
        let marker = root.join("lib/command-marker");
        assert!(
            std::fs::read(&marker).unwrap().starts_with(b"executed "),
            "{scope}"
        );
        std::fs::remove_file(marker).unwrap();
    }
    std::fs::write(
        root.join(".gitmodules"),
        b"[submodule \"math\"]\npath = lib\nurl = unused\nupdate = !echo forbidden\n",
    )
    .unwrap();
    let output = oracle::attempt(&root, &["submodule", "update", "--force"], b"");
    assert_eq!(output.status.code(), Some(128));
}

#[test]
fn implicit_urls_and_empty_commands_have_typed_shape_errors() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("a"), b"a").unwrap();
    git(root, &["add", "a"]);
    git(root, &["commit", "-qm", "initial"]);
    let id = String::from_utf8(git(root, &["rev-parse", "HEAD"])).unwrap();
    git(
        root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},lib", id.trim()),
        ],
    );
    let metadata = b"[submodule \"math\"]\npath = lib\nurl = unused\n";
    std::fs::write(root.join(".gitmodules"), metadata).unwrap();
    let config_path = root.join(".git/config");
    let mut bytes = std::fs::read(&config_path).unwrap();
    bytes.extend_from_slice(b"[submodule \"math\"]\nurl\n");
    std::fs::write(&config_path, &bytes).unwrap();
    let config = Config::parse(&bytes).unwrap();
    let modules = Modules::parse(metadata).unwrap();
    let definition = modules.get(b"math").unwrap();
    assert!(
        !oracle::attempt(root, &["submodule", "init"], b"")
            .status
            .success()
    );
    assert!(matches!(
        definition.plan_init(&config, b"remote"),
        Err(PlanError::InvalidSetting("url"))
    ));

    let config = Config::parse(b"[submodule \"math\"]\nurl = url\nupdate = !\n").unwrap();
    for commands in [Commands::Refuse, Commands::Run] {
        assert!(matches!(
            definition.plan_update(&config, recorded(), commands),
            Err(PlanError::InvalidSetting("update"))
        ));
    }
}
