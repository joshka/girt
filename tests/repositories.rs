//! Independent Git CLI fixtures; no Git source or upstream fixtures are used.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{Config, ObjectId, OpenError, Repository};
use rstest::rstest;

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }

    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A. Writer")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0530")
        .env("GIT_COMMITTER_NAME", "C. Recorder")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000123 -0700")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git is required for interoperability tests");

    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    output.stdout
}

fn init(path: &Path, bare: bool) {
    let mode = if bare { "--bare" } else { "--quiet" };
    git(
        path,
        &["init", mode, "--object-format=sha1", "--template=", "."],
        b"",
    );
}
fn canonical(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap()
}
fn snapshot(path: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files.extend(snapshot(&entry.path()));
        } else {
            files.push((entry.path(), std::fs::read(entry.path()).unwrap()));
        }
    }
    files.sort();
    files
}

#[rstest]
#[case::ordinary(false, ".", Some("."))]
#[case::git_directory(false, ".git", Some("."))]
#[case::bare(true, ".", None)]
fn opens_and_reads_git_blob_without_mutation(
    #[case] bare: bool,
    #[case] input: &str,
    #[case] worktree: Option<&str>,
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), bare);
    let output = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"hello from Git\0\xff",
    );
    let id: ObjectId = std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let before = snapshot(root.path());
    let repo = Repository::open(root.path().join(input)).unwrap();
    assert_eq!(
        repo.worktree(),
        worktree.map(|p| canonical(&root.path().join(p))).as_deref()
    );
    assert_eq!(
        repo.loose_objects().read_blob(id, 100).unwrap(),
        b"hello from Git\0\xff"
    );
    assert_eq!(snapshot(root.path()), before);
}

#[rstest]
#[case::root("work")]
#[case::gitfile("work/.git")]
fn opens_separate_git_directory(#[case] input: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--template=",
            "--object-format=sha1",
            "--separate-git-dir=metadata",
            "work",
        ],
        b"",
    );
    // Rewrite to a relative indirection, independently supported by Git.
    std::fs::write(root.path().join("work/.git"), b"gitdir: ../metadata\n").unwrap();
    git(&root.path().join("work"), &["rev-parse", "--git-dir"], b"");
    let repo = Repository::open(root.path().join(input)).unwrap();
    assert_eq!(repo.git_dir(), canonical(&root.path().join("metadata")));
    assert_eq!(
        repo.worktree(),
        Some(canonical(&root.path().join("work")).as_path())
    );
}

#[rstest]
#[case::root("linked")]
#[case::gitfile("linked/.git")]
#[case::metadata(".git/worktrees/linked")]
fn opens_linked_worktree(#[case] input: &str) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), false);
    git(
        root.path(),
        &[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
        b"",
    );
    git(root.path(), &["worktree", "add", "--detach", "linked"], b"");
    let before = snapshot(root.path());
    let repo = Repository::open(root.path().join(input)).unwrap();
    assert_eq!(
        repo.git_dir(),
        canonical(&root.path().join(".git/worktrees/linked"))
    );
    assert_eq!(repo.common_dir(), canonical(&root.path().join(".git")));
    assert_eq!(
        repo.object_dir(),
        canonical(&root.path().join(".git/objects"))
    );
    assert_eq!(
        repo.worktree(),
        Some(canonical(&root.path().join("linked")).as_path())
    );
    assert_eq!(snapshot(root.path()), before);
}

#[rstest]
#[case::comments(
    b"[test]\nvalue = first # comment\nvalue = \" a ; # \" ; ignored\n",
    b"first\0 a ; # \0"
)]
#[case::escapes(b"[test]\nvalue = \"a\\n\\t\\b\\\\\\\"\"\n", b"a\n\t\x08\\\"\0")]
#[case::continuation(b"[test]\nvalue = first\\\n  second\n", b"first  second\0")]
#[case::crlf(b"\xef\xbb\xbf[TeST]\r\nVaLuE = one\r\n", b"one\0")]
#[case::whitespace(b"[test] value= a  b\t c  \n", b"a  b\t c\0")]
#[case::bytes(b"[test]\nvalue = \xff\n", b"\xff\0")]
#[case::empty_quote_prefix(b"[test]\nvalue = \"\"  value\n", b"value\0")]
#[case::continued_prefix(b"[test]\nvalue = \\\n  value\n", b"value\0")]
fn config_values_agree_with_git(#[case] bytes: &[u8], #[case] expected: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("config"), bytes).unwrap();
    let git_values = git(
        root.path(),
        &[
            "config",
            "--file=config",
            "--null",
            "--get-all",
            "test.value",
        ],
        b"",
    );
    let config = Config::parse(bytes).unwrap();
    let actual: Vec<u8> = config
        .values("test", None, "value")
        .flat_map(|value| value.unwrap().iter().copied().chain([0]))
        .collect();
    assert_eq!(git_values, expected);
    assert_eq!(actual, expected);
}

#[test]
fn git_generated_quoted_subsections_and_repeated_keys() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "config",
            "--file=config",
            "--add",
            "remote.Up.Stream.url",
            "first",
        ],
        b"",
    );
    git(
        root.path(),
        &[
            "config",
            "--file=config",
            "--add",
            "remote.Up.Stream.url",
            "last #\t\"",
        ],
        b"",
    );
    let bytes = std::fs::read(root.path().join("config")).unwrap();
    let config = Config::parse(&bytes).unwrap();
    assert_eq!(
        config.value("REMOTE", Some(b"Up.Stream"), "URL"),
        Some(Some(b"last #\t\"".as_slice()))
    );
    assert_eq!(
        config.values("remote", Some(b"Up.Stream"), "url").count(),
        2
    );
}

#[rstest]
#[case::unknown_extension("extensions.future", "true", "extensions.future")]
#[case::ref_storage("extensions.refStorage", "unknown", "extensions.refStorage")]
#[case::future_version("core.repositoryFormatVersion", "2", "repository format version 2")]
fn rejects_unsupported_without_mutation(
    #[case] key: &str,
    #[case] value: &str,
    #[case] feature: &str,
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    git(root.path(), &["config", "--file=config", key, value], b"");
    let before = snapshot(root.path());
    let error = Repository::open(root.path()).unwrap_err();
    assert!(matches!(&error, OpenError::Unsupported { .. }), "{error}");
    assert!(error.to_string().contains(feature), "{error}");
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn recognizes_sha256_loose_storage() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--object-format=sha256",
            ".",
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.object_format(), girt::ObjectFormat::Sha256);
    assert_eq!(
        repo.loose_objects().object_format(),
        girt::ObjectFormat::Sha256
    );
}

#[rstest]
#[case::invalid_boolean(b"[core]\nbare=nonsense\n")]
#[case::conflicting_worktree(b"[core]\nbare=true\nworktree=..\n")]
#[case::invalid_version(b"[core]\nrepositoryFormatVersion=banana\n")]
fn rejects_malformed_settings(#[case] bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::write(root.path().join("config"), bytes).unwrap();
    assert!(matches!(
        Repository::open(root.path()),
        Err(OpenError::Malformed { .. })
    ));
}

#[rstest]
#[case::alternates("objects/info/alternates")]
fn permits_alternate_metadata(#[case] path: &str) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::write(root.path().join(path), b"unsupported\n").unwrap();
    assert!(Repository::open(root.path()).is_ok());
}

#[test]
fn core_worktree_is_relative_to_git_directory() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::create_dir(root.path().join("work")).unwrap();
    git(root.path(), &["config", "core.bare", "false"], b"");
    git(root.path(), &["config", "core.worktree", "work"], b"");
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.worktree(),
        Some(canonical(&root.path().join("work")).as_path())
    );
    let git_root = git(root.path(), &["rev-parse", "--show-toplevel"], b"");
    assert_eq!(
        canonical(Path::new(std::str::from_utf8(&git_root).unwrap().trim())),
        repo.worktree().unwrap()
    );
}

#[test]
fn does_not_search_parent_directories() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), false);
    std::fs::create_dir(root.path().join("child")).unwrap();
    assert!(matches!(
        Repository::open(root.path().join("child")),
        Err(OpenError::NotFound(_))
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn preserves_non_utf8_metadata_paths() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    let name = std::ffi::OsStr::from_bytes(b"work-\xff");
    std::fs::create_dir(root.path().join(name)).unwrap();
    std::fs::write(
        root.path().join("config"),
        b"[core]\nbare=false\nworktree=work-\xff\n",
    )
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(
        repo.worktree(),
        Some(canonical(&root.path().join(name)).as_path())
    );
}

#[rstest]
#[case::implicit(b"[core]\nbare\n", true)]
#[case::empty(b"[core]\nbare=\n", false)]
#[case::negative(b"[core]\nbare=-1\n", true)]
#[case::suffix(b"[core]\nbare=1k\n", true)]
#[case::octal(b"[core]\nbare=00\n", false)]
#[case::hex(b"[core]\nbare=0x1\n", true)]
#[case::case(b"[CORE]\nBARE=oFf\n", false)]
#[case::repeat(b"[core]\nbare=true\nbare=false\n", false)]
fn bare_config_agrees_with_git(#[case] bytes: &[u8], #[case] bare: bool) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), false);
    std::fs::write(root.path().join(".git/config"), bytes).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let git_value = git(
        root.path(),
        &["config", "--type=bool", "--get", "core.bare"],
        b"",
    );
    assert_eq!(git_value, format!("{bare}\n").as_bytes());
    assert_eq!(repo.worktree().is_none(), bare);
}

#[test]
fn format_one_sha1_and_last_version() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::write(root.path().join("config"), b"[core]\nrepositoryFormatVersion=0\nrepositoryFormatVersion=0x1\nbare=true\n[extensions]\nobjectFormat=sha1\n").unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.format_version(), 1);
    assert_eq!(
        git(root.path(), &["rev-parse", "--show-object-format"], b""),
        b"sha1\n"
    );
}

#[rstest]
#[case::head("HEAD", b"not a head\n")]
#[case::commondir("commondir", b"\n")]
fn malformed_metadata_is_distinct(#[case] path: &str, #[case] bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::write(root.path().join(path), bytes).unwrap();
    let before = snapshot(root.path());
    assert!(matches!(
        Repository::open(root.path()),
        Err(OpenError::Malformed { .. })
    ));
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn malformed_gitfile_is_distinct() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".git"), b"nonsense\n").unwrap();
    assert!(matches!(
        Repository::open(root.path()),
        Err(OpenError::Malformed { .. })
    ));
}

#[test]
fn opening_example_reads_git_fixture() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    let output = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"example bytes",
    );
    let id = std::str::from_utf8(&output).unwrap().trim();
    std::fs::write(
        root.path().join("hostile-config"),
        b"[include]\npath=absent\n",
    )
    .unwrap();
    let output = Command::new("cargo")
        .env("GIT_DIR", root.path().join("absent"))
        .env("GIT_COMMON_DIR", root.path().join("absent"))
        .env("GIT_WORK_TREE", root.path().join("absent"))
        .env("GIT_OBJECT_DIRECTORY", root.path().join("absent"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.repositoryformatversion")
        .env("GIT_CONFIG_VALUE_0", "999")
        .env("GIT_CONFIG_GLOBAL", root.path().join("hostile-config"))
        .args(["run", "--quiet", "--example", "open_repository", "--"])
        .arg(root.path())
        .arg(id)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Read 13 bytes from "));
}

#[test]
fn rejects_broken_linked_worktree_backlink() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), false);
    git(
        root.path(),
        &[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
        b"",
    );
    git(root.path(), &["worktree", "add", "--detach", "linked"], b"");
    std::fs::write(
        root.path().join(".git/worktrees/linked/gitdir"),
        b"relative/.git\n",
    )
    .unwrap();
    let before = snapshot(root.path());
    let error = Repository::open(root.path().join("linked")).unwrap_err();
    assert!(matches!(&error, OpenError::Malformed { .. }));
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn stale_worktree_config_is_not_read_without_extension() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::write(
        root.path().join("config.worktree"),
        b"[include]\npath=absent\n",
    )
    .unwrap();
    assert!(Repository::open(root.path()).is_ok());
}

#[test]
fn missing_config_uses_layout_defaults() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    std::fs::remove_file(root.path().join("config")).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    assert_eq!(repo.format_version(), 0);
    assert_eq!(repo.worktree(), None);
}

#[rstest]
#[case::bad_escape(b"[core]\nx=\\q\n")]
#[case::double_cr_escape(b"[core]\nx=a\\\r\r\nb\n")]
#[case::unclosed_quote(b"[core]\nx=\"value\n")]
#[case::missing_subsection_space(b"[core\"sub\"]\nx=value\n")]
fn malformed_syntax_is_rejected_by_git_and_girt(#[case] bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, bytes).unwrap();
    let output = invalid_config_output(root.path(), &path);
    assert!(!output.status.success());
    assert!(Config::parse(bytes).is_err());
}

#[test]
fn linked_worktree_of_bare_repository_has_worktree() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), true);
    let tree = git(root.path(), &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let commit = git(root.path(), &["commit-tree", tree], b"fixture\n");
    let commit = std::str::from_utf8(&commit).unwrap().trim();
    git(root.path(), &["update-ref", "HEAD", commit], b"");
    git(root.path(), &["worktree", "add", "--detach", "linked"], b"");
    let repo = Repository::open(root.path().join("linked")).unwrap();
    assert_eq!(repo.common_dir(), canonical(root.path()));
    assert_eq!(
        repo.worktree(),
        Some(canonical(&root.path().join("linked")).as_path())
    );
}

fn invalid_config_output(root: &Path, path: &Path) -> std::process::Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent"))
        .args(["config", "--file"])
        .arg(path)
        .arg("--list")
        .output()
        .unwrap()
}

#[rstest]
#[case::worktree(girt::InitKind::Worktree, b"false\n")]
#[case::bare(girt::InitKind::Bare, b"true\n")]
fn git_uses_girt_initialized_repository(#[case] kind: girt::InitKind, #[case] bare: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("repo");
    let repo = Repository::init(girt::ObjectFormat::Sha1, &path, kind).unwrap();
    assert_eq!(
        git(&path, &["rev-parse", "--is-bare-repository"], b""),
        bare
    );
    assert_eq!(
        git(&path, &["rev-parse", "--show-object-format"], b""),
        b"sha1\n"
    );
    assert_eq!(
        git(&path, &["symbolic-ref", "HEAD"], b""),
        b"refs/heads/main\n"
    );
    let blob = repo.loose_objects().write_blob(b"from girt\n").unwrap();
    assert_eq!(
        git(&path, &["cat-file", "blob", &blob.to_string()], b""),
        b"from girt\n"
    );
    let tree = git(
        &path,
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let commit = git(&path, &["commit-tree", tree], b"initial commit\n");
    let commit = std::str::from_utf8(&commit).unwrap().trim();
    git(&path, &["update-ref", "HEAD", commit], b"");
    assert_eq!(git(&path, &["show", "HEAD:file"], b""), b"from girt\n");
    git(&path, &["fsck", "--strict"], b"");
    let before = snapshot(&path);
    assert!(matches!(
        Repository::init(girt::ObjectFormat::Sha1, &path, kind),
        Err(girt::InitError::AlreadyExists(_))
    ));
    assert_eq!(snapshot(&path), before);
}

#[test]
fn git_can_add_and_commit_in_initialized_worktree() {
    let root = tempfile::tempdir().unwrap();
    Repository::init(
        girt::ObjectFormat::Sha1,
        root.path(),
        girt::InitKind::Worktree,
    )
    .unwrap();
    std::fs::write(root.path().join("file"), b"worktree bytes\n").unwrap();
    git(root.path(), &["add", "file"], b"");
    git(
        root.path(),
        &["-c", "commit.gpgSign=false", "commit", "-m", "fixture"],
        b"",
    );
    assert_eq!(
        git(root.path(), &["show", "HEAD:file"], b""),
        b"worktree bytes\n"
    );
    git(root.path(), &["fsck", "--strict"], b"");
}

#[rstest]
#[case::ordinary(false)]
#[case::bare(true)]
fn discovers_nested_git_created_repository(#[case] bare: bool) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), bare);
    let nested = root.path().join("one/two");
    std::fs::create_dir_all(&nested).unwrap();
    let expected = Repository::open(root.path()).unwrap();
    let before = snapshot(root.path());
    let discovered = Repository::discover_with_ceiling(&nested, root.path()).unwrap();
    assert_eq!(discovered.git_dir(), expected.git_dir());
    assert_eq!(discovered.worktree(), expected.worktree());
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn discovers_separate_metadata_outside_ceiling() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--template=",
            "--object-format=sha1",
            "--separate-git-dir=metadata",
            "work",
        ],
        b"",
    );
    let work = root.path().join("work");
    std::fs::write(work.join(".git"), b"gitdir: ../metadata\n").unwrap();
    std::fs::create_dir(work.join("nested")).unwrap();
    let before = snapshot(root.path());
    let repo = Repository::discover_with_ceiling(work.join("nested"), &work).unwrap();
    assert_eq!(repo.git_dir(), canonical(&root.path().join("metadata")));
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn discovers_linked_worktree_before_parent_repository() {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), false);
    git(
        root.path(),
        &[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
        b"",
    );
    git(root.path(), &["worktree", "add", "--detach", "linked"], b"");
    std::fs::create_dir(root.path().join("linked/nested")).unwrap();
    let before = snapshot(root.path());
    let repo = Repository::discover(root.path().join("linked/nested")).unwrap();
    assert_eq!(
        repo.git_dir(),
        canonical(&root.path().join(".git/worktrees/linked"))
    );
    assert_eq!(repo.common_dir(), canonical(&root.path().join(".git")));
    assert!(matches!(
        Repository::init(
            girt::ObjectFormat::Sha1,
            root.path().join("linked"),
            girt::InitKind::Worktree
        ),
        Err(girt::InitError::AlreadyExists(_))
    ));
    assert_eq!(snapshot(root.path()), before);
}

#[rstest]
#[case::unknown_backend("sha1", Some(("extensions.refStorage", "unknown")))]
fn discovery_and_initialization_reject_unsupported_without_changes(
    #[case] format: &str,
    #[case] extension: Option<(&str, &str)>,
) {
    let root = tempfile::tempdir().unwrap();
    init(root.path(), false);
    let inner = root.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    unsupported_repository(&inner, format, extension);
    std::fs::create_dir(inner.join("nested")).unwrap();
    let before = snapshot(root.path());
    assert!(matches!(
        Repository::discover(inner.join("nested")),
        Err(OpenError::Unsupported { .. })
    ));
    assert!(matches!(
        Repository::init(girt::ObjectFormat::Sha1, &inner, girt::InitKind::Worktree),
        Err(girt::InitError::Open(OpenError::Unsupported { .. }))
    ));
    assert_eq!(snapshot(root.path()), before);
}

fn unsupported_repository(path: &Path, format: &str, extension: Option<(&str, &str)>) {
    git(
        path,
        &[
            "init",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    if let Some((key, value)) = extension {
        git(path, &["config", key, value], b"");
    }
}
