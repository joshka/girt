//! CLI commands on large local repositories with the library's default object-storage limits.
//!
//! Set `GIRT_LARGE_FIXTURES` to a directory holding `linux-v6.1.git`; it defaults to
//! `$HOME/tmp/girt-fixtures`. Run with `cargo test --release --test large_fixtures -- --ignored`.

use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    let root = std::env::var_os("GIRT_LARGE_FIXTURES").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join("tmp/girt-fixtures"),
        PathBuf::from,
    );
    let path = root.join(name);
    assert!(path.is_dir(), "missing large fixture {}", path.display());
    path
}

/// `ref set` checks its target through `Repository::objects`, which opens the Linux kernel's
/// 300 MB pack index. The scratch repository borrows the fixture's objects through alternates,
/// so the fixture is never modified.
#[test]
#[ignore = "needs the Linux kernel large fixture"]
fn ref_set_checks_target_in_linux_sized_store() {
    let source = fixture("linux-v6.1.git");
    let scratch = tempfile::tempdir().unwrap();
    let repository = scratch.path().join("repo.git");
    let git = |args: &[&str]| {
        let output = Command::new("git").args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q", "--bare", repository.to_str().unwrap()]);
    std::fs::write(
        repository.join("objects/info/alternates"),
        format!("{}\n", source.join("objects").display()),
    )
    .unwrap();
    let head = git(&["--git-dir", source.to_str().unwrap(), "rev-parse", "HEAD"]);
    let head = head.trim();
    let output = Command::new(env!("CARGO_BIN_EXE_girt"))
        .arg("-C")
        .arg(&repository)
        .args(["ref", "set", "refs/heads/main", head])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stored = git(&[
        "--git-dir",
        repository.to_str().unwrap(),
        "rev-parse",
        "refs/heads/main",
    ]);
    assert_eq!(stored.trim(), head);
}
