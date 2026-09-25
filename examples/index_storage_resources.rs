//! Original split/sparse fixtures and small warm-cache timing/resource observations.
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use girt::index::{Entry, Index, Limits, Mode, SparseLimits};
use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};

fn descriptors() -> Option<usize> {
    #[cfg(target_os = "linux")]
    let path = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    let path = "/dev/fd";
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return std::fs::read_dir(path).ok().map(Iterator::count);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    None
}
fn measure(mut operation: impl FnMut()) -> u128 {
    let mut samples = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        operation();
        samples.push(start.elapsed().as_nanos());
    }
    samples.sort_unstable();
    samples[15]
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for count in [100, 10_000] {
        probe(count)?;
    }
    Ok(())
}
fn probe(count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let repo = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Worktree,
    )?;
    let id = repo.loose_objects().write_blob(b"content")?;
    let entries: Vec<_> = (0..count)
        .map(|n| Entry::new(format!("file-{n:08}").into_bytes(), Mode::Regular, id))
        .collect();
    let mut edit = repo.edit_index(Limits::default())?;
    edit.replace_entries(entries.clone())?;
    edit.commit()?;
    let mut git = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            git.env_remove(key);
        }
    }
    let status = git
        .current_dir(repo.worktree().unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("absent"))
        .args([
            "-c",
            "core.fsmonitor=false",
            "update-index",
            "--split-index",
        ])
        .status()?;
    assert!(status.success());
    let before = descriptors();
    let snapshot = repo.read_index(Limits::default())?.unwrap();
    let retained = descriptors();
    assert_eq!(snapshot.entries().len(), count);
    let shared_id = snapshot.shared_index_id().unwrap();
    let main_bytes = std::fs::metadata(repo.git_dir().join("index"))?.len();
    let shared_bytes =
        std::fs::metadata(repo.git_dir().join(format!("sharedindex.{shared_id}")))?.len();
    let read_ns = measure(|| {
        drop(repo.read_index(Limits::default()).unwrap());
    });
    let publish_ns = measure(|| {
        repo.edit_index(Limits::default())
            .unwrap()
            .commit()
            .unwrap();
    });
    drop(snapshot);
    let after = descriptors();
    let tree = Tree::new(
        ObjectFormat::Sha1,
        entries
            .into_iter()
            .map(|entry| TreeEntry {
                name: entry.path,
                mode: EntryMode::Blob,
                id,
            })
            .collect(),
    )?;
    let tree_id = repo.loose_objects().write_tree(&tree)?;
    let mut directory = Entry::new(b"outside/".to_vec(), Mode::SparseDirectory, tree_id);
    directory.skip_worktree = true;
    let sparse = Index::new(ObjectFormat::Sha1, vec![directory], Limits::default())?;
    let objects = repo.objects(girt::PackLimits::default())?;
    let expand_ns = measure(|| {
        let mut index = sparse.clone();
        index
            .expand_sparse(
                &objects,
                Limits::default(),
                SparseLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(index.entries().len(), count);
    });
    println!(
        "{}",
        serde_json::json!({"entries":count, "samples":30, "main_bytes":main_bytes,
        "shared_bytes":shared_bytes, "read_median_ns":read_ns, "publish_median_ns":publish_ns,
        "expand_median_ns":expand_ns, "fd_before":before, "fd_retained":retained, "fd_after":after})
    );
    Ok(())
}
