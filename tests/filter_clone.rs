//! Clone checkout through filters, compared byte for byte with `git clone`.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::collections::BTreeMap;
use std::path::Path;

use girt::config::ConfigInputs;
use girt::filter::Commands;
use girt::workflow::{self, CloneOptions, Environment, NoCallbacks};
use pack_git::git;

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if name != ".git" {
            found.insert(name, std::fs::read(&path).unwrap());
        }
    }
    found
}

#[test]
fn clone_checkout_matches_git_clone() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = root.join("src");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main", "."], b"");
    let attributes = "*.txt text\n*.u filter=up\n*.id ident\n";
    std::fs::write(source.join(".gitattributes"), attributes).unwrap();
    std::fs::write(source.join("a.txt"), b"one\ntwo\n").unwrap();
    std::fs::write(source.join("x.u"), b"HELLO\n").unwrap();
    std::fs::write(source.join("v.id"), b"$Id$\n").unwrap();
    git(&source, &["add", "."], b"");
    git(&source, &["commit", "-q", "-m", "one"], b"");

    // Filters and line endings come from the user's global configuration, as Git LFS installs.
    let global = root.join("global-config");
    std::fs::write(
        &global,
        "[core]\n\tautocrlf = true\n[filter \"up\"]\n\tsmudge = tr A-Z a-z\n\tclean = tr a-z A-Z\n",
    )
    .unwrap();
    let theirs = root.join("theirs");
    let output = std::process::Command::new("git")
        .args([
            "clone",
            "-q",
            source.to_str().unwrap(),
            theirs.to_str().unwrap(),
        ])
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success());

    let environment = Environment::default()
        .with("GIT_CONFIG_GLOBAL", &global)
        .with("GIT_CONFIG_NOSYSTEM", "1")
        .with("GIT_ATTR_NOSYSTEM", "1")
        .with("HOME", root);
    let inputs = ConfigInputs::git_default(&environment).unwrap();
    let ours = root.join("ours");
    let clone = |destination: &Path, options: &CloneOptions| {
        workflow::clone_with(
            source.to_str().unwrap(),
            destination,
            options,
            &inputs,
            &environment,
            &mut NoCallbacks,
        )
    };
    clone(&ours, &CloneOptions::default()).unwrap();
    assert_eq!(files(&ours), files(&theirs));
    assert_eq!(std::fs::read(ours.join("x.u")).unwrap(), b"hello\r\n");
    assert_eq!(
        std::fs::read(ours.join("a.txt")).unwrap(),
        b"one\r\ntwo\r\n"
    );

    // Without filters, checkout writes the stored bytes.
    let raw = root.join("raw");
    clone(&raw, &CloneOptions::default().with_filter_commands(None)).unwrap();
    assert_eq!(std::fs::read(raw.join("x.u")).unwrap(), b"HELLO\n");

    // Refusing commands fails the checkout rather than writing unconverted files.
    let refused = root.join("refused");
    let options = CloneOptions::default().with_filter_commands(Some(Commands::Refuse));
    assert!(clone(&refused, &options).is_err());
}

/// `attr.tree` chooses the attributes for clone checkout instead of the checked-out tree, as Git
/// uses it.
#[test]
fn attribute_tree_override_matches_git_clone() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = root.join("src");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main", "."], b"");
    std::fs::write(source.join(".gitattributes"), "*.txt -text\n").unwrap();
    std::fs::write(source.join("a.txt"), b"one\ntwo\n").unwrap();
    git(&source, &["add", "."], b"");
    git(&source, &["commit", "-q", "-m", "binary"], b"");
    git(&source, &["checkout", "-q", "-b", "attrs"], b"");
    std::fs::write(source.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
    git(&source, &["commit", "-q", "-am", "crlf"], b"");
    git(&source, &["checkout", "-q", "main"], b"");

    let global = root.join("global-config");
    std::fs::write(&global, "[attr]\n\ttree = refs/remotes/origin/attrs\n").unwrap();
    let theirs = root.join("theirs");
    let output = std::process::Command::new("git")
        .args([
            "clone",
            "-q",
            source.to_str().unwrap(),
            theirs.to_str().unwrap(),
        ])
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success());

    let environment = Environment::default()
        .with("GIT_CONFIG_GLOBAL", &global)
        .with("GIT_CONFIG_NOSYSTEM", "1")
        .with("GIT_ATTR_NOSYSTEM", "1")
        .with("HOME", root);
    let inputs = ConfigInputs::git_default(&environment).unwrap();
    let ours = root.join("ours");
    workflow::clone_with(
        source.to_str().unwrap(),
        &ours,
        &CloneOptions::default(),
        &inputs,
        &environment,
        &mut NoCallbacks,
    )
    .unwrap();
    assert_eq!(files(&ours), files(&theirs));
}
