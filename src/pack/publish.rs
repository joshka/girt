//! No-clobber, durable publication of caller-validated, synchronized pack artifacts.
//!
//! The caller excludes conflicting publishers and synchronizes complete temporary contents before
//! calling. This module only publishes names; it does not validate objects or authorize retirement.
//! Publication runs in this order, and every caller gets the same guarantees:
//!
//! 1. An optional `.promisor` marker is written to a temporary file, synchronized, published under
//!    its final name and its name synchronized. Readers treat objects a promisor pack references as
//!    promised, so the marker must be durable before the pack can be seen.
//! 2. The `.pack` is published and its name synchronized before the index can be seen.
//! 3. The `.idx`, the visibility marker for the pair, is published and its name synchronized.
//!
//! Directory synchronization happens on Unix; other platforms keep the weaker name-durability
//! guarantee they had. Interruption can leave a marker with no pack, or a complete unindexed pack;
//! readers ignore both.
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::NamedTempFile;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("pack publication cancelled")]
    Cancelled,
    #[error("conflicting existing pack artifact: {0}")]
    Existing(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The visible change that a publication step is about to make.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    /// Publishing the `.promisor` marker.
    Marker,
    /// Synchronizing the marker's name, before the pack can be seen.
    SyncMarker,
    /// Publishing the `.pack`.
    Pack,
    /// Synchronizing the pack's name, before the index can be seen.
    SyncPack,
    /// Publishing the `.idx`, which makes the pair visible.
    Index,
    /// Synchronizing the index name: the final sync of the publication.
    SyncIndex,
}

/// A synchronized temporary pack and index, with the name they publish under.
pub(crate) struct Pair<'a> {
    pub(crate) pack: NamedTempFile,
    pub(crate) index: NamedTempFile,
    /// `pack-<checksum>` in the pack directory, without an extension.
    pub(crate) basename: &'a Path,
    /// Contents of a `.promisor` marker to publish first, if the pack came from a promisor.
    pub(crate) marker: Option<&'a [u8]>,
}

/// What one publication made visible, kept when a later step fails.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "only Unix maintenance reads publication progress")
)]
pub(crate) struct Progress {
    /// Published names, in publication order. A name here is visible even if its directory sync
    /// failed.
    pub(crate) visible: Vec<PathBuf>,
    /// The name whose directory synchronization failed, so its durability is unknown.
    pub(crate) uncertain: Option<PathBuf>,
}

impl Progress {
    /// Whether the `.idx`, and so the complete pair, became visible.
    #[cfg(any(unix, test))]
    pub(crate) fn pair_visible(&self) -> bool {
        self.visible
            .last()
            .is_some_and(|path| path.extension().is_some_and(|ext| ext == "idx"))
    }
}

/// Publishes `pair` in the order the module describes, recording each visible name in `progress`.
///
/// A publication synchronizes the pack directory exactly once per name: after the marker (only
/// when there is one), after the pack, and a final sync after the index. Pack and index contents
/// are the caller's to synchronize first; the marker's contents are synchronized here. `.rev` and
/// `.bitmap` sidecars are not part of this call: bitmap installation publishes them later with its
/// own directory sync, and both are optional, regenerable and checksum-verified on read, so losing
/// them in a crash only costs speed.
///
/// `before` runs before each [`Step`], and cancellation is checked after each publication step's
/// callback; callers use it to recheck that their inputs are unchanged, and tests to stop at a
/// step. An error from a sync step's callback counts as that sync failing. An existing marker that
/// is a regular file is kept: its contents are advisory, and only its presence promises objects.
pub(crate) fn pair_checked<E>(
    pair: Pair<'_>,
    cancel: &AtomicBool,
    progress: &mut Progress,
    mut before: impl FnMut(Step) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<Error>,
{
    let Pair {
        pack,
        index,
        basename,
        marker,
    } = pair;
    let directory = basename.parent().expect("pack basename has a directory");
    let sync = |before: &mut dyn FnMut(Step) -> Result<(), E>,
                step: Step,
                path: PathBuf,
                progress: &mut Progress|
     -> Result<(), E> {
        progress.uncertain = Some(path);
        before(step)?;
        sync_directory(directory).map_err(Error::Io)?;
        progress.uncertain = None;
        Ok(())
    };
    if let Some(contents) = marker {
        let path = basename.with_extension("promisor");
        before(Step::Marker)?;
        check(cancel)?;
        publish_marker(directory, &path, contents)?;
        progress.visible.push(path.clone());
        sync(&mut before, Step::SyncMarker, path, progress)?;
    }
    let path = basename.with_extension("pack");
    before(Step::Pack)?;
    check(cancel)?;
    file(pack, &path, cancel)?;
    progress.visible.push(path.clone());
    // Persist the pack name before publishing its index visibility marker.
    sync(&mut before, Step::SyncPack, path, progress)?;
    let path = basename.with_extension("idx");
    before(Step::Index)?;
    check(cancel)?;
    file(index, &path, cancel)?;
    progress.visible.push(path.clone());
    sync(&mut before, Step::SyncIndex, path, progress)
}

/// Writes and synchronizes `contents` in a temporary file, then publishes it at `path` unless a
/// regular file already exists there.
fn publish_marker(directory: &Path, path: &Path, contents: &[u8]) -> Result<(), Error> {
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(path)?.is_file() {
                Ok(())
            } else {
                Err(Error::Existing(path.into()))
            }
        }
        Err(error) => Err(error.error.into()),
    }
}

/// Synchronizes the names in `directory`. Other platforms have no directory handle to sync.
pub(crate) fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

pub(crate) fn file(
    temporary: NamedTempFile,
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            let mut existing = File::open(path)?;
            let mut candidate = error.file.reopen()?;
            if existing.metadata()?.len() != candidate.metadata()?.len() {
                return Err(Error::Existing(path.into()));
            }
            let mut a = [0; 8192];
            let mut b = [0; 8192];
            loop {
                check(cancel)?;
                let n = existing.read(&mut a)?;
                candidate.read_exact(&mut b[..n])?;
                if a[..n] != b[..n] {
                    return Err(Error::Existing(path.into()));
                }
                if n == 0 {
                    return Ok(());
                }
            }
        }
        Err(error) => Err(error.error.into()),
    }
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::NamedTempFile;

    use super::*;

    fn temporary(directory: &Path, bytes: &[u8]) -> NamedTempFile {
        let mut file = NamedTempFile::new_in(directory).unwrap();
        file.write_all(bytes).unwrap();
        file.as_file().sync_all().unwrap();
        file
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn refuses_conflicting_existing_bytes_without_clobber(#[case] different_length: bool) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pack-existing.pack");
        let original = vec![b'a'; 17_000];
        fs::write(&path, &original).unwrap();
        let mut candidate = original.clone();
        if different_length {
            candidate.push(b'b');
        } else {
            candidate[16_999] = b'b';
        }
        let result = file(
            temporary(root.path(), &candidate),
            &path,
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Existing(ref conflict)) if *conflict == path));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn identical_existing_artifacts_are_verified_and_reused() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pack-existing.pack");
        let original = vec![b'a'; 17_000];
        fs::write(&path, &original).unwrap();
        file(
            temporary(root.path(), &original),
            &path,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    fn pair<'a>(root: &Path, basename: &'a Path, marker: Option<&'a [u8]>) -> Pair<'a> {
        Pair {
            pack: temporary(root, b"pack"),
            index: temporary(root, b"index"),
            basename,
            marker,
        }
    }

    #[test]
    fn pack_name_is_synced_before_index_visibility() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let events = RefCell::new(Vec::new());
        let mut progress = Progress::default();
        pair_checked::<Error>(
            pair(root.path(), &basename, None),
            &AtomicBool::new(false),
            &mut progress,
            |step| {
                if step == Step::Index {
                    // This step follows pack-name directory synchronization on Unix.
                    assert_eq!(fs::read(basename.with_extension("pack")).unwrap(), b"pack");
                    assert!(!basename.with_extension("idx").exists());
                }
                events.borrow_mut().push(step);
                Ok(())
            },
        )
        .unwrap();
        // Exactly two directory syncs without a marker: after the pack and after the index.
        assert_eq!(
            *events.borrow(),
            [Step::Pack, Step::SyncPack, Step::Index, Step::SyncIndex]
        );
        assert_eq!(fs::read(basename.with_extension("idx")).unwrap(), b"index");
        assert!(!basename.with_extension("promisor").exists());
        assert!(progress.pair_visible() && progress.uncertain.is_none());
    }

    #[test]
    fn marker_is_published_before_the_pack() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let marker = basename.with_extension("promisor");
        let events = RefCell::new(Vec::new());
        let mut progress = Progress::default();
        pair_checked::<Error>(
            pair(root.path(), &basename, Some(b"")),
            &AtomicBool::new(false),
            &mut progress,
            |step| {
                match step {
                    Step::Marker => assert!(!marker.exists()),
                    Step::Pack => {
                        assert_eq!(fs::read(&marker).unwrap(), b"");
                        assert!(!basename.with_extension("pack").exists());
                    }
                    Step::Index => assert!(!basename.with_extension("idx").exists()),
                    _ => {}
                }
                events.borrow_mut().push(step);
                Ok(())
            },
        )
        .unwrap();
        // Exactly three directory syncs with a marker.
        assert_eq!(
            *events.borrow(),
            [
                Step::Marker,
                Step::SyncMarker,
                Step::Pack,
                Step::SyncPack,
                Step::Index,
                Step::SyncIndex
            ]
        );
        assert_eq!(fs::read(&marker).unwrap(), b"");
        assert_eq!(fs::read(basename.with_extension("pack")).unwrap(), b"pack");
        assert_eq!(fs::read(basename.with_extension("idx")).unwrap(), b"index");
        // Only the three published names remain; no temporary file is left behind.
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
        assert_eq!(
            progress.visible,
            ["promisor", "pack", "idx"].map(|ext| basename.with_extension(ext))
        );
    }

    #[rstest::rstest]
    #[case::marker(Step::Marker, &[], None)]
    #[case::marker_sync(Step::SyncMarker, &["promisor"], Some("promisor"))]
    #[case::pack(Step::Pack, &["promisor"], None)]
    #[case::pack_sync(Step::SyncPack, &["promisor", "pack"], Some("pack"))]
    #[case::index(Step::Index, &["promisor", "pack"], None)]
    #[case::final_sync(Step::SyncIndex, &["promisor", "pack", "idx"], Some("idx"))]
    fn stopping_at_a_step_reports_exactly_the_visible_names(
        #[case] stop: Step,
        #[case] visible: &[&str],
        #[case] uncertain: Option<&str>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let mut progress = Progress::default();
        let result = pair_checked::<Error>(
            pair(root.path(), &basename, Some(b"")),
            &AtomicBool::new(false),
            &mut progress,
            |step| {
                if step == stop {
                    Err(Error::Io(io::Error::other("injected")))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(Error::Io(_))));
        let mut names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.file_stem() == basename.file_stem())
            .map(|path| path.extension().unwrap().to_str().unwrap().to_owned())
            .collect();
        names.sort();
        let mut expected = visible.to_vec();
        expected.sort();
        assert_eq!(names, expected);
        let reported: Vec<_> = visible
            .iter()
            .map(|ext| basename.with_extension(ext))
            .collect();
        assert_eq!(progress.visible, reported);
        assert_eq!(
            progress.uncertain,
            uncertain.map(|ext| basename.with_extension(ext))
        );
        assert_eq!(progress.pair_visible(), stop == Step::SyncIndex);
    }

    #[test]
    fn existing_marker_file_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let marker = basename.with_extension("promisor");
        fs::write(&marker, b"advertised tips\n").unwrap();
        pair_checked::<Error>(
            pair(root.path(), &basename, Some(b"")),
            &AtomicBool::new(false),
            &mut Progress::default(),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(fs::read(marker).unwrap(), b"advertised tips\n");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
    }

    #[test]
    fn non_file_marker_refuses_before_pack_visibility() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let marker = basename.with_extension("promisor");
        fs::create_dir(&marker).unwrap();
        let result = pair_checked::<Error>(
            pair(root.path(), &basename, Some(b"")),
            &AtomicBool::new(false),
            &mut Progress::default(),
            |_| Ok(()),
        );
        assert!(matches!(result, Err(Error::Existing(ref path)) if *path == marker));
        assert!(!basename.with_extension("pack").exists());
    }

    #[test]
    fn cancellation_between_names_preserves_only_unindexed_pack() {
        let root = tempfile::tempdir().unwrap();
        let basename = root.path().join("pack-pair");
        let cancel = AtomicBool::new(false);
        let result = pair_checked::<Error>(
            pair(root.path(), &basename, None),
            &cancel,
            &mut Progress::default(),
            |step| {
                if step == Step::Index {
                    // Cancellation arrives after the pack name is published.
                    cancel.store(true, Ordering::Relaxed);
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(fs::read(basename.with_extension("pack")).unwrap(), b"pack");
        assert!(!basename.with_extension("idx").exists());
    }
}
