//! Compares pathspec selection with `git ls-files` over an index written through Git.
//!
//! Paths enter the index with `update-index --cacheinfo`, so case-only differences survive on
//! case-insensitive filesystems.
use std::path::Path;
use std::process::Command;

use girt::attributes::{Attributes, Case, Limits, Source};
use girt::pathspec::{Context, ParseError, Pathspec};

const PATHS: &[&str] = &[
    "README",
    "readme",
    "src/README",
    "src/a.rs",
    "src/sub/b.rs",
    "src/gen/out.rs",
    "tests/t.rs",
    "docs/x.md",
    "a b/c",
    "st*r",
    "q?",
    "a[b]/x",
    "a[b]f",
    "ab",
    "AB",
    "aB",
    "Ab",
    "srcx",
    "sub/deep/g",
    "sub/f",
    ".hidden/h",
];

const SPECS: &[&[&str]] = &[
    &["."],
    &["src"],
    &["src/"],
    &["sr"],
    &["src/a.rs/"],
    &["srcx"],
    &["src/*.rs"],
    &["*.rs"],
    &["s*"],
    &["s?c"],
    &["*/b.rs"],
    &["src/*/"],
    &["q?"],
    &["q[?]"],
    &["q\\?"],
    &["st*r"],
    &["a[b]"],
    &["a[b]f"],
    &["\\*"],
    &["[?]"],
    &[":(glob)src/*.rs"],
    &[":(glob)*.rs"],
    &[":(glob)**/*.rs"],
    &[":(glob)src/**"],
    &[":(glob)src/**/b.rs"],
    &[":(glob)s*"],
    &[":(glob)src"],
    &[":(glob)src/*"],
    &[":(glob)"],
    &[":(glob)a[b]"],
    &[":(glob)**"],
    &[":(exclude)tests"],
    &[":!tests"],
    &[":^tests"],
    &[".", ":!src"],
    &[":!*.rs", "src"],
    &[":!src", ":!tests"],
    &[":(top,exclude)src"],
    &[":!src/"],
    &[":^src/a.rs", "src"],
    &[":(exclude)"],
    &[":!"],
    &[":/!x"],
    &[":!/x"],
    &[":(exclude)*.rs"],
    &[":(icase)readme"],
    &[":(icase)SRC"],
    &[":(icase)Src/A.RS"],
    &[":(icase)S*"],
    &[":(icase,glob)SRC/*.RS"],
    &[":(icase)A[B]"],
    &[":(icase)a[B]"],
    &[":(icase)A[b]"],
    &[":(icase)a[A-Z]"],
    &[":(icase)[[:upper:]]b"],
    &[":(icase,exclude)README"],
    &[":(literal)st*r"],
    &[":(literal)q?"],
    &[":(literal)q[?]"],
    &[":(literal)"],
    &[":(literal,icase)ST*R"],
    &[":/src"],
    &[":/"],
    &[":(top)src"],
    &[":"],
    &["::x"],
    &[":x"],
    &["::src"],
    &[":()src"],
    &["./src"],
    &["src/../docs"],
    &["src//a.rs"],
    &["src/."],
    &["sub/deep/.."],
];

const PREFIXED: &[&[&str]] = &[
    &["a.rs"],
    &["../tests"],
    &[":/tests"],
    &[":(top)"],
    &["*"],
    &[":!sub"],
    &[":!gen"],
    &[":(exclude)../README"],
    &[":(top,exclude)src/gen"],
    &["."],
    &["../*.md"],
    &[":(glob)*"],
];

fn git(directory: &Path, args: &[&str], envs: &[(&str, &str)]) -> (bool, Vec<u8>) {
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
        .envs(envs.iter().copied())
        .args(args)
        .output()
        .expect("Git required for pathspec compatibility");
    (output.status.success(), output.stdout)
}

fn repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    assert!(git(root.path(), &["init", "-q", "--template=", "."], &[]).0);
    let (_, blob) = git(root.path(), &["hash-object", "-w", "--stdin"], &[]);
    let blob = String::from_utf8(blob).unwrap();
    for path in PATHS {
        let info = format!("100644,{},{path}", blob.trim());
        assert!(
            git(
                root.path(),
                &["update-index", "--add", "--cacheinfo", &info],
                &[]
            )
            .0
        );
    }
    std::fs::create_dir_all(root.path().join("src/sub")).unwrap();
    root
}

/// Returns Git's selection, or `None` when Git rejects the specs.
fn git_selection(
    root: &Path,
    prefix: &str,
    specs: &[&str],
    envs: &[(&str, &str)],
) -> Option<Vec<Vec<u8>>> {
    let mut args = vec!["ls-files", "-z", "--full-name", "--"];
    args.extend(specs);
    let (ok, stdout) = git(&root.join(prefix), &args, envs);
    ok.then(|| {
        let mut paths: Vec<_> = stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        paths.sort();
        paths
    })
}

fn girt_selection(specs: &[&str], context: &Context) -> Result<Vec<Vec<u8>>, ParseError> {
    let spec = Pathspec::parse(specs, context)?;
    let mut paths: Vec<_> = PATHS
        .iter()
        .map(|path| path.as_bytes())
        .filter(|path| spec.matches(path))
        .map(<[u8]>::to_vec)
        .collect();
    paths.sort();
    Ok(paths)
}

fn compare(root: &Path, prefix: &str, specs: &[&str], context: &Context, envs: &[(&str, &str)]) {
    let expected = git_selection(root, prefix, specs, envs);
    let actual = girt_selection(specs, context);
    match (&expected, &actual) {
        (Some(expected), Ok(actual)) => assert_eq!(
            actual
                .iter()
                .map(|p| String::from_utf8_lossy(p))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|p| String::from_utf8_lossy(p))
                .collect::<Vec<_>>(),
            "{specs:?} from {prefix:?} with {envs:?}"
        ),
        (None, Err(_)) => {}
        _ => panic!("{specs:?} from {prefix:?}: git {expected:?}, girt {actual:?}"),
    }
    // Pruning must never hide a selected path or claim an unselected one.
    if let Ok(spec) = Pathspec::parse(specs, context) {
        for path in PATHS {
            let path = path.as_bytes();
            let selected = spec.matches(path);
            for (index, _) in path.iter().enumerate().filter(|(_, byte)| **byte == b'/') {
                match spec.directory(&path[..index]) {
                    girt::pathspec::Prune::Skip => assert!(!selected, "{specs:?} skipped {path:?}"),
                    girt::pathspec::Prune::All => assert!(selected, "{specs:?} took {path:?}"),
                    girt::pathspec::Prune::Descend => {}
                }
            }
        }
    }
}

#[test]
fn matches_git_ls_files() {
    let root = repository();
    for specs in SPECS {
        compare(root.path(), "", specs, &Context::new(), &[]);
    }
    let context = Context::new().with_prefix(b"src");
    for specs in PREFIXED {
        compare(root.path(), "src", specs, &context, &[]);
    }
}

#[test]
fn environment_defaults_match_git() {
    let root = repository();
    let cases: &[(&str, Context)] = &[
        ("GIT_GLOB_PATHSPECS", Context::new().with_glob(true)),
        ("GIT_NOGLOB_PATHSPECS", Context::new().with_noglob(true)),
        ("GIT_ICASE_PATHSPECS", Context::new().with_icase(true)),
        ("GIT_LITERAL_PATHSPECS", Context::new().with_literal(true)),
    ];
    for (variable, context) in cases {
        for specs in SPECS {
            compare(root.path(), "", specs, context, &[(variable, "1")]);
        }
    }
}

const ATTRIBUTES: &[u8] = b"src/*.rs foo\ndocs/** foo=bar\n*.md -foo\nsub/** v=a,b\n";

const ATTR_SPECS: &[&[&str]] = &[
    &[":(attr:foo)"],
    &[":(attr:-foo)"],
    &[":(attr:!foo)"],
    &[":(attr:foo=bar)"],
    &[":(attr:foo -foo)"],
    &[":(attr:foo)src"],
    &[":(attr:foo)*.md"],
    &[":(attr:-foo,icase)DOCS"],
    &[":(attr:foo,exclude)"],
    &[":(attr:foo,exclude)src"],
    &[":(attr:  foo  )"],
    &[r":(attr:v=a\,b)"],
    &[":(attr:v=a)"],
    &[":(attr:!foo !v)", ":!.hidden"],
    &[":(attr:)"],
    &[":(attr:b@d)"],
    &[":(attr:a,attr:b)"],
    &[r":(attr:foo=bar\ x)"],
];

#[test]
fn attribute_magic_matches_git() {
    let root = repository();
    std::fs::write(root.path().join(".gitattributes"), ATTRIBUTES).unwrap();
    let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
    attributes.add(Source::Directory(b""), ATTRIBUTES).unwrap();
    for specs in ATTR_SPECS {
        let expected = git_selection(root.path(), "", specs, &[]);
        let actual = Pathspec::parse(*specs, &Context::new()).map(|spec| {
            let mut paths: Vec<_> = PATHS
                .iter()
                .map(|path| path.as_bytes())
                .filter(|path| {
                    spec.matches_with(path, |requirement| {
                        attributes
                            .get(path, requirement.name())
                            .map(|state| requirement.accepts(state))
                    })
                    .unwrap()
                })
                .map(<[u8]>::to_vec)
                .collect();
            paths.sort();
            paths
        });
        match (&expected, &actual) {
            (Some(expected), Ok(actual)) => assert_eq!(actual, expected, "{specs:?}"),
            (None, Err(_)) => {}
            _ => panic!("{specs:?}: git {expected:?}, girt {actual:?}"),
        }
    }
}
