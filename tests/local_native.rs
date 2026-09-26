//! Native local transfers using original objects and independent Git reads.

use std::ops::ControlFlow;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchError, FetchLimits, receive_local};
use girt::push::{
    ForcePolicy, PreparedPush, PushCommand, PushError, PushFailure, PushLimits, send_local,
    send_local_with_identity,
};
use girt::refs::{Backend, Expected, RefName, Target};
use girt::transport::TransportControl;
use girt::{
    Commit, CommitFields, Config, InitKind, ObjectFormat, PackLimits, Repository, Signature, Tree,
};
use rstest::rstest;

fn repository(
    root: &tempfile::TempDir,
    name: &str,
    format: ObjectFormat,
    backend: Backend,
) -> Repository {
    Repository::init_with_backend(format, root.path().join(name), InitKind::Bare, backend).unwrap()
}

fn commit(repository: &Repository) -> girt::ObjectId {
    let store = repository.loose_objects();
    let tree = Tree::new(repository.object_format(), vec![]).unwrap();
    store.write_tree(&tree).unwrap();
    let person = Signature {
        name: b"Local Test".to_vec(),
        email: b"local@example.invalid".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit = Commit::new(CommitFields {
        tree: tree.id(),
        parents: vec![],
        author: person.clone(),
        committer: person,
        extra_headers: vec![],
        message: b"Local transfer\n".to_vec(),
    })
    .unwrap();
    let id = store.write_commit(&commit).unwrap();
    let name = RefName::new("refs/heads/main").unwrap();
    repository
        .references()
        .unwrap()
        .update_without_reflog(&name, Target::Direct(id), Expected::Absent)
        .unwrap();
    id
}

fn git_cat_file(repository: &Repository, id: girt::ObjectId) -> Vec<u8> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(plain_local_path(repository.git_dir()))
        .args(["cat-file", "commit", &id.to_string()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn plain_local_path(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    }
    #[cfg(not(windows))]
    {
        value.into_owned()
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn native_push_creates_and_deletes_git_reflog_with_explicit_identity(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, Backend::Files);
    let dest = repository(&root, "dest", format, Backend::Files);
    let id = commit(&source);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(dest.git_dir().join("config"))
        .unwrap()
        .write_all(b"\n[core]\n logAllRefUpdates = true\n")
        .unwrap();
    let command = PushCommand {
        name: RefName::new("refs/heads/main").unwrap(),
        expected: None,
        new: id,
        force: ForcePolicy::FastForwardOnly,
    };
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default()).unwrap();
    let prepared =
        PreparedPush::new_local(&objects, vec![command], &[], PushLimits::default(), &cancel)
            .unwrap();
    assert!(matches!(
        send_local(dest.git_dir(), &prepared, &cancel),
        Err(PushError::NotSent(PushFailure::Identity))
    ));
    let identity = Signature {
        name: b"Pusher".to_vec(),
        email: b"push@example.invalid".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let report = send_local_with_identity(
        dest.git_dir(),
        &prepared,
        TransportControl::new(&cancel),
        &identity,
    )
    .unwrap();
    assert!(report.all_succeeded());
    assert!(report.refs[0].effects.is_some());
    let log = dest
        .references()
        .unwrap()
        .reflog(&RefName::new("refs/heads/main").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].committer, identity);
    assert_eq!(log[0].new, id);
    let git_log = Command::new("git")
        .arg("--git-dir")
        .arg(plain_local_path(dest.git_dir()))
        .args(["reflog", "show", "--format=%H", "refs/heads/main"])
        .output()
        .unwrap();
    assert!(git_log.status.success());
    assert!(git_log.stdout.starts_with(id.to_string().as_bytes()));
    let deletion = PushCommand {
        name: RefName::new("refs/heads/main").unwrap(),
        expected: Some(id),
        new: girt::ObjectId::null(format),
        force: ForcePolicy::FastForwardOnly,
    };
    let prepared = PreparedPush::new_local(
        &objects,
        vec![deletion],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local_with_identity(
        dest.git_dir(),
        &prepared,
        TransportControl::new(&cancel),
        &identity,
    )
    .unwrap();
    assert!(report.all_succeeded());
    assert!(
        dest.references()
            .unwrap()
            .reflog(&RefName::new("refs/heads/main").unwrap())
            .unwrap()
            .is_none()
    );
    let git_log = Command::new("git")
        .arg("--git-dir")
        .arg(plain_local_path(dest.git_dir()))
        .args(["reflog", "exists", "refs/heads/main"])
        .status()
        .unwrap();
    assert!(!git_log.success());
}

#[test]
fn native_current_branch_policies_distinguish_updates_and_deletes() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let dest = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("worktree"),
        InitKind::Worktree,
    )
    .unwrap();
    let id = commit(&source);
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default()).unwrap();
    let update = PushCommand {
        name: RefName::new("refs/heads/main").unwrap(),
        expected: None,
        new: id,
        force: ForcePolicy::FastForwardOnly,
    };
    let prepared =
        PreparedPush::new_local(&objects, vec![update], &[], PushLimits::default(), &cancel)
            .unwrap();
    let refused = send_local(dest.git_dir(), &prepared, &cancel).unwrap();
    assert!(matches!(
        refused.refs[0].status,
        Some(girt::push::Status::Rejected(_))
    ));
    std::fs::OpenOptions::new()
        .append(true)
        .open(dest.git_dir().join("config"))
        .unwrap()
        .write_all(b"\n[receive]\n denyCurrentBranch = ignore\n")
        .unwrap();
    let identity = Signature {
        name: b"Pusher".to_vec(),
        email: b"push@example.invalid".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let accepted = send_local_with_identity(
        dest.git_dir(),
        &prepared,
        TransportControl::new(&cancel),
        &identity,
    )
    .unwrap();
    assert!(accepted.all_succeeded());
    let deletion = PushCommand {
        name: RefName::new("refs/heads/main").unwrap(),
        expected: Some(id),
        new: girt::ObjectId::null(ObjectFormat::Sha1),
        force: ForcePolicy::FastForwardOnly,
    };
    let prepared = PreparedPush::new_local(
        &objects,
        vec![deletion],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let refused = send_local(dest.git_dir(), &prepared, &cancel).unwrap();
    assert!(matches!(
        refused.refs[0].status,
        Some(girt::push::Status::Rejected(_))
    ));
    std::fs::OpenOptions::new()
        .append(true)
        .open(dest.git_dir().join("config"))
        .unwrap()
        .write_all(b" denyDeleteCurrent = false\n")
        .unwrap();
    let accepted = send_local_with_identity(
        dest.git_dir(),
        &prepared,
        TransportControl::new(&cancel),
        &identity,
    )
    .unwrap();
    assert!(accepted.all_succeeded());
    assert!(
        dest.references()
            .unwrap()
            .read(&RefName::new("refs/heads/main").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn native_hidden_ref_rejects_independently_of_visible_ref() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let dest = repository(&root, "dest", ObjectFormat::Sha1, Backend::Files);
    let id = commit(&source);
    std::fs::OpenOptions::new()
        .append(true)
        .open(dest.git_dir().join("config"))
        .unwrap()
        .write_all(b"\n[receive]\n hideRefs = refs/heads/private\n hideRefs = !refs/heads/private/allowed\n")
        .unwrap();
    let commands =
        ["refs/heads/private/blocked", "refs/heads/private/allowed"].map(|name| PushCommand {
            name: RefName::new(name).unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        });
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default()).unwrap();
    let prepared = PreparedPush::new_local(
        &objects,
        commands.to_vec(),
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local(dest.git_dir(), &prepared, &cancel).unwrap();
    assert!(matches!(
        report.refs[0].status,
        Some(girt::push::Status::Rejected(_))
    ));
    assert_eq!(report.refs[1].status, Some(girt::push::Status::Ok));
    let refs = dest.references().unwrap();
    assert_eq!(refs.read(&commands[0].name).unwrap(), None);
    assert_eq!(
        refs.read(&commands[1].name).unwrap(),
        Some(Target::Direct(id))
    );
}

#[test]
fn native_warn_mode_reports_accepted_current_branch_update() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let dest = Repository::init(
        ObjectFormat::Sha1,
        root.path().join("worktree"),
        InitKind::Worktree,
    )
    .unwrap();
    let id = commit(&source);
    std::fs::OpenOptions::new()
        .append(true)
        .open(dest.git_dir().join("config"))
        .unwrap()
        .write_all(b"\n[receive]\n denyCurrentBranch = warn\n")
        .unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = PreparedPush::new_local(
        &source.objects(PackLimits::default()).unwrap(),
        vec![PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        }],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let identity = Signature {
        name: b"Pusher".to_vec(),
        email: b"push@example.invalid".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let report = send_local_with_identity(
        dest.git_dir(),
        &prepared,
        TransportControl::new(&cancel),
        &identity,
    )
    .unwrap();
    assert!(report.all_succeeded());
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(
        dest.references()
            .unwrap()
            .read(&RefName::new("refs/heads/main").unwrap())
            .unwrap(),
        Some(Target::Direct(id))
    );
}

#[rstest]
#[case::deletes("denyDeletes")]
#[case::non_fast_forward("denyNonFastForwards")]
fn invalid_receive_boolean_refuses_before_native_installation(#[case] key: &str) {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let dest = repository(&root, "dest", ObjectFormat::Sha1, Backend::Files);
    let id = commit(&source);
    std::fs::OpenOptions::new()
        .append(true)
        .open(dest.git_dir().join("config"))
        .unwrap()
        .write_all(format!("\n[receive]\n {key} = invalid\n").as_bytes())
        .unwrap();
    let cancel = AtomicBool::new(false);
    let prepared = PreparedPush::new_local(
        &source.objects(PackLimits::default()).unwrap(),
        vec![PushCommand {
            name: RefName::new("refs/heads/main").unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        }],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    assert!(matches!(
        send_local(dest.git_dir(), &prepared, &cancel),
        Err(PushError::NotSent(PushFailure::Unsupported(_)))
    ));
    assert!(
        dest.objects(PackLimits::default())
            .unwrap()
            .read(id, Default::default())
            .unwrap()
            .is_none()
    );
}

#[rstest]
#[case::sha1_files(ObjectFormat::Sha1, Backend::Files)]
#[case::sha1_reftable(ObjectFormat::Sha1, Backend::Reftable)]
#[case::sha256_files(ObjectFormat::Sha256, Backend::Files)]
#[case::sha256_reftable(ObjectFormat::Sha256, Backend::Reftable)]
fn native_fetch_and_push_preserve_git_objects_and_refs(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, backend);
    let fetched = repository(&root, "fetched", format, backend);
    let pushed = repository(&root, "pushed", format, backend);
    let id = commit(&source);
    let received = receive_local(
        source.git_dir(),
        |_| vec![id],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(received.wants(), &[id]);
    received
        .install(&fetched, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(git_cat_file(&fetched, id), git_cat_file(&source, id));
    let command = PushCommand {
        name: RefName::new("refs/heads/main").unwrap(),
        expected: None,
        new: id,
        force: ForcePolicy::FastForwardOnly,
    };
    let objects = source.objects(PackLimits::default()).unwrap();
    let prepared = PreparedPush::new_local(
        &objects,
        vec![command],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = send_local(pushed.git_dir(), &prepared, &AtomicBool::new(false)).unwrap();
    assert!(report.all_succeeded());
    assert_eq!(
        pushed
            .references()
            .unwrap()
            .resolve(&RefName::new("refs/heads/main").unwrap(), 8)
            .unwrap()
            .id,
        Some(id)
    );
    assert_eq!(git_cat_file(&pushed, id), git_cat_file(&source, id));
}

#[rstest]
#[case::sha1_files(ObjectFormat::Sha1, Backend::Files)]
#[case::sha1_reftable(ObjectFormat::Sha1, Backend::Reftable)]
#[case::sha256_files(ObjectFormat::Sha256, Backend::Files)]
#[case::sha256_reftable(ObjectFormat::Sha256, Backend::Reftable)]
fn native_push_deletes_one_ref_and_creates_another(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, backend);
    let dest = repository(&root, "dest", format, backend);
    let id = commit(&source);
    let objects = source.objects(PackLimits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let old = PushCommand {
        name: RefName::new("refs/for/main").unwrap(),
        expected: None,
        new: id,
        force: ForcePolicy::FastForwardOnly,
    };
    let prepared =
        PreparedPush::new_local(&objects, vec![old], &[], PushLimits::default(), &cancel).unwrap();
    assert!(
        send_local(dest.git_dir(), &prepared, &cancel)
            .unwrap()
            .all_succeeded()
    );

    let commands = vec![
        PushCommand {
            name: RefName::new("refs/for/main").unwrap(),
            expected: Some(id),
            new: girt::ObjectId::null(format),
            force: ForcePolicy::FastForwardOnly,
        },
        PushCommand {
            name: RefName::new("refs/tags/main").unwrap(),
            expected: None,
            new: id,
            force: ForcePolicy::FastForwardOnly,
        },
    ];
    let prepared =
        PreparedPush::new_local(&objects, commands, &[], PushLimits::default(), &cancel).unwrap();
    assert!(
        send_local(dest.git_dir(), &prepared, &cancel)
            .unwrap()
            .all_succeeded()
    );
    let refs = dest.references().unwrap();
    assert_eq!(
        refs.read(&RefName::new("refs/for/main").unwrap()).unwrap(),
        None
    );
    assert_eq!(
        refs.read(&RefName::new("refs/tags/main").unwrap()).unwrap(),
        Some(Target::Direct(id))
    );
}

#[test]
fn native_fetch_rejects_missing_source_object_without_installation() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let id = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"missing");
    source
        .references()
        .unwrap()
        .update_without_reflog(
            &RefName::new("refs/heads/main").unwrap(),
            Target::Direct(id),
            Expected::Absent,
        )
        .unwrap();
    let result = receive_local(
        source.git_dir(),
        |_| vec![id],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::Missing(missing)) if missing == id));
}

#[test]
fn resolved_file_url_fetches_from_local_storage() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source path", ObjectFormat::Sha1, Backend::Files);
    let id = commit(&source);
    let path = plain_local_path(source.git_dir())
        .replace('\\', "/")
        .replace(' ', "%20");
    let url = if cfg!(windows) {
        format!("file:///{path}")
    } else {
        format!("file://{path}")
    };
    let config = Config::parse(format!("[remote \"origin\"]\nurl = {url}\n").as_bytes()).unwrap();
    let remote = girt::remote::Remote::find(&config, b"origin")
        .unwrap()
        .unwrap();
    let destination = remote
        .fetch_destination(&config, &Default::default())
        .unwrap();
    let received = receive_local(
        destination.local_path().unwrap(),
        |_| vec![id],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(received.wants(), &[id]);
}

#[test]
fn native_fetch_rejects_corrupt_source_object() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let id = commit(&source);
    let hex = id.to_string();
    let path = source.object_dir().join(&hex[..2]).join(&hex[2..]);
    std::fs::write(path, b"corrupt loose object").unwrap();
    let result = receive_local(
        source.git_dir(),
        |_| vec![id],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(FetchError::LocalRead { id: failed, .. }) if failed == id));
}

#[test]
fn native_fetch_callback_interruption_returns_no_pack() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha256, Backend::Reftable);
    let id = commit(&source);
    let result = receive_local(
        source.git_dir(),
        |_| vec![id],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Break(()),
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
}
