//! Native local transfers using original objects and independent Git reads.

use std::ops::ControlFlow;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
use girt::fetch::{FetchError, FetchLimits, receive_local};
use girt::push::{
    ForcePolicy, PreparedPush, PushCommand, PushError, PushFailure, PushLimits, RejectionOrigin,
    send_local,
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
    let command = PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
        .with_force(ForcePolicy::FastForwardOnly);
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default()).unwrap();
    let prepared =
        PreparedPush::new(&objects, vec![command], &[], PushLimits::default(), &cancel).unwrap();
    assert!(matches!(
        send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&prepared),
            girt::transport::TransportControl::new(&cancel),
            |_| {}
        ),
        Err(PushError::NotSent(PushFailure::Identity))
    ));
    let identity = Signature {
        name: b"Pusher".to_vec(),
        email: b"push@example.invalid".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared).with_identity(Some(&identity)),
        TransportControl::new(&cancel),
        |_| {},
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
    let deletion = {
        let input = PushCommand::new(
            RefName::new("refs/heads/main").unwrap(),
            Some(id),
            girt::ObjectId::null(format),
        );
        input.with_force(ForcePolicy::FastForwardOnly)
    };
    let prepared = PreparedPush::new(
        &objects,
        vec![deletion],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared).with_identity(Some(&identity)),
        TransportControl::new(&cancel),
        |_| {},
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
    let update = PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
        .with_force(ForcePolicy::FastForwardOnly);
    let prepared =
        PreparedPush::new(&objects, vec![update], &[], PushLimits::default(), &cancel).unwrap();
    let refused = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
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
    let accepted = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared).with_identity(Some(&identity)),
        TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
    assert!(accepted.all_succeeded());
    let deletion = {
        let input = PushCommand::new(
            RefName::new("refs/heads/main").unwrap(),
            Some(id),
            girt::ObjectId::null(ObjectFormat::Sha1),
        );
        input.with_force(ForcePolicy::FastForwardOnly)
    };
    let prepared = PreparedPush::new(
        &objects,
        vec![deletion],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let refused = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
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
    let accepted = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared).with_identity(Some(&identity)),
        TransportControl::new(&cancel),
        |_| {},
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

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn native_push_uses_explicit_inherited_receive_config(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, Backend::Files);
    let dest = repository(&root, "dest", format, Backend::Files);
    let id = commit(&source);
    let name = RefName::new("refs/heads/topic").unwrap();
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default()).unwrap();
    let create = PreparedPush::new(
        &objects,
        vec![PushCommand::new(name.clone(), None, id).with_force(ForcePolicy::FastForwardOnly)],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    assert!(
        send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&create),
            girt::transport::TransportControl::new(&cancel),
            |_| {}
        )
        .unwrap()
        .all_succeeded()
    );

    let global_config = root.path().join("global-config");
    std::fs::write(&global_config, b"[receive]\n denyDeletes = true\n").unwrap();
    let config_inputs = ConfigInputs::default().with_files(vec![
        ConfigFile::new(global_config, ConfigScope::Global).with_optional(false),
    ]);
    let delete = PreparedPush::new(
        &objects,
        vec![
            PushCommand::new(name.clone(), Some(id), girt::ObjectId::null(format))
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&delete).with_config_inputs(&config_inputs),
        TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
    assert!(matches!(
        report.refs[0].status,
        Some(girt::push::Status::Rejected(_))
    ));
    assert_eq!(
        report.refs[0].rejection_origin,
        Some(RejectionOrigin::Receiver)
    );
    assert_eq!(
        dest.references().unwrap().read(&name).unwrap(),
        Some(Target::Direct(id))
    );
    assert!(
        send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&delete),
            girt::transport::TransportControl::new(&cancel),
            |_| {}
        )
        .unwrap()
        .all_succeeded()
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
    let commands = ["refs/heads/private/blocked", "refs/heads/private/allowed"].map(|name| {
        PushCommand::new(RefName::new(name).unwrap(), None, id)
            .with_force(ForcePolicy::FastForwardOnly)
    });
    let cancel = AtomicBool::new(false);
    let objects = source.objects(PackLimits::default()).unwrap();
    let prepared = PreparedPush::new(
        &objects,
        commands.to_vec(),
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
    assert!(matches!(
        report.refs[0].status,
        Some(girt::push::Status::Rejected(_))
    ));
    assert_eq!(
        report.refs[0].rejection_origin,
        Some(RejectionOrigin::Receiver)
    );
    assert_eq!(report.refs[1].status, Some(girt::push::Status::Ok));
    let refs = dest.references().unwrap();
    assert_eq!(refs.read(&commands[0].name).unwrap(), None);
    assert_eq!(
        refs.read(&commands[1].name).unwrap(),
        Some(Target::Direct(id))
    );
}

#[rstest]
#[case::sha1_files(ObjectFormat::Sha1, Backend::Files)]
#[case::sha1_reftable(ObjectFormat::Sha1, Backend::Reftable)]
#[case::sha256_files(ObjectFormat::Sha256, Backend::Files)]
#[case::sha256_reftable(ObjectFormat::Sha256, Backend::Reftable)]
fn native_stale_expectation_is_distinct_from_receiver_rejection(
    #[case] format: ObjectFormat,
    #[case] backend: Backend,
) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, backend);
    let dest = repository(&root, "dest", format, backend);
    let id = commit(&source);
    let stale = RefName::new("refs/heads/stale").unwrap();
    let accepted = RefName::new("refs/heads/accepted").unwrap();
    let commands = vec![
        PushCommand::new(stale.clone(), Some(id), id).with_force(ForcePolicy::FastForwardOnly),
        PushCommand::new(accepted.clone(), None, id).with_force(ForcePolicy::FastForwardOnly),
    ];
    let cancel = AtomicBool::new(false);
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        commands,
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
    assert!(matches!(
        report.refs[0].status,
        Some(girt::push::Status::Rejected(_))
    ));
    assert_eq!(
        report.refs[0].rejection_origin,
        Some(RejectionOrigin::ExpectedValue)
    );
    assert_eq!(report.refs[1].status, Some(girt::push::Status::Ok));
    assert_eq!(report.refs[1].rejection_origin, None);
    let refs = dest.references().unwrap();
    assert_eq!(refs.read(&stale).unwrap(), None);
    assert_eq!(refs.read(&accepted).unwrap(), Some(Target::Direct(id)));
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
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        vec![
            PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
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
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared).with_identity(Some(&identity)),
        TransportControl::new(&cancel),
        |_| {},
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
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        vec![
            PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &[],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    assert!(matches!(
        send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&prepared),
            girt::transport::TransportControl::new(&cancel),
            |_| {}
        ),
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
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        girt::fetch::LocalFetchObserver::default(),
    )
    .unwrap();
    assert_eq!(received.wants(), &[id]);
    received
        .install(&fetched, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(git_cat_file(&fetched, id), git_cat_file(&source, id));
    let command = PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, id)
        .with_force(ForcePolicy::FastForwardOnly);
    let objects = source.objects(PackLimits::default()).unwrap();
    let prepared = PreparedPush::new(
        &objects,
        vec![command],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = send_local(
        pushed.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
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
    let old = PushCommand::new(RefName::new("refs/for/main").unwrap(), None, id)
        .with_force(ForcePolicy::FastForwardOnly);
    let prepared =
        PreparedPush::new(&objects, vec![old], &[], PushLimits::default(), &cancel).unwrap();
    assert!(
        send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&prepared),
            girt::transport::TransportControl::new(&cancel),
            |_| {}
        )
        .unwrap()
        .all_succeeded()
    );

    let commands = vec![
        PushCommand::new(
            RefName::new("refs/for/main").unwrap(),
            Some(id),
            girt::ObjectId::null(format),
        )
        .with_force(ForcePolicy::FastForwardOnly),
        PushCommand::new(RefName::new("refs/tags/main").unwrap(), None, id)
            .with_force(ForcePolicy::FastForwardOnly),
    ];
    let prepared =
        PreparedPush::new(&objects, commands, &[], PushLimits::default(), &cancel).unwrap();
    assert!(
        send_local(
            dest.git_dir(),
            girt::push::LocalPushRequest::new(&prepared),
            girt::transport::TransportControl::new(&cancel),
            |_| {}
        )
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
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        girt::fetch::LocalFetchObserver::default(),
    );
    assert!(matches!(result, Err(FetchError::Missing(missing)) if missing == id));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn native_fetch_materializes_absent_canonical_empty_tree(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, Backend::Files);
    let destination = repository(&root, "destination", format, Backend::Files);
    let id = commit(&source);
    let empty_tree = Tree::new(format, vec![]).unwrap().id();
    let hex = empty_tree.to_string();
    std::fs::remove_file(source.object_dir().join(&hex[..2]).join(&hex[2..])).unwrap();

    let cancel = AtomicBool::new(false);
    let received = receive_local(
        source.git_dir(),
        |_| vec![id],
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        girt::transport::TransportControl::new(&cancel),
        girt::fetch::LocalFetchObserver::default(),
    )
    .unwrap();
    received
        .install(&destination, PackLimits::default(), &cancel)
        .unwrap();
    let tree = destination
        .objects(PackLimits::default())
        .unwrap()
        .read(empty_tree, Default::default())
        .unwrap()
        .unwrap();
    assert_eq!(tree.kind(), girt::ObjectKind::Tree);
    assert!(tree.data().is_empty());
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
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        girt::fetch::LocalFetchObserver::default(),
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
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        girt::fetch::LocalFetchObserver::default(),
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
        girt::fetch::LocalFetchRequest::new(FetchLimits::default()),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        girt::fetch::LocalFetchObserver::default().with_messages(&mut |_| ControlFlow::Break(())),
    );
    assert!(matches!(result, Err(FetchError::Cancelled)));
}

/// Git treats the canonical empty tree as present everywhere, so a push whose history reaches it
/// must succeed or reject per reference when neither repository stores it.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn native_push_accepts_history_reaching_unstored_empty_tree(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, Backend::Files);
    let dest = repository(&root, "dest", format, Backend::Files);
    let empty_tree = Tree::new(format, vec![]).unwrap().id();
    let person = Signature {
        name: b"Local Test".to_vec(),
        email: b"local@example.invalid".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let write = |repository: &Repository, parents, message: &[u8]| {
        let commit = Commit::new(CommitFields {
            tree: empty_tree,
            parents,
            author: person.clone(),
            committer: person.clone(),
            extra_headers: vec![],
            message: message.to_vec(),
        })
        .unwrap();
        repository.loose_objects().write_commit(&commit).unwrap()
    };
    // Both repositories store the commits but not the tree, as when another tool writes them.
    let main = RefName::new("refs/heads/main").unwrap();
    let mut parent = None;
    let mut child = None;
    for repository in [&source, &dest] {
        let first = write(repository, vec![], b"Parent\n");
        let second = write(repository, vec![first], b"Child\n");
        repository
            .references()
            .unwrap()
            .update_without_reflog(&main, Target::Direct(second), Expected::Absent)
            .unwrap();
        (parent, child) = (Some(first), Some(second));
    }
    let (parent, child) = (parent.unwrap(), child.unwrap());

    // `parent` is stored by the destination but isn't a reference tip, so the pack is empty and
    // validation walks `parent` down to the unstored empty tree.
    let copy = RefName::new("refs/heads/copy").unwrap();
    let commands = vec![
        PushCommand::new(main.clone(), None, parent).with_force(ForcePolicy::FastForwardOnly),
        PushCommand::new(copy.clone(), None, parent).with_force(ForcePolicy::FastForwardOnly),
    ];
    let cancel = AtomicBool::new(false);
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        commands,
        &[child],
        PushLimits::default(),
        &cancel,
    )
    .unwrap();
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&cancel),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        report.refs[0].rejection_origin,
        Some(RejectionOrigin::ExpectedValue)
    );
    assert_eq!(report.refs[1].status, Some(girt::push::Status::Ok));
    let refs = dest.references().unwrap();
    assert_eq!(refs.read(&main).unwrap(), Some(Target::Direct(child)));
    assert_eq!(refs.read(&copy).unwrap(), Some(Target::Direct(parent)));
    // The accepted update depends on the tree, so the destination now stores it.
    let hex = empty_tree.to_string();
    assert!(dest.object_dir().join(&hex[..2]).join(&hex[2..]).exists());
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(plain_local_path(dest.git_dir()))
        .args(["fsck", "--connectivity-only"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn receive_hooks_on_unsupported_platform_refuse_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", ObjectFormat::Sha1, Backend::Files);
    let destination = repository(&root, "destination", ObjectFormat::Sha1, Backend::Files);
    let id = commit(&source);
    let path = destination.git_dir().join("hooks/pre-receive");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"hook must not execute").unwrap();
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        vec![
            PushCommand::new(RefName::new(b"refs/heads/main").unwrap(), None, id)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &[],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let error = send_local(
        destination.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        |_| {},
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PushError::NotSent(PushFailure::Unsupported(
            "receive hook execution on this platform"
        ))
    ));
    assert!(
        destination
            .objects(PackLimits::default())
            .unwrap()
            .read(id, Default::default())
            .unwrap()
            .is_none()
    );
    assert!(
        destination
            .references()
            .unwrap()
            .read(&RefName::new(b"refs/heads/main").unwrap())
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
fn git_unstored_empty_tree_commit(
    repository: &Repository,
    tree: girt::ObjectId,
    parent: Option<girt::ObjectId>,
) -> girt::ObjectId {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .arg("--git-dir")
        .arg(repository.git_dir())
        .args([
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree.to_string(),
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            repository.git_dir().join("absent-config"),
        )
        .env("GIT_AUTHOR_NAME", "Local Test")
        .env("GIT_AUTHOR_EMAIL", "local@example.invalid")
        .env("GIT_COMMITTER_NAME", "Local Test")
        .env("GIT_COMMITTER_EMAIL", "local@example.invalid")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .stdin(std::process::Stdio::null());
    if let Some(parent) = parent {
        command.args(["-p", &parent.to_string()]);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(unix)]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn pre_receive_rejection_does_not_materialize_unstored_empty_tree(#[case] format: ObjectFormat) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let source = repository(&root, "source", format, Backend::Files);
    let dest = repository(&root, "destination", format, Backend::Files);
    let tree = Tree::new(format, vec![]).unwrap().id();
    let parent = git_unstored_empty_tree_commit(&source, tree, None);
    let child = git_unstored_empty_tree_commit(&source, tree, Some(parent));
    let parent_commit = Commit::parse(format, &git_cat_file(&source, parent)).unwrap();
    let child_commit = Commit::parse(format, &git_cat_file(&source, child)).unwrap();
    assert_eq!(
        dest.loose_objects().write_commit(&parent_commit).unwrap(),
        parent
    );
    assert_eq!(
        dest.loose_objects().write_commit(&child_commit).unwrap(),
        child
    );
    dest.references()
        .unwrap()
        .update_without_reflog(
            &RefName::new(b"refs/heads/main").unwrap(),
            Target::Direct(child),
            Expected::Absent,
        )
        .unwrap();
    let hex = tree.to_string();
    let relative_tree = format!("{}/{}", &hex[..2], &hex[2..]);
    assert!(!dest.object_dir().join(&relative_tree).exists());
    let path = dest.git_dir().join("hooks/pre-receive");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("#!/bin/sh\ntest -f \"$GIT_OBJECT_DIRECTORY/{relative_tree}\" || exit 9\ngit cat-file -e {tree} || exit 9\nprintf visible >hook-observed\ncat >/dev/null\nexit 1\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let prepared = PreparedPush::new(
        &source.objects(PackLimits::default()).unwrap(),
        vec![
            PushCommand::new(RefName::new(b"refs/heads/copy").unwrap(), None, parent)
                .with_force(ForcePolicy::FastForwardOnly),
        ],
        &[child],
        PushLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = send_local(
        dest.git_dir(),
        girt::push::LocalPushRequest::new(&prepared),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        report.refs[0].status,
        Some(girt::push::Status::Rejected(
            b"pre-receive hook declined".to_vec()
        ))
    );
    assert_eq!(
        std::fs::read(dest.git_dir().join("hook-observed")).unwrap(),
        b"visible"
    );
    assert!(!dest.object_dir().join(relative_tree).exists());
    assert!(
        dest.references()
            .unwrap()
            .read(&RefName::new(b"refs/heads/copy").unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        dest.references()
            .unwrap()
            .read(&RefName::new(b"refs/heads/main").unwrap())
            .unwrap(),
        Some(Target::Direct(child))
    );
}
