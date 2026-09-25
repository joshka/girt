//! Original disposable fixtures observe Git's CLI; no upstream source/test input.
use std::path::Path;
use std::process::{Command, Output};

use girt::config::{ConfigEdit, ConfigFile, ConfigInputs, ConfigScope, Document, EditError};
use girt::remote::{Remote, RemoteConfig, RemoteEditError, RemoteKey};
use girt::{Config, ObjectFormat, Repository};
use rstest::rstest;

fn attempt(path: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-global"))
        .args(args)
        .output()
        .unwrap()
}
fn git(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = attempt(path, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn init(format: ObjectFormat) -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::remove_dir(dir.path()).unwrap();
    let repo = Repository::init(format, dir.path(), girt::InitKind::Bare).unwrap();
    (dir, repo)
}
type EntryValue = (Vec<u8>, Option<Vec<u8>>, Vec<u8>, Option<Vec<u8>>);
fn entries(bytes: &[u8]) -> Vec<EntryValue> {
    let config = Config::parse(bytes).unwrap();
    let mut entries: Vec<_> = config
        .entries()
        .iter()
        .map(|e| {
            (
                e.section.to_ascii_lowercase(),
                e.subsection.clone(),
                e.name.to_ascii_lowercase(),
                e.value.clone(),
            )
        })
        .collect();
    // Ignore physical section placement; stable sorting preserves each key's occurrence order.
    entries.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    entries
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn add_set_append_remove_url_agree_with_git(#[case] format: ObjectFormat) {
    let (dir, repo) = init(format);
    let original = std::fs::read(dir.path().join("config")).unwrap();
    git(dir.path(), &["remote", "add", "origin", "first"]);
    git(
        dir.path(),
        &["remote", "set-url", "--add", "origin", "second"],
    );
    git(
        dir.path(),
        &["remote", "set-url", "origin", "replacement", "^first$"],
    );
    git(
        dir.path(),
        &["remote", "set-url", "--push", "--add", "origin", "push"],
    );
    let expected = std::fs::read(dir.path().join("config")).unwrap();
    std::fs::write(dir.path().join("config"), original).unwrap();
    let mut guard = repo.edit_config(4096).unwrap();
    let mut edit = RemoteConfig::new(guard.document_mut());
    edit.add(
        b"origin",
        b"first",
        &[b"+refs/heads/*:refs/remotes/origin/*"],
    )
    .unwrap();
    edit.append(b"origin", RemoteKey::Url, b"second").unwrap();
    edit.set(b"origin", RemoteKey::Url, 0, b"replacement")
        .unwrap();
    edit.append(b"origin", RemoteKey::PushUrl, b"push").unwrap();
    assert_eq!(entries(guard.document().as_bytes()), entries(&expected));
    guard.commit().unwrap();
    assert_eq!(
        git(dir.path(), &["remote", "get-url", "--all", "origin"]),
        b"replacement\nsecond\n"
    );
    assert_eq!(
        git(
            dir.path(),
            &["remote", "get-url", "--push", "--all", "origin"]
        ),
        b"push\n"
    );
    let mut guard = repo.edit_config(4096).unwrap();
    RemoteConfig::new(guard.document_mut())
        .remove_value(b"origin", RemoteKey::PushUrl, 0)
        .unwrap();
    guard.commit().unwrap();
    assert_eq!(
        git(
            dir.path(),
            &["remote", "get-url", "--push", "--all", "origin"]
        ),
        b"replacement\nsecond\n"
    );
    assert!(Remote::find(repo.config(), b"origin").unwrap().is_none());
    let refreshed = Repository::open(dir.path()).unwrap();
    assert_eq!(
        Remote::find(refreshed.config(), b"origin")
            .unwrap()
            .unwrap()
            .urls()
            .len(),
        2
    );
}

const REMOTE: &[u8] = b"# keep top\n[remote \"old\"]\nurl=one\nfetch=+refs/heads/*:refs/remotes/old/*\nfetch=refs/heads/x:refs/custom/old\npush=refs/heads/*:refs/remotes/old/*\n[branch \"main\"]\nremote=old\npushRemote=old\nmerge=refs/heads/main\nrebase=true # preserve\n[remote]\npushDefault=old\n[other]\nkeep = exact  # untouched\n";
fn rename_old_remote(document: &mut Document) -> Result<(), RemoteEditError> {
    RemoteConfig::new(document).rename(b"old", b"new")
}
fn remove_old_remote(document: &mut Document) -> Result<(), RemoteEditError> {
    RemoteConfig::new(document).remove(b"old")
}

#[rstest]
#[case::rename(&["remote", "rename", "old", "new"], rename_old_remote)]
#[case::remove(&["remote", "remove", "old"], remove_old_remote)]
fn branch_and_refspec_changes_agree_with_git(
    #[case] command: &[&str],
    #[case] operation: fn(&mut Document) -> Result<(), RemoteEditError>,
) {
    let (dir, _) = init(ObjectFormat::Sha1);
    let path = dir.path().join("config");
    let mut original = std::fs::read(&path).unwrap();
    original.extend_from_slice(REMOTE);
    std::fs::write(&path, &original).unwrap();
    git(dir.path(), command);
    let expected = std::fs::read(&path).unwrap();
    let mut document = Document::parse(&original).unwrap();
    operation(&mut document).unwrap();
    assert_eq!(entries(document.as_bytes()), entries(&expected));
    assert!(
        document
            .as_bytes()
            .ends_with(b"[other]\nkeep = exact  # untouched\n")
    );
    assert!(document.as_bytes().windows(10).any(|s| s == b"# preserve"));
}

#[test]
fn includes_globals_and_inherited_only_remotes_are_never_rewritten() {
    let (dir, _) = init(ObjectFormat::Sha1);
    let included = dir.path().join("included");
    let global = dir.path().join("global");
    std::fs::write(
        &included,
        b"[remote \"inherited\"]\nurl=included\n[remote \"mixed\"]\nurl=parent\n",
    )
    .unwrap();
    std::fs::write(&global, b"[remote \"global\"]\nurl=global\n").unwrap();
    let path = dir.path().join("config");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"[include]\npath=included\n[remote \"mixed\"]\nurl=local\n");
    std::fs::write(&path, &bytes).unwrap();
    let inputs = ConfigInputs {
        files: vec![ConfigFile {
            path: global.clone(),
            scope: ConfigScope::Global,
            optional: false,
        }],
        ..Default::default()
    };
    let repo = Repository::open_with_config(dir.path(), &inputs).unwrap();
    let mut guard = repo.edit_config(4096).unwrap();
    assert!(matches!(
        RemoteConfig::new(guard.document_mut()).remove(b"inherited"),
        Err(RemoteEditError::NotLocal)
    ));
    RemoteConfig::new(guard.document_mut())
        .remove(b"mixed")
        .unwrap();
    guard.commit().unwrap();
    let fresh = Repository::open_with_config(dir.path(), &inputs).unwrap();
    assert_eq!(
        Remote::find(fresh.config(), b"mixed")
            .unwrap()
            .unwrap()
            .fetch_url(),
        Some(b"parent".as_slice())
    );
    assert_eq!(
        std::fs::read(included).unwrap(),
        b"[remote \"inherited\"]\nurl=included\n[remote \"mixed\"]\nurl=parent\n"
    );
    assert_eq!(
        std::fs::read(global).unwrap(),
        b"[remote \"global\"]\nurl=global\n"
    );
    assert_eq!(
        git(dir.path(), &["remote", "get-url", "mixed"]),
        b"parent\n"
    );
    assert!(
        !attempt(dir.path(), &["remote", "remove", "inherited"])
            .status
            .success()
    );
}

#[rstest]
#[case::deprecated(b"[remote.OLD] url=old ;keep\r\n", b"old")]
#[case::bytes(
    b"\xef\xbb\xbf[remote \"a\\\"\\\\\xff\"]\nurl=first\\\nsecond #keep\n",
    b"a\"\\\xff"
)]
fn git_reads_lossless_unusual_syntax(#[case] source: &[u8], #[case] name: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let mut document = Document::parse(source).unwrap();
    RemoteConfig::new(&mut document)
        .set(name, RemoteKey::Url, 0, b" a\"\\\n\t#;\xff ")
        .unwrap();
    std::fs::write(dir.path().join("config"), document.as_bytes()).unwrap();
    let output = git(
        dir.path(),
        &["config", "--file", "config", "--null", "--list"],
    );
    let mut expected = b"remote.".to_vec();
    expected.extend_from_slice(name);
    expected.extend_from_slice(b".url\n a\"\\\n\t#;\xff \0");
    assert_eq!(output, expected);
    assert!(
        document
            .as_bytes()
            .windows(5)
            .any(|b| b == b";keep" || b == b"#keep")
    );
}

#[test]
fn competing_threads_cannot_overwrite_from_stale_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    let first = ConfigEdit::open(&path, 1024).unwrap();
    let other_path = path.clone();
    let result = std::thread::spawn(move || ConfigEdit::open(other_path, 1024))
        .join()
        .unwrap();
    assert!(matches!(result, Err(EditError::Locked(_))));
    first.commit().unwrap();
    let second = ConfigEdit::open(&path, 1024).unwrap();
    assert!(matches!(
        second.require_source(None),
        Err(EditError::Changed(_))
    ));
    second.abort().unwrap();
}

#[test]
fn repeated_fetch_push_and_reset_values_remain_ordered() {
    let (dir, repo) = init(ObjectFormat::Sha1);
    git(dir.path(), &["remote", "add", "o", "old"]);
    git(
        dir.path(),
        &["config", "--add", "remote.o.fetch", "^refs/heads/private/*"],
    );
    git(
        dir.path(),
        &[
            "config",
            "--add",
            "remote.o.push",
            "refs/heads/a:refs/heads/b",
        ],
    );
    git(
        dir.path(),
        &[
            "config",
            "--add",
            "remote.o.push",
            "refs/heads/c:refs/heads/d",
        ],
    );
    let mut guard = repo.edit_config(4096).unwrap();
    let mut edit = RemoteConfig::new(guard.document_mut());
    edit.set(b"o", RemoteKey::Push, 0, b"refs/heads/a:refs/heads/new")
        .unwrap();
    edit.append(b"o", RemoteKey::Url, b"").unwrap();
    edit.append(b"o", RemoteKey::Url, b"new").unwrap();
    edit.append(b"o", RemoteKey::PushUrl, b"old-push").unwrap();
    edit.append(b"o", RemoteKey::PushUrl, b"").unwrap();
    guard.commit().unwrap();
    assert_eq!(
        git(dir.path(), &["config", "--get-all", "remote.o.fetch"]),
        b"+refs/heads/*:refs/remotes/o/*\n^refs/heads/private/*\n"
    );
    assert_eq!(
        git(dir.path(), &["config", "--get-all", "remote.o.push"]),
        b"refs/heads/a:refs/heads/new\nrefs/heads/c:refs/heads/d\n"
    );
    assert_eq!(git(dir.path(), &["remote", "get-url", "o"]), b"new\n");
    assert_eq!(
        git(dir.path(), &["remote", "get-url", "--push", "--all", "o"]),
        b"new\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn explicit_file_path_retains_os_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(std::ffi::OsStr::from_bytes(b"config-\xff"));
    let mut guard = ConfigEdit::open(&path, 4096).unwrap();
    guard
        .document_mut()
        .append("core", None, "x", b"yes")
        .unwrap();
    guard.commit().unwrap();
    assert_eq!(
        Config::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .value("core", None, "x"),
        Some(Some(b"yes".as_slice()))
    );
}

#[test]
fn explicit_unicode_file_path_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config-資料");
    let mut guard = ConfigEdit::open(&path, 4096).unwrap();
    guard
        .document_mut()
        .append("core", None, "x", b"yes")
        .unwrap();
    guard.commit().unwrap();
    assert_eq!(
        Config::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .value("core", None, "x"),
        Some(Some(b"yes".as_slice()))
    );
}

#[rstest]
#[case::trailing(b"[x]\nk=a # \0 hi\nnext=b\n", b"[x]\nk=\"changed\" # \0 hi\nnext=b\n")]
#[case::standalone(b"[x]\nk=a\n; \0\nnext=b\n", b"[x]\nk=\"changed\"\n; \0\nnext=b\n")]
#[case::header(b"[x] # \0\nk=a\nnext=b\n", b"[x] # \0\nk=\"changed\"\nnext=b\n")]
#[case::crlf(
    b"[x]\r\nk=a ; \0\r\nnext=b\r\n",
    b"[x]\r\nk=\"changed\" ; \0\r\nnext=b\r\n"
)]
fn inert_nul_comments_survive_resolution_and_edit(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] original: &[u8],
    #[case] edited: &[u8],
) {
    let (dir, repo) = init(format);
    let path = dir.path().join("config");
    let mut bytes = std::fs::read(&path).unwrap();
    let prefix = bytes.len();
    bytes.extend_from_slice(original);
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(git(dir.path(), &["config", "--get", "x.k"]), b"a\n");
    let reopened = Repository::open(dir.path()).unwrap();
    let inputs = ConfigInputs {
        files: vec![ConfigFile {
            path: path.clone(),
            scope: ConfigScope::Local,
            optional: false,
        }],
        ..Default::default()
    };
    assert_eq!(
        Config::resolve(&inputs).unwrap().value("x", None, "next"),
        Some(Some(b"b".as_slice()))
    );
    let mut document = Document::parse(&bytes).unwrap();
    assert_eq!(document.as_bytes(), bytes);
    let occurrence = document
        .config()
        .entries()
        .iter()
        .position(|e| e.section == b"x" && e.name == b"k")
        .unwrap();
    document.set_value(occurrence, b"changed").unwrap();
    let mut expected = bytes[..prefix].to_vec();
    expected.extend_from_slice(edited);
    assert_eq!(document.as_bytes(), expected);
    let mut edit = reopened.edit_config(4096).unwrap();
    edit.document_mut()
        .set_value(occurrence, b"changed")
        .unwrap();
    edit.commit().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    assert_eq!(git(dir.path(), &["config", "--get", "x.k"]), b"changed\n");
    assert_eq!(git(dir.path(), &["config", "--get", "x.next"]), b"b\n");
    assert_eq!(repo.object_format(), reopened.object_format());
}
