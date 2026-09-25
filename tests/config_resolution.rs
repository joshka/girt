//! Original fixtures generated here; Git's executable is the independent interpretation oracle.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output};

use girt::config::{ConfigFile, ConfigInputs, ConfigScope, ResolveFailure};
use girt::{Config, InitKind, ObjectFormat, Repository};
use rstest::rstest;

fn file(path: &Path, scope: ConfigScope) -> ConfigFile {
    ConfigFile {
        path: path.into(),
        scope,
        optional: false,
    }
}

fn inputs(path: &Path) -> ConfigInputs {
    ConfigInputs {
        files: vec![file(path, ConfigScope::Local)],
        ..Default::default()
    }
}

fn git(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    if let Some(system) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", system);
    }
    command
        .current_dir(root)
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-global"))
        .args(args);
    command.envs(env.iter().copied());
    command
        .output()
        .expect("Git is required for configuration observations")
}

fn observed(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Vec<u8> {
    let output = git(root, args, env);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn values(config: &Config) -> Vec<u8> {
    config
        .values("demo", None, "value")
        .flat_map(|value| {
            let mut bytes = value.unwrap_or_default().to_vec();
            bytes.push(0);
            bytes
        })
        .collect()
}

#[test]
fn nested_relative_includes_preserve_order_provenance_and_snapshot() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    let path = root.path().join("config");
    let original = b"[demo]\nvalue=first\n[include]\npath=sub/one\n[demo]\nvalue=last\n";
    std::fs::write(&path, original).unwrap();
    std::fs::write(
        root.path().join("sub/one"),
        b"[include]\npath=../two\n[demo]\nvalue=one\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("two"),
        b"[demo]\nvalue=two\nvalue=\nvalue=\xff\n",
    )
    .unwrap();
    let environment_before: BTreeMap<_, _> = std::env::vars_os().collect();
    let config = Config::resolve(&inputs(&path)).unwrap();
    assert_eq!(values(&config), b"first\0two\0\0\xff\0one\0last\0");
    assert_eq!(
        values(&config),
        observed(
            root.path(),
            &[
                "config",
                "--file",
                "config",
                "--includes",
                "--null",
                "--get-all",
                "demo.value"
            ],
            &[]
        )
    );
    let nested = &config.entries()[3];
    assert_eq!(nested.origin.as_ref().unwrap().included_from.len(), 2);
    assert_eq!(nested.origin.as_ref().unwrap().included_from[0].line, 4);
    assert_eq!(nested.origin.as_ref().unwrap().included_from[1].line, 2);
    assert_eq!(nested.origin.as_ref().unwrap().scope, ConfigScope::Local);
    assert_eq!(nested.line, 2);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::env::vars_os().collect::<BTreeMap<_, _>>(),
        environment_before
    );
    std::fs::write(root.path().join("two"), b"[demo]\nvalue=new\n").unwrap();
    assert_eq!(values(&config), b"first\0two\0\0\xff\0one\0last\0");
    assert_eq!(
        values(&Config::resolve(&inputs(&path)).unwrap()),
        b"first\0new\0one\0last\0"
    );
}

#[rstest]
#[case::branch("onbranch:topic/", true)]
#[case::branch_star("onbranch:topic/*", true)]
#[case::branch_miss("onbranch:main", false)]
#[case::gitdir("gitdir:**/repo/", true)]
#[case::gitdir_suffix("gitdir:repo/", true)]
#[case::casefold("gitdir/i:REPO/", true)]
#[case::case_sensitive("gitdir:REPO/", false)]
#[case::relative("gitdir:./repo/", true)]
#[case::home("gitdir:~/repo/", true)]
#[case::unknown("future:condition", false)]
#[case::hasconfig("hasconfig:remote.*.url:https://example.com/**", true)]
#[case::hasconfig_miss("hasconfig:remote.*.url:ssh://**", false)]
fn conditions_match_git(#[case] condition: &str, #[case] matches: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    Repository::init(ObjectFormat::Sha1, &repo, InitKind::Worktree).unwrap();
    std::fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/topic/one\n").unwrap();
    std::fs::write(root.path().join("child"), b"[demo]\nvalue=matched\n").unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, format!("[demo]\nvalue=base\n[includeIf \"{condition}\"]\npath=child\n[remote \"r\"]\nurl=https://example.com/a/b\n")).unwrap();
    let mut input = inputs(&path);
    input
        .context
        .git_dirs
        .push(std::fs::canonicalize(repo.join(".git")).unwrap());
    input.context.home = Some(std::fs::canonicalize(root.path()).unwrap());
    input.context.branch = Some(b"topic/one".to_vec());
    let actual = values(&Config::resolve(&input).unwrap());
    let expected: &[u8] = if matches {
        b"base\0matched\0"
    } else {
        b"base\0"
    };
    assert_eq!(actual, expected);
    assert_eq!(
        actual,
        observed(
            root.path(),
            &[
                "-C",
                "repo",
                "config",
                "--file",
                "../config",
                "--includes",
                "--null",
                "--get-all",
                "demo.value"
            ],
            &[]
        )
    );
}

#[test]
fn layers_environment_command_remote_reset_and_format_bootstrap() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    Repository::init(ObjectFormat::Sha256, &repo, InitKind::Worktree).unwrap();
    std::fs::create_dir_all(root.path().join(".config/git")).unwrap();
    std::fs::write(
        root.path().join("system"),
        b"[demo]\nvalue=system\n[remote \"r\"]\nurl=old\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join(".config/git/config"),
        b"[demo]\nvalue=xdg\n",
    )
    .unwrap();
    std::fs::write(root.path().join(".gitconfig"), b"[demo]\nvalue=home\n").unwrap();
    let local = repo.join(".git/config");
    let mut bytes = std::fs::read(&local).unwrap();
    bytes.extend_from_slice(
        b"[extensions]\nworktreeConfig=true\n[demo]\nvalue=local\n[include]\npath=included\n",
    );
    std::fs::write(&local, bytes).unwrap();
    std::fs::write(
        repo.join(".git/included"),
        b"[extensions]\nobjectFormat=sha1\n[remote \"r\"]\nurl=\nurl=new\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".git/config.worktree"),
        b"[demo]\nvalue=worktree\n[extensions]\nobjectFormat=sha1\n",
    )
    .unwrap();
    let mut env: BTreeMap<String, OsString> = BTreeMap::new();
    env.insert("HOME".into(), root.path().into());
    env.insert("GIT_CONFIG_COUNT".into(), "1".into());
    env.insert("GIT_CONFIG_KEY_0".into(), "demo.value".into());
    env.insert("GIT_CONFIG_VALUE_0".into(), "environment".into());
    let mut input = ConfigInputs::from_environment(
        Some(root.path().join("system")),
        |key| env.get(key).cloned(),
        10,
    )
    .unwrap();
    input.command = Some(Config::parse(b"[demo]\nvalue=command\n").unwrap());
    let opened = Repository::open_with_config(&repo, &input).unwrap();
    assert_eq!(
        values(opened.config()),
        b"system\0xdg\0home\0local\0worktree\0environment\0command\0"
    );
    let scopes: Vec<_> = opened
        .config()
        .entries()
        .iter()
        .filter(|e| e.section.eq_ignore_ascii_case(b"demo"))
        .map(|e| e.origin.as_ref().unwrap().scope)
        .collect();
    assert_eq!(
        scopes,
        [
            ConfigScope::System,
            ConfigScope::Global,
            ConfigScope::Global,
            ConfigScope::Local,
            ConfigScope::Worktree,
            ConfigScope::Environment,
            ConfigScope::Command
        ]
    );
    assert_eq!(opened.object_format(), ObjectFormat::Sha256);
    assert_eq!(
        girt::remote::Remote::find(opened.config(), b"r")
            .unwrap()
            .unwrap()
            .urls(),
        [b"new".to_vec()]
    );
    let git_env = [
        ("GIT_CONFIG_NOSYSTEM", "0"),
        ("GIT_CONFIG_SYSTEM", "../system"),
        ("GIT_CONFIG_GLOBAL", "../.gitconfig"),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "demo.value"),
        ("GIT_CONFIG_VALUE_0", "environment"),
    ];
    // Explicit GLOBAL suppresses XDG, so compare the independently expected selected sequence.
    assert_eq!(
        observed(
            &repo,
            &[
                "-c",
                "demo.value=command",
                "config",
                "--null",
                "--get-all",
                "demo.value"
            ],
            &git_env
        ),
        b"system\0home\0local\0worktree\0environment\0command\0"
    );
    assert_eq!(
        observed(&repo, &["rev-parse", "--show-object-format"], &[]),
        b"sha256\n"
    );
}

#[rstest]
#[case::not_matching("no-match")]
#[case::matching("https://example.com/**")]
fn hasconfig_rejects_remote_urls_even_in_unmatched_descendants(#[case] pattern: &str) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, format!("[includeIf \"hasconfig:remote.*.url:{pattern}\"]\npath=child\n[remote \"r\"]\nurl=https://example.com/a\n")).unwrap();
    std::fs::write(root.path().join("child"), b"[include]\npath=grandchild\n").unwrap();
    std::fs::write(
        root.path().join("grandchild"),
        b"[remote \"secret\"]\nurl=secret\n",
    )
    .unwrap();
    let error = Config::resolve(&inputs(&path)).unwrap_err();
    assert!(matches!(error.source, ResolveFailure::ConditionalRemote));
    assert_eq!(error.included_from.len(), 2);
    assert_eq!(error.location.line, 2);
    assert!(
        !git(
            root.path(),
            &["config", "--file", "config", "--includes", "--list"],
            &[]
        )
        .status
        .success()
    );
}

#[rstest]
#[case::missing(false)]
#[case::optional(true)]
fn missing_root_policy(#[case] optional: bool) {
    let root = tempfile::tempdir().unwrap();
    let mut input = inputs(&root.path().join("absent"));
    input.files[0].optional = optional;
    assert_eq!(Config::resolve(&input).is_ok(), optional);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[rstest]
#[case::cycle(b"[include]\npath=config\n", "cycle")]
#[case::malformed(b"[include]\npath=child\n", "parse")]
#[case::implicit(b"[include]\npath\n", "input")]
fn contextual_failures(#[case] bytes: &[u8], #[case] category: &str) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, bytes).unwrap();
    std::fs::write(root.path().join("child"), b"[demo]\nvalue=\\q\n").unwrap();
    let error = Config::resolve(&inputs(&path)).unwrap_err();
    let actual = match error.source {
        ResolveFailure::Cycle => "cycle",
        ResolveFailure::Parse(_) => "parse",
        ResolveFailure::Input(_) => "input",
        _ => "unexpected",
    };
    assert_eq!(actual, category);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(
        !git(
            root.path(),
            &["config", "--file", "config", "--includes", "--list"],
            &[]
        )
        .status
        .success()
    );
}

#[rstest]
#[case::bytes(3, 10, 100, 1000)]
#[case::depth(1000, 0, 100, 1000)]
#[case::entries(1000, 10, 1, 1000)]
#[case::matching(1000, 10, 100, 1)]
fn resolution_budgets(
    #[case] bytes: usize,
    #[case] depth: usize,
    #[case] entries: usize,
    #[case] match_cells: usize,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, b"[includeIf \"onbranch:topic/**\"]\npath=child\n").unwrap();
    std::fs::write(root.path().join("child"), b"[demo]\nvalue=one\n").unwrap();
    let mut input = inputs(&path);
    input.context.branch = Some(b"topic/one".to_vec());
    input.limits = girt::config::ResolveLimits {
        bytes,
        depth,
        entries,
        match_cells,
    };
    assert!(matches!(
        Config::resolve(&input).unwrap_err().source,
        ResolveFailure::Limit(_)
    ));
}

#[test]
fn environment_values_and_name_bytes_are_literal() {
    let map: BTreeMap<_, _> = [
        ("GIT_CONFIG_COUNT", b"2".to_vec()),
        ("GIT_CONFIG_KEY_0", b"Remote.Ori\xffgin.URL".to_vec()),
        ("GIT_CONFIG_VALUE_0", b"\\n # literal\xff".to_vec()),
        ("GIT_CONFIG_KEY_1", b"demo.value".to_vec()),
        ("GIT_CONFIG_VALUE_1", Vec::new()),
    ]
    .into();
    let config = Config::from_environment(|key| map.get(key).cloned(), 2).unwrap();
    assert_eq!(
        config.value("remote", Some(b"Ori\xffgin"), "url"),
        Some(Some(b"\\n # literal\xff".as_slice()))
    );
    assert_eq!(config.entries()[0].section, b"Remote");
    assert_eq!(config.entries()[0].name, b"URL");
    assert_eq!(
        config.value("demo", None, "value"),
        Some(Some(b"".as_slice()))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_include_path_and_subsection() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, b"[include]\npath=child-\xff\n").unwrap();
    std::fs::write(
        root.path().join(std::ffi::OsStr::from_bytes(b"child-\xff")),
        b"[remote \"n\xff\"]\nURL=v\xff\n[demo]\nvalue=bytes\n",
    )
    .unwrap();
    let config = Config::resolve(&inputs(&path)).unwrap();
    assert_eq!(
        config.value("remote", Some(b"n\xff"), "url"),
        Some(Some(b"v\xff".as_slice()))
    );
    assert_eq!(
        values(&config),
        observed(
            root.path(),
            &[
                "config",
                "--file",
                "config",
                "--includes",
                "--null",
                "--get-all",
                "demo.value"
            ],
            &[]
        )
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn linked_worktree_uses_private_gitdir_branch_and_config(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let initialized = Repository::init(format, &repo, InitKind::Worktree).unwrap();
    // Git creates the linked layout; an orphan worktree needs no commit fixture.
    observed(
        &repo,
        &[
            "worktree",
            "add",
            "--orphan",
            "-b",
            "topic/linked",
            "../linked",
        ],
        &[],
    );
    let local = initialized.common_dir().join("config");
    let mut bytes = std::fs::read(&local).unwrap();
    bytes.extend_from_slice(b"[extensions]\nworktreeConfig=true\n[demo]\nvalue=common\n[includeIf \"gitdir:**/worktrees/linked\"]\npath=conditional\n[includeIf \"onbranch:topic/\"]\npath=branch\n");
    std::fs::write(&local, bytes).unwrap();
    std::fs::write(
        initialized.common_dir().join("conditional"),
        b"[demo]\nvalue=gitdir\n",
    )
    .unwrap();
    std::fs::write(
        initialized.common_dir().join("branch"),
        b"[demo]\nvalue=branch\n",
    )
    .unwrap();
    std::fs::write(
        initialized
            .common_dir()
            .join("worktrees/linked/config.worktree"),
        b"[demo]\nvalue=private\n",
    )
    .unwrap();
    let linked = root.path().join("linked");
    let opened = Repository::open(&linked).unwrap();
    assert_eq!(opened.object_format(), format);
    assert_eq!(
        values(opened.config()),
        b"common\0gitdir\0branch\0private\0"
    );
    assert_eq!(
        values(opened.config()),
        observed(
            &linked,
            &["config", "--null", "--get-all", "demo.value"],
            &[]
        )
    );
    let last = opened
        .config()
        .entries()
        .last()
        .unwrap()
        .origin
        .as_ref()
        .unwrap();
    assert_eq!(last.scope, ConfigScope::Worktree);
    assert_eq!(
        last.location.path.as_deref(),
        Some(opened.git_dir().join("config.worktree").as_path())
    );
}

#[test]
fn explicit_environment_file_selection_replaces_defaults() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("system"), b"[demo]\nvalue=system\n").unwrap();
    std::fs::write(root.path().join("global"), b"[demo]\nvalue=global\n").unwrap();
    std::fs::write(root.path().join(".gitconfig"), b"malformed ignored home").unwrap();
    let map: BTreeMap<&str, OsString> = [
        ("HOME", root.path().into()),
        ("GIT_CONFIG_SYSTEM", root.path().join("system").into()),
        ("GIT_CONFIG_GLOBAL", root.path().join("global").into()),
    ]
    .into();
    let input = ConfigInputs::from_environment(
        Some(root.path().join("invalid-default")),
        |key| map.get(key).cloned(),
        10,
    )
    .unwrap();
    assert_eq!(
        values(&Config::resolve(&input).unwrap()),
        b"system\0global\0"
    );
    assert_eq!(
        observed(
            root.path(),
            &["config", "--null", "--get-all", "demo.value"],
            &[
                ("GIT_CONFIG_NOSYSTEM", "0"),
                ("GIT_CONFIG_SYSTEM", "system"),
                ("GIT_CONFIG_GLOBAL", "global")
            ]
        ),
        b"system\0global\0"
    );
    let input = ConfigInputs::from_environment(
        None,
        |key| {
            if key == "GIT_CONFIG_NOSYSTEM" {
                Some("true".into())
            } else {
                map.get(key).cloned()
            }
        },
        10,
    )
    .unwrap();
    assert_eq!(values(&Config::resolve(&input).unwrap()), b"global\0");
}

#[test]
fn xdg_precedes_home_and_absent_includes_are_optional() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("xdg/git")).unwrap();
    std::fs::write(root.path().join("xdg/git/config"), b"[demo]\nvalue=xdg\n").unwrap();
    std::fs::write(
        root.path().join(".gitconfig"),
        b"[demo]\nvalue=home\n[include]\npath=missing\n",
    )
    .unwrap();
    let map: BTreeMap<&str, OsString> = [
        ("HOME", root.path().into()),
        ("XDG_CONFIG_HOME", root.path().join("xdg").into()),
    ]
    .into();
    let input = ConfigInputs::from_environment(None, |key| map.get(key).cloned(), 0).unwrap();
    assert_eq!(values(&Config::resolve(&input).unwrap()), b"xdg\0home\0");
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    if let Some(system) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", system);
    }
    let result = command
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("xdg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["config", "--null", "--get-all", "demo.value"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"xdg\0home\0");
}

#[rstest]
#[case::quoted(b"[DEMO \"CaSe\"]\nVaLuE=bytes-\xff\n", b"CaSe".as_slice())]
#[case::deprecated(b"[DEMO.CaSe]\nVaLuE=bytes-\xff\n", b"case".as_slice())]
fn raw_spelling_and_subsection_case_match_git(#[case] bytes: &[u8], #[case] subsection: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, bytes).unwrap();
    let config = Config::resolve(&inputs(&path)).unwrap();
    assert_eq!(config.entries()[0].section, b"DEMO");
    assert_eq!(config.entries()[0].name, b"VaLuE");
    assert_eq!(
        config.value("demo", Some(subsection), "value"),
        Some(Some(b"bytes-\xff".as_slice()))
    );
    let key = format!("demo.{}.value", std::str::from_utf8(subsection).unwrap());
    assert_eq!(
        observed(
            root.path(),
            &["config", "--file", "config", "--null", "--get", &key],
            &[]
        ),
        b"bytes-\xff\0"
    );
}

#[test]
fn hasconfig_sees_environment_url_without_applying_included_urls() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(
        &path,
        b"[includeIf \"hasconfig:remote.*.url:match/**\"]\npath=child\n[demo]\nvalue=last\n",
    )
    .unwrap();
    std::fs::write(root.path().join("child"), b"[demo]\nvalue=first\n").unwrap();
    let mut input = inputs(&path);
    input.environment = Some(Config::parse(b"[remote \"r\"]\nurl=match/path\n").unwrap());
    assert_eq!(values(&Config::resolve(&input).unwrap()), b"first\0last\0");
    // Git's --file mode excludes runtime pairs; normal layered lookup includes them.
    assert_eq!(
        observed(
            root.path(),
            &["config", "--null", "--get-all", "demo.value"],
            &[
                ("GIT_CONFIG_GLOBAL", "config"),
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "remote.r.url"),
                ("GIT_CONFIG_VALUE_0", "match/path")
            ]
        ),
        b"first\0last\0"
    );
}

#[test]
fn worktree_layout_settings_do_not_reinterpret_object_format() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
    let local = repo.git_dir().join("config");
    let mut bytes = std::fs::read(&local).unwrap();
    bytes.extend_from_slice(b"[extensions]\nworktreeConfig=true\n");
    std::fs::write(local, bytes).unwrap();
    std::fs::write(
        repo.git_dir().join("config.worktree"),
        b"[core]\nbare=true\n[extensions]\nobjectFormat=sha256\n",
    )
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.worktree(), None);
    assert_eq!(repo.object_format(), ObjectFormat::Sha1);
    assert_eq!(
        observed(
            root.path(),
            &["rev-parse", "--is-bare-repository", "--show-object-format"],
            &[]
        ),
        b"true\nsha1\n"
    );
}

#[test]
fn empty_include_path_is_inert() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, b"[include]\npath=\n[demo]\nvalue=kept\n").unwrap();
    assert_eq!(values(&Config::resolve(&inputs(&path)).unwrap()), b"kept\0");
    assert_eq!(
        observed(
            root.path(),
            &[
                "config",
                "--file",
                "config",
                "--includes",
                "--null",
                "--get-all",
                "demo.value"
            ],
            &[]
        ),
        b"kept\0"
    );
}

#[test]
fn bootstrap_config_reads_obey_byte_budget() {
    let root = tempfile::tempdir().unwrap();
    Repository::init(ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
    let mut input = ConfigInputs::default();
    input.limits.bytes = 1;
    assert!(matches!(
        Repository::open_with_config(root.path(), &input),
        Err(girt::OpenError::Resolve(_))
    ));
}

#[cfg(unix)]
#[test]
fn repository_gitdir_conditions_match_logical_symlink_alias() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("physical");
    Repository::init(ObjectFormat::Sha1, &repo, InitKind::Worktree).unwrap();
    std::os::unix::fs::symlink(&repo, root.path().join("alias")).unwrap();
    let local = repo.join(".git/config");
    let mut bytes = std::fs::read(&local).unwrap();
    bytes.extend_from_slice(b"[includeIf \"gitdir:alias/\"]\npath=child\n");
    std::fs::write(local, bytes).unwrap();
    std::fs::write(repo.join(".git/child"), b"[demo]\nvalue=logical\n").unwrap();
    let opened = Repository::open(root.path().join("alias/.git")).unwrap();
    assert_eq!(values(opened.config()), b"logical\0");
    // Explicit GIT_DIR preserves the alias; Git's -C canonicalizes the working directory first.
    assert_eq!(
        observed(
            root.path(),
            &["config", "--null", "--get-all", "demo.value"],
            &[("GIT_DIR", "alias/.git")]
        ),
        b"logical\0"
    );
}

#[rstest]
#[case::plus("+1", b"v\0".as_slice())]
#[case::leading_space(" 1", b"v\0".as_slice())]
#[case::leading_zero("01", b"v\0".as_slice())]
fn environment_count_spellings_match_git(#[case] count: &str, #[case] expected: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let env = [
        ("GIT_CONFIG_COUNT", count),
        ("GIT_CONFIG_KEY_0", "demo.value"),
        ("GIT_CONFIG_VALUE_0", "v"),
    ];
    let config = Config::from_environment(
        |key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.as_bytes().to_vec())
        },
        10,
    )
    .unwrap();
    assert_eq!(values(&config), expected);
    assert_eq!(
        observed(
            root.path(),
            &["config", "--null", "--get-all", "demo.value"],
            &env
        ),
        expected
    );
}
