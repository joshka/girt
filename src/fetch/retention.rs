//! Explicit ownership of a newly installed pack's Git retention marker.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::{FetchError, FetchInstalled, ReceivedFetch};
use crate::{ObjectId, PackLimits, Repository};

/// A Git `.keep` marker protecting a newly installed complete fetch pack.
///
/// Keep this handle until all references that should retain the fetched objects have been
/// published. Git repacking honors the marker; it is not a repository lock and does not protect
/// against tools that ignore `.keep`, manual deletion, or concurrent removal of the marker.
/// Callers must own marker removal. Other fetches finding the same marker fail without changing it.
///
/// Dropping the handle deliberately leaves the marker in place. This includes unwinding and
/// installation/publication failures: inspect partial effects before releasing retention. After a
/// crash, the owner may remove the marker at [`Self::path`] when no operation still relies on it.
/// Abandoned markers can retain disk space indefinitely. No automatic expiry is provided.
///
/// # Examples
///
/// Install objects that no reference points to yet, keep them safe from repacking while the
/// application decides what to publish, then release the marker:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::fetch::{FetchLimits, LocalFetchObserver, LocalFetchRequest, receive_local};
/// use girt::refs::{Expected, RefName, Target};
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
///
/// let cancel = AtomicBool::new(false);
/// let received = receive_local(
///     upstream.git_dir(),
///     |advertisement| {
///         advertisement
///             .refs
///             .iter()
///             .map(|reference| reference.id)
///             .collect()
///     },
///     LocalFetchRequest::new(FetchLimits::default()),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
/// let (_installed, retention) =
///     received.install_retained(&repo, PackLimits::default(), &cancel)?;
/// assert!(
///     retention
///         .path()
///         .extension()
///         .is_some_and(|extension| extension == "keep")
/// );
///
/// // Publish a reference that keeps the objects reachable, then release the marker.
/// let name = RefName::new("refs/imports/main")?;
/// repo.references()?
///     .update_without_reflog(&name, Target::Direct(main), Expected::Absent)?;
/// let marker = retention.path().to_owned();
/// retention.release()?;
/// assert!(!marker.exists());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[must_use = "release retention explicitly after publishing refs or abandoning the transfer"]
pub struct FetchRetention {
    path: PathBuf,
    file: File,
}

impl FetchRetention {
    /// Path of the owned marker, for diagnostics and recovery.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Releases this pack's retention after publication or deliberate abandonment.
    ///
    /// Once released, unreferenced objects may be collected. Source-only fetch users must first
    /// establish their own persistent roots. This does not remove pack/index files.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors or [`FetchError::Existing`] if the marker was replaced. A
    /// replacement is left untouched. On error the marker can remain and requires inspection;
    /// record [`Self::path`] before releasing if recovery is needed. Directory entries are not
    /// synced, so release does not promise power-loss durability.
    pub fn release(self) -> Result<(), FetchError> {
        let current = fs::symlink_metadata(&self.path)?;
        if !current.is_file() || current.file_type().is_symlink() {
            return Err(FetchError::Existing(self.path));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owned = self.file.metadata()?;
            if current.dev() != owned.dev() || current.ino() != owned.ino() {
                return Err(FetchError::Existing(self.path));
            }
        }
        // On Windows the open handle denies deletion until this explicit release.
        drop(self.file);
        fs::remove_file(self.path)?;
        Ok(())
    }
}

/// Retained installation failed, possibly after acquiring a marker or publishing pack bytes.
///
/// If `retention` is `None`, nothing was written. If it is `Some`, the marker was acquired and
/// pack files may be partially published; they stay protected until the marker is released.
/// Inspect the pack directory, then release the marker when the partial installation is no longer
/// needed.
///
/// # Examples
///
/// Retained installation refuses a pack that is already installed, because a collector may
/// already have chosen the old copy for deletion:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::fetch::{FetchLimits, LocalFetchObserver, LocalFetchRequest, receive_local};
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, _, _) = include!("../doctest/remote.rs");
///
/// let cancel = AtomicBool::new(false);
/// let received = receive_local(
///     upstream.git_dir(),
///     |advertisement| {
///         advertisement
///             .refs
///             .iter()
///             .map(|reference| reference.id)
///             .collect()
///     },
///     LocalFetchRequest::new(FetchLimits::default()),
///     TransportControl::new(&cancel),
///     LocalFetchObserver::default(),
/// )?;
/// received.install(&repo, PackLimits::default(), &cancel)?;
///
/// let error = received
///     .install_retained(&repo, PackLimits::default(), &cancel)
///     .unwrap_err();
/// assert!(
///     error.retention.is_none(),
///     "refused before acquiring a marker"
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[error("retained fetch installation")]
#[non_exhaustive]
pub struct RetainedFetchError {
    /// Original validation, publication, or filesystem failure. No references were changed.
    #[source]
    pub source: Box<FetchError>,
    /// Owned retention, if acquired. Inspect partial effects before explicitly releasing it.
    /// Dropping this error leaves the marker in place.
    pub retention: Option<FetchRetention>,
}

impl RetainedFetchError {
    pub(super) fn before_retention(source: FetchError) -> Self {
        Self {
            source: Box::new(source),
            retention: None,
        }
    }
}

pub(super) fn install(
    received: &ReceivedFetch,
    repository: &Repository,
    checksum: ObjectId,
    limits: PackLimits,
    cancel: &AtomicBool,
) -> Result<(FetchInstalled, FetchRetention), RetainedFetchError> {
    install_with(repository, checksum, || {
        received.install(repository, limits, cancel)
    })
}

pub(super) fn install_with(
    repository: &Repository,
    checksum: ObjectId,
    publish: impl FnOnce() -> Result<FetchInstalled, FetchError>,
) -> Result<(FetchInstalled, FetchRetention), RetainedFetchError> {
    let directory = repository.object_dir().join("pack");
    fs::create_dir_all(&directory)
        .map_err(|error| RetainedFetchError::before_retention(error.into()))?;
    let basename = directory.join(format!("pack-{checksum}"));
    // Check both before and after acquiring the marker. Existing artifacts may already be in
    // a collector's deletion plan; merely adding a keep file cannot withdraw that decision.
    require_absent(&basename).map_err(RetainedFetchError::before_retention)?;
    let path = basename.with_extension("keep");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3);
    }
    let file = options
        .open(&path)
        .map_err(|error| RetainedFetchError::before_retention(error.into()))?;
    let retention = FetchRetention { path, file };
    let result = require_absent(&basename).and_then(|()| publish());
    match result {
        Ok(installed) => Ok((installed, retention)),
        Err(source) => Err(RetainedFetchError {
            source: Box::new(source),
            retention: Some(retention),
        }),
    }
}

fn require_absent(basename: &Path) -> Result<(), FetchError> {
    for extension in ["pack", "idx"] {
        let path = basename.with_extension(extension);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(FetchError::Existing(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use rstest::rstest;

    use super::*;
    use crate::fetch::{FetchLimits, KnownHistory, receive_local};
    use crate::refs::{Expected, RefName, Target};
    use crate::transport::TransportControl;
    use crate::{InitKind, ObjectFormat};

    fn fixture() -> (tempfile::TempDir, Repository, Repository, ReceivedFetch) {
        let root = tempfile::tempdir().unwrap();
        let source = Repository::init(
            ObjectFormat::Sha1,
            root.path().join("source"),
            InitKind::Bare,
        )
        .unwrap();
        let destination = Repository::init(
            ObjectFormat::Sha1,
            root.path().join("destination"),
            InitKind::Bare,
        )
        .unwrap();
        let id = source
            .loose_objects()
            .write_blob(b"original retention fixture")
            .unwrap();
        source
            .references()
            .unwrap()
            .update_without_reflog(
                &RefName::new("refs/tags/blob").unwrap(),
                Target::Direct(id),
                Expected::Absent,
            )
            .unwrap();
        let received = receive_local(
            source.git_dir(),
            |_| vec![id],
            crate::fetch::LocalFetchRequest::new(FetchLimits::default()),
            crate::transport::TransportControl::new(&AtomicBool::new(false)),
            crate::fetch::LocalFetchObserver::default(),
        )
        .unwrap();
        (root, source, destination, received)
    }

    #[test]
    fn simultaneous_installers_cannot_share_marker_ownership() {
        let (_root, _source, destination, received) = fixture();
        let barrier = Barrier::new(2);
        let install = || {
            barrier.wait();
            received.install_retained(&destination, PackLimits::default(), &AtomicBool::new(false))
        };
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(install);
            let second = scope.spawn(install);
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let successful = first.ok().or_else(|| second.ok()).unwrap();
        assert!(successful.1.path().exists());
        successful.1.release().unwrap();
    }

    #[test]
    fn cancellation_after_marker_acquisition_returns_retention() {
        let (_root, _source, destination, received) = fixture();
        let checksum = received
            .install(&destination, PackLimits::default(), &AtomicBool::new(false))
            .unwrap()
            .checksum
            .unwrap();
        let base = destination
            .object_dir()
            .join("pack")
            .join(format!("pack-{checksum}"));
        fs::remove_file(base.with_extension("pack")).unwrap();
        fs::remove_file(base.with_extension("idx")).unwrap();
        let error = install_with(&destination, checksum, || {
            assert!(base.with_extension("keep").exists());
            received.install(&destination, PackLimits::default(), &AtomicBool::new(true))
        })
        .unwrap_err();
        assert!(matches!(*error.source, FetchError::Cancelled));
        assert!(!base.with_extension("pack").exists());
        error.retention.unwrap().release().unwrap();
        assert!(!base.with_extension("keep").exists());
    }

    #[test]
    fn publication_failure_retains_partial_pack_and_foreign_index() {
        let (_root, _source, destination, received) = fixture();
        let installed = received
            .install(&destination, PackLimits::default(), &AtomicBool::new(false))
            .unwrap();
        let checksum = installed.checksum.unwrap();
        let base = destination
            .object_dir()
            .join("pack")
            .join(format!("pack-{checksum}"));
        fs::remove_file(base.with_extension("pack")).unwrap();
        fs::remove_file(base.with_extension("idx")).unwrap();
        let error = install_with(&destination, checksum, || {
            fs::write(base.with_extension("idx"), b"foreign index").unwrap();
            received.install(&destination, PackLimits::default(), &AtomicBool::new(false))
        })
        .unwrap_err();
        assert!(matches!(*error.source, FetchError::Existing(_)));
        assert!(base.with_extension("pack").exists());
        assert_eq!(
            fs::read(base.with_extension("idx")).unwrap(),
            b"foreign index"
        );
        assert!(error.retention.as_ref().unwrap().path().exists());
        error.retention.unwrap().release().unwrap();
    }

    #[rstest]
    #[case::pack("pack")]
    #[case::index("idx")]
    #[case::marker("keep")]
    fn existing_artifacts_are_not_adopted(#[case] extension: &str) {
        let (_root, _source, destination, received) = fixture();
        let checksum = received
            .install(&destination, PackLimits::default(), &AtomicBool::new(false))
            .unwrap()
            .checksum
            .unwrap();
        let base = destination
            .object_dir()
            .join("pack")
            .join(format!("pack-{checksum}"));
        fs::remove_file(base.with_extension("pack")).unwrap();
        fs::remove_file(base.with_extension("idx")).unwrap();
        let foreign = base.with_extension(extension);
        fs::write(&foreign, b"foreign").unwrap();
        let error = received
            .install_retained(&destination, PackLimits::default(), &AtomicBool::new(false))
            .unwrap_err();
        assert!(error.retention.is_none());
        assert_eq!(fs::read(foreign).unwrap(), b"foreign");
        assert_eq!(fs::read_dir(base.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn incremental_history_is_rejected_before_acquiring_retention() {
        let (_root, source, destination, received) = fixture();
        let cancel = AtomicBool::new(false);
        let known = KnownHistory::new(
            &source.objects(PackLimits::default()).unwrap(),
            received.wants(),
            FetchLimits::default(),
            &cancel,
        )
        .unwrap();
        let incremental = receive_local(
            source.git_dir(),
            |_| received.wants().to_vec(),
            crate::fetch::LocalFetchRequest::new(FetchLimits::default()).with_known(&known),
            TransportControl::new(&cancel),
            crate::fetch::LocalFetchObserver::default(),
        )
        .unwrap();
        let error = incremental
            .install_retained(&destination, PackLimits::default(), &cancel)
            .unwrap_err();
        assert!(matches!(
            *error.source,
            FetchError::Unsupported("retention requires complete history")
        ));
        assert!(error.retention.is_none());
        assert_eq!(
            fs::read_dir(destination.object_dir().join("pack"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn dropping_handle_preserves_marker_for_recovery() {
        let (_root, _source, destination, received) = fixture();
        let (_, retention) = received
            .install_retained(&destination, PackLimits::default(), &AtomicBool::new(false))
            .unwrap();
        let path = retention.path().to_owned();
        drop(retention);
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn release_preserves_replaced_marker() {
        let (_root, _source, destination, received) = fixture();
        let (_, retention) = received
            .install_retained(&destination, PackLimits::default(), &AtomicBool::new(false))
            .unwrap();
        let path = retention.path().to_owned();
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(matches!(retention.release(), Err(FetchError::Existing(_))));
        assert_eq!(fs::read(path).unwrap(), b"replacement");
    }
}
