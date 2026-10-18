//! `ReplacedObjects` compared with Git's replace-reference and graft behaviour on original
//! histories written through the `git` executable.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use girt::replace::{ReplacedObjects, Settings};
use girt::workflow::Environment;
use girt::{HistoryError, ObjectId, ObjectReadError, Repository};

fn run(directory: &Path, args: &[&str]) -> Output {
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
        .env(
            "GIT_CONFIG_PARAMETERS",
            "'advice.graftFileDeprecated=false'",
        )
        .env("GIT_AUTHOR_NAME", "Replace Author")
        .env("GIT_AUTHOR_EMAIL", "replace@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Replace Author")
        .env("GIT_COMMITTER_EMAIL", "replace@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git required for replace compatibility tests");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    child.wait_with_output().unwrap()
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = run(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn ids(text: &str) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = text.lines().map(|line| line.parse().unwrap()).collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// A repository with `main` = c1..c4 and an unrelated two-commit history `old` = o1, o2.
struct Fixture {
    root: tempfile::TempDir,
    main: Vec<ObjectId>,
    old: Vec<ObjectId>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        git(path, &["init", "-q", "-b", "main", "--template=", "."]);
        let mut commit = |message: &str| {
            std::fs::write(path.join("f"), message).unwrap();
            git(path, &["add", "f"]);
            git(path, &["commit", "-q", "-m", message]);
            git(path, &["rev-parse", "HEAD"]).parse().unwrap()
        };
        let main = ["c1", "c2", "c3", "c4"].map(&mut commit).to_vec();
        git(path, &["checkout", "-q", "--orphan", "old"]);
        let old = ["o1", "o2"].map(&mut commit).to_vec();
        git(path, &["checkout", "-q", "main"]);
        Self { root, main, old }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn view(&self, settings: &Settings) -> ReplacedObjects {
        let repo = Repository::open(self.path()).unwrap();
        ReplacedObjects::new(
            repo.objects().unwrap(),
            repo.replacements(settings).unwrap(),
        )
    }

    fn git_walk(&self, args: &[&str]) -> Vec<ObjectId> {
        // Git's global `--no-replace-objects` precedes the subcommand.
        let (mut command, args) = match args {
            ["--no-replace-objects", rest @ ..] => (vec!["--no-replace-objects", "rev-list"], rest),
            _ => (vec!["rev-list"], args),
        };
        command.extend_from_slice(args);
        ids(&git(self.path(), &command))
    }
}

#[test]
fn graft_replacement_matches_rev_list_merge_base_and_cat_file() {
    let fixture = Fixture::new();
    let [_, c2, _, c4] = fixture.main[..] else {
        panic!()
    };
    // Stitch c2 onto the old history; write a commit-graph first so it must be ignored.
    git(fixture.path(), &["commit-graph", "write", "--reachable"]);
    git(
        fixture.path(),
        &[
            "replace",
            "--graft",
            &c2.to_string(),
            &fixture.old[1].to_string(),
        ],
    );

    let view = fixture.view(&Settings::default());

    assert_eq!(
        sorted(view.walk(&[c4]).unwrap()),
        fixture.git_walk(&["main"])
    );
    assert_eq!(
        sorted(view.walk_range(&[c4], &[fixture.old[0]]).unwrap()),
        fixture.git_walk(&["main", "^old~1"])
    );
    assert_eq!(
        sorted(view.merge_bases(c4, fixture.old[1]).unwrap()),
        ids(&git(
            fixture.path(),
            &["merge-base", "--all", "main", "old"]
        ))
    );
    assert!(view.is_ancestor(fixture.old[0], c4).unwrap());
    assert!(!view.raw().is_ancestor(fixture.old[0], c4).unwrap());
    assert_eq!(
        sorted(view.raw().walk(&[c4]).unwrap()),
        fixture.git_walk(&["--no-replace-objects", "main"])
    );

    let object = view.read(c2).unwrap().unwrap();
    assert_eq!(
        object.data(),
        run(fixture.path(), &["cat-file", "commit", &c2.to_string()]).stdout
    );
    assert_ne!(object.id(), c2);
    assert_eq!(view.raw().read(c2).unwrap().unwrap().id(), c2);
}

#[test]
fn disabled_settings_match_no_replace_objects() {
    let fixture = Fixture::new();
    let [_, c2, _, c4] = fixture.main[..] else {
        panic!()
    };
    git(
        fixture.path(),
        &[
            "replace",
            "--graft",
            &c2.to_string(),
            &fixture.old[1].to_string(),
        ],
    );
    let expected = fixture.git_walk(&["--no-replace-objects", "main"]);

    let disabled = Settings::default().with_replace_refs(false);
    assert_eq!(
        sorted(fixture.view(&disabled).walk(&[c4]).unwrap()),
        expected
    );

    git(fixture.path(), &["config", "core.useReplaceRefs", "false"]);
    let repo = Repository::open(fixture.path()).unwrap();
    let from_config = Settings::from_config(repo.config()).unwrap();
    assert!(!from_config.uses_replace_refs());
    assert_eq!(
        sorted(fixture.view(&from_config).walk(&[c4]).unwrap()),
        expected
    );

    let environment = Environment::default().with("GIT_NO_REPLACE_OBJECTS", "0");
    let from_environment = Settings::default().with_environment(&environment).unwrap();
    assert_eq!(
        sorted(fixture.view(&from_environment).walk(&[c4]).unwrap()),
        expected
    );
}

#[test]
fn replace_ref_base_moves_the_namespace() {
    let fixture = Fixture::new();
    let [c1, _, _, c4] = fixture.main[..] else {
        panic!()
    };
    git(
        fixture.path(),
        &["update-ref", &format!("refs/other/{c4}"), &c1.to_string()],
    );
    let environment = Environment::default().with("GIT_REPLACE_REF_BASE", "refs/other/");
    let settings = Settings::default().with_environment(&environment).unwrap();

    assert_eq!(fixture.view(&settings).walk(&[c4]).unwrap(), [c4]);
    assert_eq!(fixture.view(&settings).read(c4).unwrap().unwrap().id(), c1);
    assert!(Settings::default().with_ref_base("refs/replace").is_err());
    assert!(Settings::default().with_ref_base("").is_err());
}

#[test]
fn grafts_override_parents_but_not_content_and_ignore_replace_switch() {
    let fixture = Fixture::new();
    let [c1, c2, c3, c4] = fixture.main[..] else {
        panic!()
    };
    let info = fixture.path().join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(
        info.join("grafts"),
        format!(
            "# comment\n\n{c3} {c1}\ngarbage\n{c3} {c2}\n{}\t{}",
            fixture.old[1], c4
        )
        .to_uppercase()
        .replace("# COMMENT", "# comment")
        .replace("GARBAGE", "garbage"),
    )
    .unwrap();

    let disabled = Settings::default().with_replace_refs(false);
    for settings in [Settings::default(), disabled] {
        let view = fixture.view(&settings);
        assert_eq!(
            sorted(view.walk(&[c4]).unwrap()),
            fixture.git_walk(&["main"])
        );
        assert_eq!(view.replacements().ignored_graft_lines(), [4, 5]);
        assert_eq!(view.replacements().graft_count(), 2);
        assert_eq!(view.read(c3).unwrap().unwrap().id(), c3);
    }
    assert_eq!(
        sorted(fixture.view(&Settings::default()).walk(&[c4]).unwrap()),
        fixture.git_walk(&["--no-replace-objects", "main"])
    );
}

#[test]
fn graft_on_original_wins_over_replacement_and_shallow_wins_over_both() {
    let fixture = Fixture::new();
    let [c1, c2, c3, c4] = fixture.main[..] else {
        panic!()
    };
    git(
        fixture.path(),
        &["replace", "--graft", &c3.to_string(), &c1.to_string()],
    );
    let info = fixture.path().join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("grafts"), format!("{c3} {c2}\n")).unwrap();
    assert_eq!(
        sorted(fixture.view(&Settings::default()).walk(&[c4]).unwrap()),
        fixture.git_walk(&["main"])
    );

    std::fs::write(fixture.path().join(".git/shallow"), format!("{c3}\n")).unwrap();
    assert_eq!(
        sorted(fixture.view(&Settings::default()).walk(&[c4]).unwrap()),
        fixture.git_walk(&["main"])
    );
    assert_eq!(
        sorted(fixture.view(&Settings::default()).walk(&[c4]).unwrap()),
        sorted(vec![c3, c4])
    );
}

#[test]
fn chains_resolve_four_links_and_fail_beyond() {
    let fixture = Fixture::new();
    let [c1, c2, c3, c4] = fixture.main[..] else {
        panic!()
    };
    let chain = [c1, c2, c3, c4, fixture.old[0], fixture.old[1]];
    let replace = |from: ObjectId, to: ObjectId| {
        git(
            fixture.path(),
            &["replace", &from.to_string(), &to.to_string()],
        );
    };
    for link in chain[..5].windows(2) {
        replace(link[0], link[1]);
    }
    let view = fixture.view(&Settings::default());
    assert_eq!(view.read(c1).unwrap().unwrap().id(), fixture.old[0]);
    assert_eq!(
        git(fixture.path(), &["cat-file", "-p", &c1.to_string()]).as_bytes(),
        view.read(c1).unwrap().unwrap().data().trim_ascii_end()
    );

    replace(fixture.old[0], fixture.old[1]);
    let view = fixture.view(&Settings::default());
    assert!(matches!(view.read(c1), Err(ObjectReadError::ReplaceDepth(id)) if id == c1));
    assert!(
        !run(fixture.path(), &["cat-file", "-p", &c1.to_string()])
            .status
            .success()
    );
    assert!(matches!(
        view.walk(&[c1]),
        Err(HistoryError::Read {
            source: ObjectReadError::ReplaceDepth(_),
            ..
        })
    ));
}

#[test]
fn kind_changing_and_missing_replacements() {
    let fixture = Fixture::new();
    let [c1, c2, _, c4] = fixture.main[..] else {
        panic!()
    };
    let blob: ObjectId = git(fixture.path(), &["rev-parse", "main:f"])
        .parse()
        .unwrap();
    git(
        fixture.path(),
        &["replace", "-f", &blob.to_string(), &c1.to_string()],
    );
    let view = fixture.view(&Settings::default());
    assert_eq!(
        view.read(blob).unwrap().unwrap().kind(),
        girt::ObjectKind::Commit
    );
    assert_eq!(
        git(fixture.path(), &["cat-file", "-t", &blob.to_string()]),
        "commit"
    );

    git(fixture.path(), &["replace", "-d", &blob.to_string()]);
    git(
        fixture.path(),
        &["replace", "-f", &c2.to_string(), &blob.to_string()],
    );
    assert!(matches!(
        fixture.view(&Settings::default()).walk(&[c4]),
        Err(HistoryError::NotCommit(id)) if id == c2
    ));
    git(fixture.path(), &["replace", "-d", &c2.to_string()]);

    let absent = "1111111111111111111111111111111111111111";
    let replace_dir = fixture.path().join(".git/refs/replace");
    std::fs::create_dir_all(&replace_dir).unwrap();
    std::fs::write(replace_dir.join(c2.to_string()), format!("{absent}\n")).unwrap();
    assert!(matches!(
        fixture.view(&Settings::default()).read(c2),
        Err(ObjectReadError::MissingReplacement(id)) if id.to_string() == absent
    ));
    assert!(
        !run(fixture.path(), &["cat-file", "-p", &c2.to_string()])
            .status
            .success()
    );
}

#[test]
fn malformed_and_duplicate_replace_ref_names() {
    let fixture = Fixture::new();
    let [c1, c2, ..] = fixture.main[..] else {
        panic!()
    };
    git(
        fixture.path(),
        &["update-ref", "refs/replace/not-an-id", &c2.to_string()],
    );
    let upper = c1.to_string().to_uppercase();
    git(
        fixture.path(),
        &[
            "update-ref",
            &format!("refs/replace/{upper}"),
            &c2.to_string(),
        ],
    );
    let view = fixture.view(&Settings::default());
    assert_eq!(view.read(c1).unwrap().unwrap().id(), c2);
    assert_eq!(view.replacements().ignored_refs().len(), 1);

    // Pack the uppercase spelling so a loose lowercase one can coexist on case-insensitive
    // filesystems.
    git(fixture.path(), &["pack-refs", "--all"]);
    git(
        fixture.path(),
        &["update-ref", &format!("refs/replace/{c1}"), &c2.to_string()],
    );
    let repo = Repository::open(fixture.path()).unwrap();
    assert!(matches!(
        repo.replacements(&Settings::default()),
        Err(girt::replace::LoadError::Duplicate(_))
    ));
    assert!(!run(fixture.path(), &["log", "-1", "main"]).status.success());
}

#[test]
fn revision_syntax_resolves_through_replacements_and_grafts() {
    use girt::revision::{Range, Resolver, Revision};
    let fixture = Fixture::new();
    let [_, c2, c3, c4] = fixture.main[..] else {
        panic!()
    };
    git(
        fixture.path(),
        &[
            "replace",
            "--graft",
            &c3.to_string(),
            &fixture.old[1].to_string(),
        ],
    );
    let repo = Repository::open(fixture.path()).unwrap();
    let view = fixture.view(&Settings::default());
    let resolver = Resolver::new(&repo, view.raw())
        .unwrap()
        .with_replacements(&view);
    let rev = |text: &str| {
        let expected: ObjectId = git(fixture.path(), &["rev-parse", text]).parse().unwrap();
        assert_eq!(
            resolver
                .resolve(&text.parse::<Revision>().unwrap())
                .unwrap(),
            expected,
            "{text}"
        );
    };
    rev("main~2");
    rev("main~3");
    rev("main~1^{tree}");

    let ranges: Vec<Range> = vec!["main...old".parse().unwrap()];
    let selection = resolver.select(&ranges).unwrap();
    let selected = view
        .walk_range(&selection.include, &selection.exclude)
        .unwrap();
    assert_eq!(sorted(selected), fixture.git_walk(&["main...old"]));
    let raw = Resolver::new(&repo, view.raw()).unwrap();
    assert_eq!(raw.resolve(&"main~2".parse().unwrap()).unwrap(), c2);

    // Grafted parents are followed too, including lines with trailing whitespace.
    git(fixture.path(), &["replace", "-d", &c3.to_string()]);
    let info = fixture.path().join(".git/info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(info.join("grafts"), format!("{c4} \t\r\n")).unwrap();
    let view = fixture.view(&Settings::default());
    assert!(view.replacements().ignored_graft_lines().is_empty());
    assert_eq!(
        sorted(view.walk(&[c4]).unwrap()),
        fixture.git_walk(&["main"])
    );
    let resolver = Resolver::new(&repo, view.raw())
        .unwrap()
        .with_replacements(&view);
    assert!(resolver.resolve(&"main~1".parse().unwrap()).is_err());
    assert!(
        !run(fixture.path(), &["rev-parse", "--verify", "-q", "main~1"])
            .status
            .success()
    );
}
