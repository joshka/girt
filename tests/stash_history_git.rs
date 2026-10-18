//! Destructive stash-history acceptance fixtures, generated independently by Git.
use girt::stash::{History, LogError, LogLimits};
use girt::{ObjectId, Repository};
use rstest::rstest;

#[path = "support/replay_git.rs"]
#[allow(dead_code)]
mod support;
use support::{Fixture, success, writer};

fn stashes() -> Fixture {
    let fixture = Fixture::new("sha1");
    for text in ["first", "second", "third"] {
        std::fs::write(fixture.root.path().join("other"), text).unwrap();
        success(fixture.root.path(), &["stash", "push", "-m", text]);
    }
    fixture
}
fn read(path: &std::path::Path) -> History {
    History::read(&Repository::open(path).unwrap(), LogLimits::default()).unwrap()
}

#[rstest]
#[case::newest(0)]
#[case::middle(1)]
#[case::oldest(2)]
fn drop_preserves_surviving_objects_and_matches_git(#[case] position: usize) {
    let fixture = stashes();
    let path = fixture.root.path();
    let repo = Repository::open(path).unwrap();
    let observed = read(path);
    let log_path = path.join(".git/logs/refs/stash");
    let ref_path = path.join(".git/refs/stash");
    let original_log = std::fs::read(&log_path).unwrap();
    let original_ref = std::fs::read(&ref_path).unwrap();
    success(path, &["stash", "drop", &format!("stash@{{{position}}}")]);
    let expected_log = std::fs::read(&log_path).unwrap();
    let expected_ref = std::fs::read(&ref_path).unwrap();
    std::fs::write(&log_path, original_log).unwrap();
    std::fs::write(&ref_path, original_ref).unwrap();
    observed.drop(position).unwrap().publish(&repo).unwrap();
    assert_eq!(std::fs::read(&log_path).unwrap(), expected_log);
    assert_eq!(std::fs::read(&ref_path).unwrap(), expected_ref);
    let survivors = read(path);
    assert_eq!(survivors.entries().count(), 2);
    let objects = repo.objects().unwrap();
    for entry in survivors.entries() {
        objects.read_commit(entry.new).unwrap();
    }
}

#[test]
fn stale_or_locked_history_preserves_all_bytes() {
    let fixture = stashes();
    let path = fixture.root.path();
    let repo = Repository::open(path).unwrap();
    let plan = read(path).drop(1).unwrap();
    let log_path = path.join(".git/logs/refs/stash");
    let reference = std::fs::read(path.join(".git/refs/stash")).unwrap();
    let log = std::fs::read(&log_path).unwrap();
    let lock = path.join(".git/logs/refs/stash.lock");
    std::fs::write(&lock, b"other writer").unwrap();
    assert!(plan.publish(&repo).is_err());
    assert_eq!(std::fs::read(&lock).unwrap(), b"other writer");
    assert_eq!(std::fs::read(&log_path).unwrap(), log);
    std::fs::remove_file(lock).unwrap();
    std::fs::write(&log_path, [log.as_slice(), log.as_slice()].concat()).unwrap();
    assert!(matches!(plan.publish(&repo), Err(LogError::Changed)));
    assert_eq!(
        std::fs::read(path.join(".git/refs/stash")).unwrap(),
        reference
    );
    assert_eq!(
        std::fs::read(&log_path).unwrap(),
        [log.as_slice(), log.as_slice()].concat()
    );
}

#[test]
fn final_drop_removes_stash_name_and_log_without_deleting_objects() {
    let fixture = stashes();
    let path = fixture.root.path();
    let repo = Repository::open(path).unwrap();
    let ids: Vec<ObjectId> = read(path).entries().map(|entry| entry.new).collect();
    for _ in 0..3 {
        read(path).drop(0).unwrap().publish(&repo).unwrap();
    }
    assert!(!path.join(".git/refs/stash").exists());
    assert!(!path.join(".git/logs/refs/stash").exists());
    for id in ids {
        repo.objects().unwrap().read_commit(id).unwrap();
    }
}

#[test]
fn explicit_push_is_visible_to_git_and_bounded_reads_refuse_truncation() {
    let fixture = stashes();
    let path = fixture.root.path();
    let repo = Repository::open(path).unwrap();
    let history = read(path);
    let id = history.entries().next().unwrap().new;
    history
        .push(id, writer(), b"stored again".to_vec())
        .unwrap()
        .publish(&repo)
        .unwrap();
    let list = success(path, &["stash", "list", "--format=%gs"]);
    assert!(list.starts_with(b"stored again\n"));
    let limits = LogLimits::default().with_max_bytes(0);
    assert!(matches!(History::read(&repo, limits), Err(LogError::Limit)));
}
