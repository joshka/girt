//! `filter.<name>.clean` and `smudge` drivers compared with `git add` and `git checkout-index`.
#![cfg(unix)]
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::path::Path;

use girt::filter::{Commands, DriverError, Error, Limits, Pipeline, Staged};
use girt::workflow::Environment;
use girt::{ObjectId, Repository};
use pack_git::{git, git_output};

const CONFIG: &[(&str, &str)] = &[
    ("filter.up.clean", "tr a-z A-Z"),
    ("filter.up.smudge", "tr A-Z a-z"),
    ("filter.args.clean", "printf '%s|' %f \"$PWD\" >> args; cat"),
    ("filter.args.smudge", "printf '%s|' %f >> args; cat"),
    ("filter.bad.clean", "exit 3"),
    ("filter.bad.smudge", "exit 3"),
    ("filter.badreq.clean", "exit 3"),
    ("filter.badreq.required", "true"),
    ("filter.none.required", "true"),
    ("filter.short.clean", "head -c 1"),
];

const ATTRIBUTES: &str = "*.u filter=up\n*.a filter=args\n*.b filter=bad\n*.r filter=badreq\n\
    *.n filter=none\n*.e filter=up text eol=crlf\n*.x filter=unconfigured\n*.s filter=short\n";

fn repository(root: &Path) {
    git(root, &["init", "-q", "."], b"");
    git(root, &["config", "core.attributesFile", "absent"], b"");
    for (key, value) in CONFIG {
        git(root, &["config", key, value], b"");
    }
    std::fs::write(root.join(".gitattributes"), ATTRIBUTES).unwrap();
}

fn filters(root: &Path, commands: Commands) -> Pipeline {
    let environment = Environment::default().with("GIT_ATTR_NOSYSTEM", "1");
    Repository::open(root)
        .unwrap()
        .filters(&environment, commands)
        .unwrap()
}

#[test]
fn clean_matches_git_add() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    std::fs::create_dir(root.join("sub")).unwrap();
    let files: &[(&str, &[u8])] = &[
        ("x.u", b"hello\n"),
        ("sub/it's a.a", b"q\n"),
        ("x.b", b"b\n"),
        ("x.e", b"hi\r\n"),
        ("x.x", b"x\n"),
        ("x.s", &[b'z'; 100_000]),
    ];
    for (name, bytes) in files {
        std::fs::write(root.join(name), bytes).unwrap();
        let output = git_output(&root.join("sub"), &["add", &format!("../{name}")], b"");
        assert!(output.status.success(), "{name}");
    }
    let git_args = std::fs::read(root.join("args")).unwrap();
    std::fs::remove_file(root.join("args")).unwrap();

    let mut filters = filters(root, Commands::Run);
    for (name, bytes) in files {
        let expected = git(root, &["cat-file", "blob", &format!(":{name}")], b"");
        let ours = filters
            .to_repository(name.as_bytes(), bytes, Staged::Absent)
            .unwrap();
        assert_eq!(*ours.bytes, *expected, "{name}");
        assert_eq!(ours.skipped.is_some(), *name == "x.b", "{name}");
    }
    // `%f` and the working directory match what Git gave the command. Later `git add` runs clean
    // the racily clean entry again, so Git's record repeats.
    let ours = std::fs::read(root.join("args")).unwrap();
    assert!(!ours.is_empty() && git_args.starts_with(&ours), "{ours:?}");
}

#[test]
fn smudge_matches_git_checkout_index() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let blobs: &[(&str, &[u8])] = &[
        ("x.u", b"HELLO\n"),
        ("y.a", b"q\n"),
        ("x.b", b"b\n"),
        ("x.e", b"HI\n"),
        ("x.x", b"x\n"),
    ];
    let mut ids = Vec::new();
    for (name, bytes) in blobs {
        let id = git(
            root,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            bytes,
        );
        let id = String::from_utf8(id).unwrap().trim().to_owned();
        let info = format!("100644,{id},{name}");
        git(root, &["update-index", "--add", "--cacheinfo", &info], b"");
        ids.push(id.parse::<ObjectId>().unwrap());
    }
    git(root, &["checkout-index", "-a", "-f"], b"");
    let git_args = std::fs::read(root.join("args")).unwrap();
    std::fs::remove_file(root.join("args")).unwrap();

    let mut filters = filters(root, Commands::Run);
    for ((name, bytes), id) in blobs.iter().zip(ids) {
        let written = std::fs::read(root.join(name)).unwrap();
        let ours = filters.to_worktree(name.as_bytes(), bytes, id).unwrap();
        assert_eq!(*ours.bytes, *written, "{name}");
        assert_eq!(ours.skipped.is_some(), *name == "x.b", "{name}");
    }
    assert_eq!(std::fs::read(root.join("args")).unwrap(), git_args);
}

#[test]
fn required_failures_are_errors_as_in_git() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let mut filters = filters(root, Commands::Run);
    for name in ["x.r", "x.n"] {
        std::fs::write(root.join(name), b"r\n").unwrap();
        let output = git_output(root, &["add", name], b"");
        assert_eq!(output.status.code(), Some(128), "{name}");
        let error = filters
            .to_repository(name.as_bytes(), b"r\n", Staged::Absent)
            .unwrap_err();
        let Error::Driver { source, .. } = error else {
            panic!("{name}: {error:?}");
        };
        match name {
            "x.r" => assert!(matches!(source, DriverError::Failed { .. })),
            _ => assert!(matches!(source, DriverError::Missing { .. })),
        }
    }
}

#[test]
fn refuse_runs_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    let mut filters = filters(root, Commands::Refuse);
    let error = filters
        .to_repository(b"y.a", b"q\n", Staged::Absent)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    let id = ObjectId::null(girt::ObjectFormat::Sha1);
    let error = filters.to_worktree(b"y.a", b"q\n", id).unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    assert!(!root.join("args").exists());
    // A driver without a command for the direction needs no permission.
    let ours = filters
        .to_repository(b"x.x", b"x\n", Staged::Absent)
        .unwrap();
    assert_eq!(*ours.bytes, *b"x\n");
}

#[test]
fn output_limit_stops_the_command() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root);
    git(root, &["config", "filter.up.required", "true"], b"");
    let environment = Environment::default().with("GIT_ATTR_NOSYSTEM", "1");
    let repo = Repository::open(root).unwrap();
    let mut filters = repo.filters(&environment, Commands::Run).unwrap();
    let drivers = girt::filter::Drivers::from_config(repo.config())
        .unwrap()
        .with_commands(Commands::Run)
        .with_limits(Limits::default().with_max_output_bytes(10));
    let lookup = repo
        .attributes(&environment, repo.attribute_source(&environment).unwrap())
        .unwrap();
    let mut limited = Pipeline::new(lookup, filters.settings(), drivers);
    let big = vec![b'a'; 1 << 20];
    let error = limited
        .to_repository(b"x.u", &big, Staged::Absent)
        .unwrap_err();
    assert!(
        matches!(
            error,
            Error::Driver {
                source: DriverError::TooLarge { .. },
                ..
            }
        ),
        "{error:?}"
    );
    let ours = filters.to_repository(b"x.u", &big, Staged::Absent).unwrap();
    assert_eq!(ours.bytes.len(), big.len());
}
