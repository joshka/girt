//! Edit one local file, then explicitly refresh the effective repository snapshot.
use girt::config::EditError;
use girt::remote::{Remote, RemoteConfig, RemoteKey};
use girt::{InitKind, ObjectFormat, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo"),
        InitKind::Bare,
    )?;
    let mut edit = repository.edit_config(1024 * 1024)?;
    let mut remotes = RemoteConfig::new(edit.document_mut());
    remotes.add(
        b"origin",
        b"https://example.invalid/project",
        &[b"+refs/heads/*:refs/remotes/origin/*"],
    )?;
    remotes.append(
        b"origin",
        RemoteKey::PushUrl,
        b"ssh://example.invalid/project",
    )?;
    remotes.rename(b"origin", b"upstream")?;
    match edit.commit() {
        Ok(()) => {}
        Err(EditError::Cleanup { operation, cleanup }) => {
            // Inspect the residual lock before removing it; never blindly retry publication.
            return Err(format!("{operation}; manual lock recovery required: {cleanup}").into());
        }
        Err(error) => return Err(error.into()),
    }
    // This example supplied no globals. With open_with_config, reuse those explicit inputs here.
    // Included/global remotes can survive a local removal; success only describes the local file.
    let refreshed = Repository::open(directory.path().join("repo"))?;
    let remote = Remote::find(refreshed.config(), b"upstream")?.expect("published local remote");
    assert_eq!(remote.urls().len(), 1);
    assert_eq!(remote.configured_push_urls().len(), 1);
    directory.close()?;
    Ok(())
}
