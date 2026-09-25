//! Original Git executable observations retained in docs/evidence/r02-git-observations.json.
use girt::{ObjectFormat, ObjectId, ObjectKind};
use rstest::rstest;

#[rstest]
#[case::sha1_empty_blob(ObjectFormat::Sha1, ObjectKind::Blob, vec![], "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")]
#[case::sha1_text_blob(ObjectFormat::Sha1, ObjectKind::Blob, b"hello\n".to_vec(), "ce013625030ba8dba906f756967f9e9ca394464a")]
#[case::sha1_binary_blob(ObjectFormat::Sha1, ObjectKind::Blob, (0..=255).collect(), "c86626638e0bc8cf47ca49bb1525b40e9737ee64")]
#[case::sha1_empty_tree(ObjectFormat::Sha1, ObjectKind::Tree, vec![], "4b825dc642cb6eb9a060e54bf8d69288fbee4904")]
#[case::sha1_literal_commit(ObjectFormat::Sha1, ObjectKind::Commit, b"arbitrary\0payload".to_vec(), "2fdd56d084c43db6ace473a47c56ae5bf1d33f2a")]
#[case::sha1_literal_tag(ObjectFormat::Sha1, ObjectKind::Tag, b"arbitrary\xffpayload".to_vec(), "cbe1f6c7e12407841662c51d99bc830f0de080aa")]
#[case::sha256_empty_blob(ObjectFormat::Sha256, ObjectKind::Blob, vec![], "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813")]
#[case::sha256_text_blob(ObjectFormat::Sha256, ObjectKind::Blob, b"hello\n".to_vec(), "2cf8d83d9ee29543b34a87727421fdecb7e3f3a183d337639025de576db9ebb4")]
#[case::sha256_binary_blob(ObjectFormat::Sha256, ObjectKind::Blob, (0..=255).collect(), "a48b8cb64916e81947ff26bb9e96eec5228bbca67e70154027bcba7544c0d308")]
#[case::sha256_empty_tree(ObjectFormat::Sha256, ObjectKind::Tree, vec![], "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321")]
#[case::sha256_literal_commit(ObjectFormat::Sha256, ObjectKind::Commit, b"arbitrary\0payload".to_vec(), "d1ebd4381082265f3c44aa41e72d4af260071310487916ed135624957e0208f9")]
#[case::sha256_literal_tag(ObjectFormat::Sha256, ObjectKind::Tag, b"arbitrary\xffpayload".to_vec(), "d5ce7830e6fb27ecfb604d0201ad2ad89a615636c9d1feff687699a537c98e51")]
fn matches_independent_git_vectors(
    #[case] format: ObjectFormat,
    #[case] kind: ObjectKind,
    #[case] bytes: Vec<u8>,
    #[case] expected: &str,
) {
    let id = format.hash_object(kind, &bytes);
    assert_eq!(id.to_string(), expected);
    assert_eq!(id.format(), format);
    assert_eq!(ObjectId::from_hex(format, expected).unwrap(), id);
}

#[test]
fn sha1_repository_refuses_sha256_read_and_checkout_before_locking() {
    use std::sync::atomic::AtomicBool;

    use girt::{InitKind, PackLimits, ReadLimits, Repository};

    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(root.path().join("repo"), InitKind::Worktree).unwrap();
    let id = ObjectId::Sha256([1; 32]);
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.read(id, ReadLimits::default()),
        Err(girt::ObjectReadError::Loose(girt::Error::ObjectFormat(_)))
    ));
    let lock = repo.git_dir().join("index.lock");
    std::fs::write(&lock, b"another owner").unwrap();
    let failure = repo
        .checkout_tree(
            None,
            Some(id),
            girt::checkout::Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        *failure.cause,
        girt::checkout::Error::ObjectFormat(_)
    ));
    assert_eq!(std::fs::read(lock).unwrap(), b"another owner");
    assert!(!repo.git_dir().join("index").exists());
}
