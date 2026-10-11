// Fault model: unsynchronized directory changes may persist in either order. Replay the
// index unlink as lost only if no directory sync separated it from the pack unlink.
// This tests the algorithm under that model, not power-loss behavior of a particular filesystem.
#[test]
fn astra_unsynced_index_reappearance_does_not_break_cold_open() {
    use std::cell::Cell;
    let (root, repository, mut policy, old_index) = fixture();
    let extra = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"crash probe extra",
    );
    policy.heads.push(extra.parse().unwrap());
    let saved_index = fs::read(&old_index).unwrap();
    let pending_index_unlink = Cell::new(false);
    let replayed = Cell::new(false);
    let result = repository.retire_old_packs_at(
        &policy,
        RepackLimits::default(),
        &AtomicBool::new(false),
        || {},
        |path| {
            fs::remove_file(path)?;
            if path == old_index {
                pending_index_unlink.set(true);
            }
            if path == old_index.with_extension("pack") && pending_index_unlink.get() {
                fs::write(&old_index, &saved_index)?;
                replayed.set(true);
                return Err(io::Error::other("simulated crash persistence cut"));
            }
            Ok(())
        },
        |directory| {
            File::open(directory)?.sync_all()?;
            pending_index_unlink.set(false);
            Ok(())
        },
    );
    eprintln!(
        "persistence replay={}, retirement={result:?}",
        replayed.get()
    );
    assert!(
        !old_index.with_extension("pack").exists(),
        "fixture must retire the original pack"
    );
    let fresh = Repository::open(root.path()).unwrap();
    assert!(
        fresh.objects(crate::PackLimits::default()).is_ok(),
        "cold open failed after unsynced index unlink was lost but pack unlink persisted"
    );
}
