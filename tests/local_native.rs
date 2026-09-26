//! Native local transfers using original objects and independent Git reads.

use std::ops::ControlFlow;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchError, FetchLimits, receive_local};
use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushLimits, send_local};
use girt::refs::{Backend, Expected, RefName, Target};
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
