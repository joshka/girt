use std::fs;
use std::sync::atomic::AtomicBool;

use rstest::rstest;

use super::reftable::{Limits, RefRecord, Table};
use super::{Backend, RefName, Reference, ReferenceError, Target};
use crate::{InitKind, ObjectFormat, ObjectId, Repository};

fn identities(format: ObjectFormat) -> (ObjectId, ObjectId) {
    (
        ObjectId::for_blob(format, b"annotation"),
        ObjectId::for_blob(format, b"peeled"),
    )
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn packed_observation_owns_matching_hint_and_loose_shadows_it(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let refs = repo.references().unwrap();
    let name = RefName::new("refs/tags/example").unwrap();
    let (target, peeled) = identities(format);
    assert_eq!(refs.read_observation(&name).unwrap(), None);
    let packed = repo.git_dir().join("packed-refs");
    fs::write(&packed, format!("{target} refs/tags/example\n^{peeled}\n")).unwrap();
    let observed = refs.read_observation(&name).unwrap().unwrap();
    assert_eq!(observed.name, name);
    assert_eq!(observed.target, Target::Direct(target));
    assert_eq!(observed.peeled_hint, Some(peeled));
    assert_eq!(
        refs.list_observations_controlled(100, 100_000, &AtomicBool::new(false))
            .unwrap(),
        vec![observed.clone()]
    );
    // Existing public struct literals and target-only APIs retain their shape and meaning.
    assert_eq!(refs.read(&name).unwrap(), Some(observed.target.clone()));
    assert_eq!(
        refs.list().unwrap(),
        vec![Reference {
            name: name.clone(),
            target: observed.target.clone()
        }]
    );

    fs::create_dir_all(repo.git_dir().join("refs/tags")).unwrap();
    let loose = repo.git_dir().join("refs/tags/example");
    // Even an equal loose target must not borrow the packed record's hint.
    fs::write(&loose, format!("{target}\n")).unwrap();
    let loose_observation = refs.read_observation(&name).unwrap().unwrap();
    assert_eq!(loose_observation.target, observed.target);
    assert_eq!(loose_observation.peeled_hint, None);
    assert_eq!(
        refs.list_observations_controlled(100, 100_000, &AtomicBool::new(false))
            .unwrap(),
        vec![loose_observation]
    );
    fs::write(&loose, b"ref: refs/heads/missing\n").unwrap();
    let symbolic = refs.read_observation(&name).unwrap().unwrap();
    assert_eq!(
        symbolic.target,
        Target::Symbolic(RefName::new("refs/heads/missing").unwrap())
    );
    assert_eq!(symbolic.peeled_hint, None);
    assert_eq!(
        refs.list_observations_controlled(100, 100_000, &AtomicBool::new(false))
            .unwrap(),
        vec![symbolic]
    );
    fs::write(&loose, b"malformed\n").unwrap();
    assert!(matches!(
        refs.read_observation(&name),
        Err(ReferenceError::Malformed { .. })
    ));
    fs::remove_file(&loose).unwrap();
    // Replacing storage cannot alter an already returned target/hint pair.
    fs::write(&packed, format!("{peeled} refs/tags/example\n")).unwrap();
    let changed = refs.read_observation(&name).unwrap().unwrap();
    assert_eq!(changed.target, Target::Direct(peeled));
    assert_eq!(changed.peeled_hint, None);
    assert_eq!(observed.target, Target::Direct(target));
    assert_eq!(observed.peeled_hint, Some(peeled));
    fs::write(&packed, format!("{target} refs/tags/example\n^broken\n")).unwrap();
    assert!(matches!(
        refs.read_observation(&name),
        Err(ReferenceError::Malformed { .. })
    ));
    // A loose read does not newly validate unrelated/broken packed storage.
    fs::write(&loose, format!("{target}\n")).unwrap();
    assert_eq!(
        refs.read_observation(&name).unwrap().unwrap().peeled_hint,
        None
    );
}

#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn reftable_observation_uses_winning_record_including_tombstones(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init_with_backend(
        format,
        root.path().join("repo"),
        InitKind::Bare,
        Backend::Reftable,
    )
    .unwrap();
    let refs = repo.references().unwrap();
    let name = RefName::new("refs/tags/example").unwrap();
    let (target, peeled) = identities(format);
    let mut tables = String::new();
    let mut first = None;
    for (index, (value, hint)) in [
        (Some(Target::Direct(target)), Some(peeled)),
        (Some(Target::Direct(peeled)), None),
        (
            Some(Target::Symbolic(
                RefName::new("refs/heads/missing").unwrap(),
            )),
            None,
        ),
        (None, None),
    ]
    .into_iter()
    .enumerate()
    {
        let update = index as u64 + 1;
        let table = Table {
            format,
            min_update_index: update,
            max_update_index: update,
            references: vec![RefRecord {
                name: name.clone().into(),
                update_index: update,
                target: value.clone(),
                peeled: hint,
            }],
            logs: Vec::new(),
        };
        let filename = format!("{update}.ref");
        fs::write(
            repo.git_dir().join("reftable").join(&filename),
            table.encode(Limits::default()).unwrap(),
        )
        .unwrap();
        tables.push_str(&format!("{filename}\n"));
        fs::write(repo.git_dir().join("reftable/tables.list"), &tables).unwrap();
        let observed = refs.read_observation(&name).unwrap();
        assert_eq!(
            observed.as_ref().map(|record| &record.target),
            value.as_ref()
        );
        assert_eq!(
            observed.as_ref().and_then(|record| record.peeled_hint),
            hint
        );
        assert_eq!(refs.read(&name).unwrap(), value);
        assert_eq!(
            refs.list_observations_controlled(100, 100_000, &AtomicBool::new(false))
                .unwrap(),
            observed.iter().cloned().collect::<Vec<_>>()
        );
        if index == 0 {
            first = observed;
        }
    }
    let first = first.unwrap();
    assert_eq!(first.target, Target::Direct(target));
    assert_eq!(first.peeled_hint, Some(peeled));
    assert!(refs.list().unwrap().is_empty());
}

// Original fixture produced through installed Git, with no annotation object left to peel.
#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn git_packed_hint_survives_missing_annotation_object(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let empty_config = root.path().join("oracle-config");
    fs::write(&empty_config, b"").unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.git_dir())
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &empty_config)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let tree = git(&["mktree"]);
    let commit = git(&["commit-tree", &tree, "-m", "fixture"]);
    git(&["tag", "-a", "example", &commit, "-m", "annotation"]);
    let annotation = git(&["rev-parse", "refs/tags/example"]);
    git(&["pack-refs", "--all"]);
    fs::remove_file(
        repo.git_dir()
            .join("objects")
            .join(&annotation[..2])
            .join(&annotation[2..]),
    )
    .unwrap();
    let refs = repo.references().unwrap();
    let observed = refs
        .read_observation(&RefName::new("refs/tags/example").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(observed.target, Target::Direct(annotation.parse().unwrap()));
    assert_eq!(observed.peeled_hint, Some(commit.parse().unwrap()));
    assert_eq!(
        refs.list_observations_controlled(100, 100_000, &AtomicBool::new(false))
            .unwrap(),
        vec![observed]
    );
}

#[rstest]
#[case(ObjectFormat::Sha1, Backend::Files)]
#[case(ObjectFormat::Sha256, Backend::Files)]
#[case(ObjectFormat::Sha1, Backend::Reftable)]
#[case(ObjectFormat::Sha256, Backend::Reftable)]
fn observation_inventory_preserves_limits_order_and_cancellation(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    let root = tempfile::tempdir().unwrap();
    let repo =
        Repository::init_with_backend(format, root.path().join("repo"), InitKind::Bare, backend)
            .unwrap();
    let refs = repo.references().unwrap();
    let (target, _) = identities(format);
    for name in ["refs/tags/z", "refs/heads/a"] {
        refs.update_without_reflog(
            &RefName::new(name).unwrap(),
            Target::Direct(target),
            super::Expected::Absent,
        )
        .unwrap();
    }
    let cancel = AtomicBool::new(false);
    let inventory = refs
        .list_observations_controlled(100, 100_000, &cancel)
        .unwrap();
    assert_eq!(
        inventory
            .iter()
            .map(|entry| entry.name.as_bytes())
            .collect::<Vec<_>>(),
        vec![b"refs/heads/a".as_slice(), b"refs/tags/z".as_slice()]
    );
    assert!(inventory.iter().all(|entry| entry.peeled_hint.is_none()));
    assert!(matches!(
        refs.list_observations_controlled(0, 100_000, &cancel),
        Err(ReferenceError::Limit(_))
    ));
    if backend == Backend::Files {
        assert!(matches!(
            refs.list_observations_controlled(100, 0, &cancel),
            Err(ReferenceError::Limit(_))
        ));
    }
    assert!(matches!(
        refs.list_observations_controlled(100, 100_000, &AtomicBool::new(true)),
        Err(ReferenceError::Cancelled)
    ));
    assert_eq!(
        refs.list_namespace(&RefName::new("refs/tags").unwrap())
            .unwrap(),
        vec![Reference {
            name: RefName::new("refs/tags/z").unwrap(),
            target: Target::Direct(target)
        }]
    );
}

/// Namespace listing keeps packed hints and excludes siblings that share a name prefix.
#[rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn namespace_observations_keep_hints_within_namespace(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let refs = repo.references().unwrap();
    let (target, peeled) = identities(format);
    fs::write(
        repo.git_dir().join("packed-refs"),
        format!("{target} refs/tags/example\n^{peeled}\n{target} refs/tagsuffix/other\n"),
    )
    .unwrap();
    let observations = refs
        .list_namespace_observations(&RefName::new("refs/tags").unwrap())
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].name,
        RefName::new("refs/tags/example").unwrap()
    );
    assert_eq!(observations[0].peeled_hint, Some(peeled));
    assert!(matches!(
        refs.list_namespace_observations(&RefName::new("HEAD").unwrap()),
        Err(ReferenceError::Unsupported(_))
    ));
}
