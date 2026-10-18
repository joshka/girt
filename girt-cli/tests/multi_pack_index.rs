//! MIDX CLI operations compose the public library APIs against independently generated Git packs.
#[allow(dead_code)]
#[path = "../../tests/support/pack_git.rs"]
mod pack_git;

use std::path::Path;
use std::process::{Command, Output};

fn girt(directory: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_girt"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .args(args)
        .output()
        .unwrap()
}

fn round_trip(format: girt::ObjectFormat) {
    let fixture = pack_git::Fixture::new(format, true, 4);
    let written = girt(fixture.root.path(), &["--git", "multi-pack-index", "write"]);
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    assert!(written.stdout.is_empty());
    pack_git::git(fixture.root.path(), &["multi-pack-index", "verify"], b"");
    let verified = girt(
        fixture.root.path(),
        &["--git", "multi-pack-index", "verify"],
    );
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(verified.stdout.is_empty());
}

#[test]
fn sha1_write_and_verify() {
    round_trip(girt::ObjectFormat::Sha1);
}

#[test]
fn sha256_write_and_verify() {
    round_trip(girt::ObjectFormat::Sha256);
}

#[test]
fn unsupported_bitmap_does_not_publish_index() {
    let fixture = pack_git::Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let output = girt(
        fixture.root.path(),
        &["multi-pack-index", "write", "--bitmap"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bitmap writing is not implemented"));
    assert!(
        !fixture
            .root
            .path()
            .join("objects/pack/multi-pack-index")
            .exists()
    );
}
