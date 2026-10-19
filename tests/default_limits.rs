//! Default resource limits admit real-world large repositories.
//!
//! The ignored tests need local copies of git.git and the Linux kernel (v6.1). Set
//! `GIRT_LARGE_FIXTURES` to a directory holding `git-matrix.git` and `linux-v6.1.git`; it
//! defaults to `$HOME/tmp/girt-fixtures`. Run them with `cargo test --release --test
//! default_limits -- --ignored`.

use std::path::PathBuf;
use std::process::Command;

use girt::{ObjectId, Repository};

fn fixture(name: &str) -> PathBuf {
    let root = std::env::var_os("GIRT_LARGE_FIXTURES").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join("tmp/girt-fixtures"),
        PathBuf::from,
    );
    let path = root.join(name);
    assert!(path.is_dir(), "missing large fixture {}", path.display());
    path
}

fn git_id(repository: &PathBuf, revision: &str) -> ObjectId {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(repository)
        .args(["rev-parse", "--verify", revision])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Defaults stay well below the trusted values, which only leave arithmetic headroom.
const FINITE: u64 = 1 << 40;

#[test]
fn fetch_defaults_stay_finite() {
    let limits = girt::fetch::FetchLimits::default();
    for (name, value) in [
        ("wire bytes", limits.max_wire_bytes),
        ("known objects", limits.max_known_objects),
        ("known bytes", limits.max_known_bytes),
        (
            "verification known bytes",
            girt::fetch::FetchUpdateLimits::default()
                .verification
                .max_known_bytes,
        ),
        ("known edges", limits.max_known_edges),
        ("pack bytes", limits.max_pack_bytes),
        ("objects", limits.max_objects),
        ("object bytes", limits.max_object_bytes),
        ("delta bytes", limits.max_delta_bytes),
        ("decode bytes", limits.max_decode_bytes),
        ("resolution steps", limits.max_resolution_steps),
        ("connectivity edges", limits.max_connectivity_edges),
    ] {
        assert!(value as u64 <= FINITE, "{name}: {value}");
    }
    assert_eq!(limits.max_delta_depth, 64);
}

#[test]
fn storage_defaults_stay_finite() {
    let packs = girt::PackLimits::default();
    assert!(packs.max_index_bytes <= 1 << 30);
    assert!(
        packs.max_index_bytes >= 300 * 1024 * 1024,
        "admits a Linux kernel index"
    );
    let read = girt::ReadLimits::default();
    for value in [
        read.max_object_bytes,
        read.max_delta_bytes,
        read.max_decode_bytes,
        read.max_input_bytes,
    ] {
        assert!(value <= 1 << 30);
    }
    let checkout = girt::checkout::Limits::default();
    assert!(checkout.max_object_bytes as u64 <= 4 << 30);
    assert!(checkout.max_worktree_bytes as u64 <= 8 << 30);
}

#[test]
#[ignore = "needs the git.git large fixture"]
fn default_clone_admits_git_git() {
    let source = fixture("git-matrix.git");
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("git");
    girt::workflow::clone(source.to_str().unwrap(), &destination).unwrap();
    let repository = Repository::open(&destination).unwrap();
    let head = git_id(&source, "HEAD");
    assert!(repository.objects().unwrap().read(head).unwrap().is_some());
}

#[test]
#[ignore = "needs the Linux kernel large fixture"]
fn default_open_reads_linux() {
    let source = fixture("linux-v6.1.git");
    let repository = Repository::open(&source).unwrap();
    let objects = repository.objects().unwrap();
    for revision in [
        "HEAD",
        "HEAD^{tree}",
        "HEAD:MAINTAINERS",
        "HEAD:drivers/gpu/drm/amd/include/asic_reg/dcn/dcn_3_2_0_sh_mask.h",
        "HEAD~50000^{tree}",
        "a101ad945113be3d7f283a181810d76897f0a0d6",
    ] {
        let id = git_id(&source, revision);
        assert!(objects.read(id).unwrap().is_some(), "{revision}");
    }
}

#[test]
#[ignore = "needs the Linux kernel large fixture; slow"]
fn default_clone_admits_linux() {
    let source = fixture("linux-v6.1.git");
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("linux");
    girt::workflow::clone(source.to_str().unwrap(), &destination).unwrap();
    let repository = Repository::open(&destination).unwrap();
    let head = git_id(&source, "HEAD");
    assert!(repository.objects().unwrap().read(head).unwrap().is_some());
}
