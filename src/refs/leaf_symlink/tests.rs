use std::fs;
use std::os::unix::fs::symlink;

use rstest::rstest;

use super::*;
use crate::refs::Expected;
use crate::{InitKind, ObjectFormat, ObjectId, Repository};

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, RefName, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let name = RefName::new("refs/remotes/upstream/HEAD").unwrap();
    fs::create_dir_all(repo.git_dir().join("refs/remotes/upstream")).unwrap();
    let direct = ObjectId::for_blob(format, b"loose");
    let packed = ObjectId::for_blob(format, b"packed");
    (root, repo, name, direct, packed)
}

// Original public gix 0.87.1 lookup probes: a leaf link to a sibling file reads the file's content,
// clears packed hints and retains the requested name. Git's legacy refs/... link text is different.
#[rstest]
#[case(ObjectFormat::Sha1, false)]
#[case(ObjectFormat::Sha256, false)]
#[case(ObjectFormat::Sha1, true)]
#[case(ObjectFormat::Sha256, true)]
fn sibling_link_observes_content_without_enabling_writes(
    #[case] format: ObjectFormat,
    #[case] symbolic: bool,
) {
    let (_root, repo, name, direct, packed) = fixture(format);
    let parent = repo.git_dir().join("refs/remotes/upstream");
    let bytes = if symbolic {
        b"ref: refs/heads/main\n".to_vec()
    } else {
        format!("{direct}\n").into_bytes()
    };
    fs::write(parent.join("trunk"), &bytes).unwrap();
    let alias = parent.join("HEAD");
    symlink("trunk", &alias).unwrap();
    fs::write(
        repo.git_dir().join("packed-refs"),
        format!("{packed} refs/remotes/upstream/HEAD\n^{direct}\n"),
    )
    .unwrap();
    let refs = repo.references().unwrap();
    let observed = refs
        .read_observation_following_leaf_symlinks(&name, 1, 5 + bytes.len())
        .unwrap()
        .unwrap();
    assert_eq!(observed.name, name);
    assert_eq!(
        observed.target,
        if symbolic {
            Target::Symbolic(RefName::new("refs/heads/main").unwrap())
        } else {
            Target::Direct(direct)
        }
    );
    assert_eq!(observed.peeled_hint, None);
    assert!(refs.read(&name).is_err());
    assert!(refs.read_observation(&name).is_err());
    assert!(refs.list().is_err());
    assert!(
        refs.update_without_reflog(&name, Target::Direct(packed), Expected::Any)
            .is_err()
    );
    assert_eq!(fs::read_link(&alias).unwrap(), Path::new("trunk"));
    assert_eq!(fs::read(parent.join("trunk")).unwrap(), bytes);
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&name, 0, 1000),
        Err(ReferenceError::Limit(_))
    ));
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&name, 1, 4 + bytes.len()),
        Err(ReferenceError::Limit(_))
    ));
    fs::write(parent.join("trunk"), format!("{packed}\n")).unwrap();
    assert_ne!(
        observed,
        refs.read_observation_following_leaf_symlinks(&name, 1, 1000)
            .unwrap()
            .unwrap()
    );
}

#[rstest]
#[case(ObjectFormat::Sha1, "missing")]
#[case(ObjectFormat::Sha256, "missing")]
#[case(ObjectFormat::Sha1, "..")]
#[case(ObjectFormat::Sha256, "..")]
fn absent_or_directory_link_target_uses_one_bounded_packed_record(
    #[case] format: ObjectFormat,
    #[case] link: &str,
) {
    let (_root, repo, name, direct, packed) = fixture(format);
    symlink(link, repo.git_dir().join("refs/remotes/upstream/HEAD")).unwrap();
    let refs = repo.references().unwrap();
    assert_eq!(
        refs.read_observation_following_leaf_symlinks(&name, 1, 1000)
            .unwrap(),
        None
    );
    let body = format!("{packed} refs/remotes/upstream/HEAD\n^{direct}\n");
    fs::write(repo.git_dir().join("packed-refs"), &body).unwrap();
    let observed = refs
        .read_observation_following_leaf_symlinks(&name, 1, link.len() + body.len())
        .unwrap()
        .unwrap();
    assert_eq!(observed.target, Target::Direct(packed));
    assert_eq!(observed.peeled_hint, Some(direct));
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&name, 1, link.len() + body.len() - 1),
        Err(ReferenceError::Limit(_))
    ));
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn malformed_loops_and_ancestors_never_fall_back(#[case] format: ObjectFormat) {
    let (_root, repo, name, direct, packed) = fixture(format);
    let parent = repo.git_dir().join("refs/remotes/upstream");
    let alias = parent.join("HEAD");
    fs::write(
        repo.git_dir().join("packed-refs"),
        format!("{packed} refs/remotes/upstream/HEAD\n^{direct}\n"),
    )
    .unwrap();
    symlink("trunk", &alias).unwrap();
    fs::write(parent.join("trunk"), b"malformed\n").unwrap();
    let refs = repo.references().unwrap();
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&name, 3, 1000),
        Err(ReferenceError::Malformed { .. })
    ));
    fs::remove_file(parent.join("trunk")).unwrap();
    symlink("HEAD", parent.join("trunk")).unwrap();
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&name, 3, 1000),
        Err(ReferenceError::Limit("reference filesystem symlink hops"))
    ));
    fs::remove_file(&alias).unwrap();
    symlink("../linked/file", &alias).unwrap();
    symlink(&parent, parent.parent().unwrap().join("linked")).unwrap();
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&name, 3, 1000),
        Err(ReferenceError::Unsupported(_))
    ));
    let ancestor_name = RefName::new("refs/remotes/linked/HEAD").unwrap();
    assert!(matches!(
        refs.read_observation_following_leaf_symlinks(&ancestor_name, 3, 1000),
        Err(ReferenceError::Unsupported(_))
    ));
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn absolute_external_files_and_filesystem_not_git_relative_links(#[case] format: ObjectFormat) {
    let (_root, repo, name, direct, _) = fixture(format);
    let external = repo.git_dir().parent().unwrap().join("external");
    fs::write(&external, format!("{direct}\n")).unwrap();
    let alias = repo.git_dir().join("refs/remotes/upstream/HEAD");
    symlink(&external, &alias).unwrap();
    let refs = repo.references().unwrap();
    assert_eq!(
        refs.read_observation_following_leaf_symlinks(&name, 1, 1000)
            .unwrap()
            .unwrap()
            .target,
        Target::Direct(direct)
    );
    fs::remove_file(&alias).unwrap();
    fs::write(
        repo.git_dir().join("refs/heads/main"),
        format!("{direct}\n"),
    )
    .unwrap();
    symlink("refs/heads/main", &alias).unwrap();
    assert_eq!(
        refs.read_observation_following_leaf_symlinks(&name, 1, 1000)
            .unwrap(),
        None
    );
    fs::remove_file(repo.git_dir().join("HEAD")).unwrap();
    symlink("missing", repo.git_dir().join("HEAD")).unwrap();
    fs::write(
        repo.git_dir().join("packed-refs"),
        b"malformed unrelated packed data\n",
    )
    .unwrap();
    assert_eq!(
        refs.read_observation_following_leaf_symlinks(&RefName::new("HEAD").unwrap(), 1, 1000)
            .unwrap(),
        None
    );
}
