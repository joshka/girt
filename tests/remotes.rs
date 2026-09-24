//! Independent Git CLI fixtures; no upstream source or test data is used.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::refs::RefName;
use girt::remote::{Direction, MappingError, RefSource, Refspecs, Remote};
use girt::{ObjectId, Repository};
use rstest::rstest;

fn attempt(path: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git is required for interoperability fixtures");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let output = attempt(path, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap().trim()
}
fn init() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(
        dir.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            "--object-format=sha1",
            "--template=",
            ".",
        ],
        b"",
    );
    dir
}
fn populated() -> (tempfile::TempDir, Vec<RefSource>) {
    let dir = init();
    let tree = git(dir.path(), &["mktree"], b"");
    let commit = git(
        dir.path(),
        &["commit-tree", text(&tree)],
        b"Independent fixture\n",
    );
    let names = [
        "refs/heads/main",
        "refs/heads/private/a",
        "refs/heads/prepost",
        "refs/heads/pre-mid-post",
        "refs/tags/v1",
    ];
    let mut sources = Vec::new();
    for name in names {
        git(dir.path(), &["update-ref", name, text(&commit)], b"");
        sources.push(RefSource {
            name: RefName::new(name).unwrap(),
            id: text(&commit).parse().unwrap(),
        });
    }
    sources.push(RefSource {
        name: RefName::new(b"HEAD").unwrap(),
        id: text(&commit).parse().unwrap(),
    });
    (dir, sources)
}

#[rstest]
#[case::all(vec!["refs/heads/*:refs/remotes/o/*"], vec!["refs/remotes/o/main", "refs/remotes/o/pre-mid-post", "refs/remotes/o/prepost", "refs/remotes/o/private/a"])]
#[case::excluded(vec!["+refs/heads/*:refs/remotes/o/*", "^refs/heads/private/*"], vec!["refs/remotes/o/main", "refs/remotes/o/pre-mid-post", "refs/remotes/o/prepost"])]
#[case::negative_first(vec!["^refs/heads/private/*", "refs/heads/*:refs/remotes/o/*"], vec!["refs/remotes/o/main", "refs/remotes/o/pre-mid-post", "refs/remotes/o/prepost"])]
#[case::partial_pattern(vec!["refs/heads/pre*post:refs/tags/x*y"], vec!["refs/tags/x-mid-y", "refs/tags/xy"])]
#[case::exact(vec!["refs/heads/main:refs/remotes/o/main"], vec!["refs/remotes/o/main"])]
#[case::head(vec!["HEAD:refs/remotes/o/chosen"], vec!["refs/remotes/o/chosen"])]
#[case::selection(vec!["HEAD:"], vec![])]
#[case::duplicate(vec!["refs/heads/main:refs/remotes/o/main", "refs/heads/main:refs/remotes/o/main"], vec!["refs/remotes/o/main"])]
#[case::exact_exclusion(vec!["refs/heads/main:refs/remotes/o/main", "^refs/heads/main"], vec![])]
fn git_fetch_agrees_with_mapping(#[case] values: Vec<&str>, #[case] expected: Vec<&str>) {
    let (source, inputs) = populated();
    let destination = init();
    let specs = Refspecs::parse(Direction::Fetch, &values).unwrap();
    let mappings = specs.map(&inputs).unwrap();
    let mut destinations: Vec<_> = mappings
        .iter()
        .filter_map(|m| m.destination.as_ref())
        .map(|n| std::str::from_utf8(n.as_bytes()).unwrap())
        .collect();
    destinations.sort();
    assert_eq!(destinations, expected);
    let mut args = vec![
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        source.path().to_str().unwrap(),
    ];
    args.extend(values);
    git(destination.path(), &args, b"");
    let actual = git(
        destination.path(),
        &["for-each-ref", "--format=%(refname)"],
        b"",
    );
    assert_eq!(text(&actual).lines().collect::<Vec<_>>(), expected);
    assert_mapping_ids(destination.path(), &mappings);
}

// Verify actual Git-published values, not only the mapper's list of destination names.
fn assert_mapping_ids(path: &Path, mappings: &[girt::remote::Mapping]) {
    for mapping in mappings {
        if let (Some(source), Some(destination)) = (&mapping.source, &mapping.destination) {
            let id = git(
                path,
                &[
                    "rev-parse",
                    std::str::from_utf8(destination.as_bytes()).unwrap(),
                ],
                b"",
            );
            assert_eq!(text(&id).parse::<ObjectId>().unwrap(), source.id);
        }
    }
}

#[rstest]
#[case::explicit("refs/heads/main:refs/heads/copied", vec!["refs/heads/copied"])]
#[case::implicit_same("refs/heads/main", vec!["refs/heads/main"])]
#[case::wildcard("refs/heads/pre*:refs/heads/new*", vec!["refs/heads/new-mid-post", "refs/heads/newpost"])]
#[case::wildcard_same("refs/heads/pre*", vec!["refs/heads/pre-mid-post", "refs/heads/prepost"])]
#[case::head("HEAD:refs/heads/copied", vec!["refs/heads/copied"])]
#[case::force("+refs/heads/main:refs/heads/copied", vec!["refs/heads/copied"])]
fn git_push_agrees_with_mapping(#[case] value: &str, #[case] expected: Vec<&str>) {
    let (source, inputs) = populated();
    let destination = init();
    let mappings = Refspecs::parse(Direction::Push, [value])
        .unwrap()
        .map(&inputs)
        .unwrap();
    git(
        source.path(),
        &["push", destination.path().to_str().unwrap(), value],
        b"",
    );
    let actual = git(
        destination.path(),
        &["for-each-ref", "--format=%(refname)"],
        b"",
    );
    assert_eq!(text(&actual).lines().collect::<Vec<_>>(), expected);
    assert_eq!(mappings.len(), expected.len());
    assert_mapping_ids(destination.path(), &mappings);
}

#[test]
fn git_push_deletion_agrees_without_a_local_source() {
    let (destination, _) = populated();
    let source = init();
    let mappings = Refspecs::parse(Direction::Push, [":refs/heads/prepost"])
        .unwrap()
        .map(&[])
        .unwrap();
    assert!(mappings[0].source.is_none());
    git(
        source.path(),
        &[
            "push",
            destination.path().to_str().unwrap(),
            ":refs/heads/prepost",
        ],
        b"",
    );
    assert!(
        !attempt(
            destination.path(),
            &["show-ref", "--verify", "refs/heads/prepost"],
            b""
        )
        .status
        .success()
    );
}

#[rstest]
#[case::collision(vec!["refs/heads/main:refs/remotes/o/one", "refs/heads/prepost:refs/remotes/o/one"])]
#[case::missing(vec!["refs/heads/missing:refs/remotes/o/one"])]
#[case::excluded_missing(vec!["refs/heads/missing:refs/remotes/o/one", "^refs/heads/missing"])]
fn git_and_mapper_reject_unusable_fetch_plan(#[case] values: Vec<&str>) {
    let (source, inputs) = populated();
    let destination = init();
    let result = Refspecs::parse(Direction::Fetch, &values)
        .unwrap()
        .map(&inputs);
    assert!(matches!(
        result,
        Err(MappingError::Collision(_) | MappingError::UnmatchedSource(_))
    ));
    let mut args = vec!["fetch", "--no-tags", source.path().to_str().unwrap()];
    args.extend(values);
    assert!(!attempt(destination.path(), &args, b"").status.success());
}

#[rstest]
#[case::url_list(vec!["one", "two"], vec![])]
#[case::push_override(vec!["one", "two"], vec!["push1", "push2"])]
#[case::url_reset(vec!["old", "", "new"], vec![])]
#[case::push_reset(vec!["one", "two"], vec!["old", ""])]
#[case::push_reset_append(vec!["one"], vec!["old", "", "new"])]
#[case::duplicates(vec!["one", "one"], vec![])]
fn git_remote_url_order_and_fallback(#[case] urls: Vec<&str>, #[case] push_urls: Vec<&str>) {
    let dir = init();
    configure_urls(dir.path(), "url", &urls);
    configure_urls(dir.path(), "pushurl", &push_urls);
    let repository = Repository::open(dir.path()).unwrap();
    let remote = Remote::find(repository.config(), b"origin")
        .unwrap()
        .unwrap();
    let fetch = git(dir.path(), &["remote", "get-url", "origin"], b"");
    let push = git(
        dir.path(),
        &["remote", "get-url", "--push", "--all", "origin"],
        b"",
    );
    assert_eq!(remote.fetch_url().unwrap(), text(&fetch).as_bytes());
    assert_eq!(
        remote.push_urls(),
        text(&push)
            .lines()
            .map(|s| s.as_bytes().to_vec())
            .collect::<Vec<_>>()
    );
}
fn configure_urls(path: &Path, key: &str, values: &[&str]) {
    for value in values {
        git(
            path,
            &["config", "--add", &format!("remote.origin.{key}"), value],
            b"",
        );
    }
}

#[test]
fn git_generated_named_config_selects_the_same_fetch_refs() {
    let (source, inputs) = populated();
    let destination = init();
    git(
        destination.path(),
        &["remote", "add", "origin", source.path().to_str().unwrap()],
        b"",
    );
    git(
        destination.path(),
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "^refs/heads/private/*",
        ],
        b"",
    );
    git(
        destination.path(),
        &["config", "remote.origin.followRemoteHEAD", "never"],
        b"",
    );
    let repository = Repository::open(destination.path()).unwrap();
    let remote = Remote::find(repository.config(), b"origin")
        .unwrap()
        .unwrap();
    let mappings = remote.fetch_refspecs().map(&inputs).unwrap();
    assert_eq!(mappings.len(), 3);
    git(destination.path(), &["fetch", "--no-tags", "origin"], b"");
    assert_mapping_ids(destination.path(), &mappings);
    assert!(
        !attempt(
            destination.path(),
            &["show-ref", "--verify", "refs/remotes/origin/private/a"],
            b""
        )
        .status
        .success()
    );
}

#[test]
fn git_annotated_tag_and_symbolic_head_keep_advertised_identities() {
    use girt::fetch::{AdvertisedRef, Advertisement};
    let (source, _) = populated();
    let destination = init();
    git(
        source.path(),
        &[
            "tag",
            "--force",
            "--annotate",
            "v1",
            "--message=Original fixture",
            "HEAD",
        ],
        b"",
    );
    let head = git(source.path(), &["rev-parse", "HEAD"], b"");
    let tag = git(source.path(), &["rev-parse", "refs/tags/v1"], b"");
    let listing = git(
        source.path(),
        &["ls-remote", "--symref", source.path().to_str().unwrap()],
        b"",
    );
    let listing = text(&listing);
    assert!(listing.contains("ref: refs/heads/main\tHEAD"));
    assert!(listing.contains(&format!("{}\trefs/tags/v1^{{}}", text(&head))));
    let advertisement = Advertisement {
        refs: vec![
            AdvertisedRef {
                name: RefName::new("HEAD").unwrap(),
                id: text(&head).parse().unwrap(),
                peeled: false,
            },
            AdvertisedRef {
                name: RefName::new("refs/tags/v1").unwrap(),
                id: text(&tag).parse().unwrap(),
                peeled: false,
            },
            AdvertisedRef {
                name: RefName::new("refs/tags/v1").unwrap(),
                id: text(&head).parse().unwrap(),
                peeled: true,
            },
        ],
        capabilities: vec![b"symref=HEAD:refs/heads/main".to_vec()],
    };
    let values = ["HEAD:refs/remotes/o/chosen", "refs/tags/*:refs/tags/*"];
    let mappings = Refspecs::parse(Direction::Fetch, values)
        .unwrap()
        .map_advertisement(&advertisement)
        .unwrap();
    git(
        destination.path(),
        &[
            "fetch",
            "--no-tags",
            source.path().to_str().unwrap(),
            values[0],
            values[1],
        ],
        b"",
    );
    assert_eq!(
        mappings[0].source.as_ref().unwrap().name.as_bytes(),
        b"HEAD"
    );
    assert_ne!(
        mappings[0].source.as_ref().unwrap().id,
        mappings[1].source.as_ref().unwrap().id
    );
    assert_mapping_ids(destination.path(), &mappings);
}
