//! Revision and range resolution compared with `git rev-parse` and `git rev-list` on repositories
//! written by Git.

use std::path::Path;
use std::process::Command;

#[path = "support/short_ids_git.rs"]
mod short_ids_git;

use girt::revision::{Range, Resolver, Revision, RevisionError};
use girt::{HistoryLimits, ObjectId, PackLimits, Repository};

fn git(directory: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Rev Author")
        .env("GIT_AUTHOR_EMAIL", "rev@example.com")
        .env("GIT_COMMITTER_NAME", "Rev Author")
        .env("GIT_COMMITTER_EMAIL", "rev@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .output()
        .expect("git is required for revision parity tests");
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned())
}

fn must(directory: &Path, args: &[&str]) -> String {
    git(directory, args).unwrap_or_else(|| panic!("git {args:?} failed"))
}

/// A history with a merge, annotated tags, nested paths, a remote-tracking upstream, a local
/// upstream and several reflog entries.
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    must(path, &["init", "-q", "-b", "main", "."]);
    let commit = |name: &str, contents: &str| {
        std::fs::create_dir_all(path.join("dir/sub")).unwrap();
        std::fs::write(path.join("dir/sub").join(name), contents).unwrap();
        must(path, &["add", "-A"]);
        must(path, &["commit", "-q", "-m", name]);
    };
    commit("a", "one");
    commit("b", "two");
    must(path, &["tag", "-a", "-m", "release", "v1", "HEAD"]);
    must(path, &["tag", "-a", "-m", "nested", "v1-outer", "v1"]);
    must(path, &["tag", "light", "HEAD~1"]);
    must(path, &["checkout", "-q", "-b", "topic", "HEAD~1"]);
    commit("t", "topic");
    commit("u", "topic two");
    must(path, &["checkout", "-q", "main"]);
    commit("c", "three");
    must(
        path,
        &["merge", "-q", "--no-ff", "-m", "merge topic", "topic"],
    );
    commit("d", "four");
    // A remote-tracking upstream written directly, as a fetch would leave it.
    let upstream = must(path, &["rev-parse", "HEAD~2"]);
    must(path, &["update-ref", "refs/remotes/origin/main", &upstream]);
    must(
        path,
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    must(path, &["config", "branch.main.remote", "origin"]);
    must(path, &["config", "branch.main.merge", "refs/heads/main"]);
    must(path, &["config", "branch.topic.remote", "."]);
    must(path, &["config", "branch.topic.merge", "refs/heads/main"]);
    // A name that is both a tag and a branch: tags win.
    must(path, &["branch", "both", "HEAD~1"]);
    must(path, &["tag", "both", "topic"]);
    must(path, &["update-ref", "ORIG_HEAD", "topic"]);
    root
}

// Boxed: the error is larger on Windows, where clippy's `result_large_err` applies.
fn resolve(repository: &Repository, text: &str) -> Result<ObjectId, Box<RevisionError>> {
    let objects = repository.objects(PackLimits::default()).unwrap();
    let resolver = Resolver::new(repository, &objects)?;
    let revision = text.parse::<Revision>().map_err(RevisionError::from)?;
    Ok(resolver.resolve(&revision)?)
}

#[test]
fn single_revisions_match_git_rev_parse() {
    let root = fixture();
    let path = root.path();
    let repository = Repository::discover(path).unwrap();
    let head = must(path, &["rev-parse", "HEAD"]);
    for text in [
        "HEAD",
        "@",
        head.as_str(),
        "main",
        "refs/heads/main",
        "heads/main",
        "topic",
        "origin/main",
        "origin",
        "v1",
        "v1-outer",
        "tags/v1",
        "light",
        "both",
        "HEAD~",
        "HEAD~0",
        "HEAD^0",
        "HEAD~2",
        "HEAD^",
        "HEAD^^2",
        "HEAD~1^2~1",
        "@~3",
        "v1^{}",
        "v1-outer^{}",
        "v1-outer^{tag}",
        "v1^{commit}",
        "v1-outer~1",
        "v1^{tree}",
        "HEAD^{tree}",
        "HEAD^{object}",
        "HEAD:",
        "HEAD:dir",
        "HEAD:dir/sub/a",
        "HEAD:dir//sub/",
        "HEAD:dir/sub/",
        "HEAD:dir/sub/a/",
        "HEAD:/",
        "HEAD:/dir",
        "HEAD:dir/./sub",
        "v1:dir/sub/b",
        "HEAD~1^2:dir/sub/t",
        "HEAD:dir/sub/a^{blob}",
        "HEAD@{0}",
        "HEAD@{1}",
        "HEAD@{3}",
        "main@{0}",
        "main@{2}",
        "@{1}",
        "@{1}~1",
        "@{u}",
        "@{upstream}",
        "main@{U}",
        "topic@{u}",
        "@{push}",
        // Failures Git and girt must agree on.
        "missing",
        "HEAD~20",
        "HEAD^3",
        "HEAD:nope",
        "HEAD:dir/sub/a/deeper",
        "HEAD^{blob}",
        "v1^{tag}^{tree}^{tag}",
        "main@{99}",
        "both@{u}",
    ] {
        let expected = git(path, &["rev-parse", "--verify", "-q", text]);
        let actual = resolve(&repository, text);
        match (&expected, &actual) {
            (Some(expected), Ok(actual)) => {
                assert_eq!(&actual.to_string(), expected, "{text}");
            }
            (None, Err(error)) => assert!(
                !matches!(
                    **error,
                    RevisionError::Unsupported(_) | RevisionError::Parse(_)
                ),
                "{text}: {error}"
            ),
            _ => panic!("{text}: git {expected:?}, girt {actual:?}"),
        }
    }
}

#[test]
fn detached_head_reflog_matches_git() {
    let root = fixture();
    let path = root.path();
    must(path, &["checkout", "-q", "--detach", "HEAD~1"]);
    let repository = Repository::discover(path).unwrap();
    for text in ["@{1}", "@{0}", "HEAD@{2}", "@{u}"] {
        let expected = git(path, &["rev-parse", "--verify", "-q", text]);
        let actual = resolve(&repository, text).ok().map(|id| id.to_string());
        assert_eq!(actual, expected, "{text}");
    }
}

#[test]
fn push_destination_defaults_to_the_only_remote() {
    let root = fixture();
    let path = root.path();
    must(path, &["config", "--unset", "branch.main.remote"]);
    must(path, &["config", "push.default", "current"]);
    let repository = Repository::discover(path).unwrap();
    let expected = git(path, &["rev-parse", "--verify", "-q", "@{push}"]);
    assert!(expected.is_some());
    let actual = resolve(&repository, "@{push}")
        .ok()
        .map(|id| id.to_string());
    assert_eq!(actual, expected);
}

#[test]
fn resolve_to_tag_keeps_annotated_tags() {
    let root = fixture();
    let repository = Repository::discover(root.path()).unwrap();
    let objects = repository.objects(PackLimits::default()).unwrap();
    let resolver = Resolver::new(&repository, &objects).unwrap();
    let tag = resolver
        .resolve_to(&"v1".parse().unwrap(), girt::ObjectKind::Tag)
        .unwrap();
    assert_eq!(tag.to_string(), must(root.path(), &["rev-parse", "v1"]));
    assert!(
        resolver
            .resolve_to(&"light".parse().unwrap(), girt::ObjectKind::Tag)
            .is_err()
    );
}

#[test]
fn push_destination_follows_push_default() {
    let root = fixture();
    let path = root.path();
    must(path, &["update-ref", "refs/remotes/fork/main", "topic"]);
    must(
        path,
        &["remote", "add", "fork", "https://example.invalid/f.git"],
    );
    for (key, value) in [
        ("remote.pushDefault", "fork"),
        ("push.default", "current"),
        ("push.default", "upstream"),
        ("push.default", "nothing"),
        ("push.default", "simple"),
        ("remote.pushDefault", "origin"),
        ("branch.main.merge", "refs/heads/other"),
    ] {
        must(path, &["config", key, value]);
        let repository = Repository::discover(path).unwrap();
        for text in ["@{push}", "main@{push}"] {
            let expected = git(path, &["rev-parse", "--verify", "-q", text]);
            let actual = resolve(&repository, text).ok().map(|id| id.to_string());
            assert_eq!(actual, expected, "{key}={value} {text}");
        }
    }
}

#[test]
fn ranges_match_git_rev_list_counts() {
    let root = fixture();
    let path = root.path();
    let repository = Repository::discover(path).unwrap();
    let objects = repository.objects(PackLimits::default()).unwrap();
    let resolver = Resolver::new(&repository, &objects).unwrap();
    for args in [
        &["HEAD"][..],
        &["topic..main"],
        &["main..topic"],
        &["main...topic"],
        &["topic...main"],
        &["..topic"],
        &["topic.."],
        &["main", "^topic"],
        &["main", "topic", "^v1"],
        &["v1-outer..HEAD"],
    ] {
        let ranges: Vec<Range> = args.iter().map(|arg| arg.parse().unwrap()).collect();
        let selection = resolver.select(&ranges).unwrap();
        let excluded: std::collections::HashSet<_> = objects
            .walk(&selection.exclude, HistoryLimits::trusted())
            .unwrap()
            .into_iter()
            .collect();
        let count = objects
            .walk(&selection.include, HistoryLimits::trusted())
            .unwrap()
            .into_iter()
            .filter(|id| !excluded.contains(id))
            .count();
        let mut git_args = vec!["rev-list", "--count"];
        git_args.extend_from_slice(args);
        assert_eq!(count.to_string(), must(path, &git_args), "{args:?}");
    }
}

#[test]
fn unsupported_forms_are_refused_explicitly() {
    let root = fixture();
    let repository = Repository::discover(root.path()).unwrap();
    let error = resolve(&repository, "ORIG_HEAD").unwrap_err();
    assert!(matches!(*error, RevisionError::Unsupported(_)), "{error}");
}

#[test]
fn abbreviated_ids_match_git_rev_parse() {
    let root = fixture();
    let path = root.path();
    let repository = Repository::discover(path).unwrap();
    let short = must(path, &["rev-parse", "--short=7", "HEAD"]);
    let tag = must(path, &["rev-parse", "--short=8", "v1"]);
    for text in [
        short.clone(),
        format!("{short}~1"),
        format!("{short}^{{tree}}"),
        format!("{short}:dir/sub/a"),
        format!("{tag}^{{}}"),
        tag.clone(),
        "0000000".to_owned(),
    ] {
        let expected = git(path, &["rev-parse", "--verify", "-q", &text]);
        let actual = resolve(&repository, &text).ok().map(|id| id.to_string());
        assert_eq!(actual, expected, "{text}");
    }
}

/// Many packed blobs make the commit's four-digit prefix ambiguous; a step that needs a commit
/// selects it, as Git does. Fast-import with fixed dates makes the IDs deterministic.
#[test]
fn ambiguous_abbreviations_are_reported_or_disambiguated_by_kind() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    must(path, &["init", "-q", "--bare", "."]);
    short_ids_git::populate(path, 70_000);
    let repository = Repository::open(path).unwrap();
    let commit = must(path, &["rev-parse", "main"]);
    let prefix = &commit[..4];
    assert!(
        git(path, &["rev-parse", "--verify", "-q", prefix]).is_none(),
        "fixture must make {prefix} ambiguous"
    );
    let error = resolve(&repository, prefix).unwrap_err();
    assert!(
        matches!(
            *error,
            RevisionError::Abbreviated(girt::ResolvePrefixError::Ambiguous { .. })
        ),
        "{error}"
    );
    for text in [format!("{prefix}^{{commit}}"), format!("{prefix}:file")] {
        let expected = git(path, &["rev-parse", "--verify", "-q", &text]);
        assert!(expected.is_some(), "git disambiguates {text}");
        let actual = resolve(&repository, &text).ok().map(|id| id.to_string());
        assert_eq!(actual, expected, "{text}");
    }
}
