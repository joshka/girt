//! Shared lazy object snapshots; never hold a lock while resolving or reading object contents.
use std::fmt::Display;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::revision::{Resolver, RevisionError};
use crate::{Commit, ObjectId, ObjectKind, Objects, Repository, Tree};

#[derive(Debug)]
pub(super) struct ObjectView {
    pub(super) repository: Repository,
    snapshot: Mutex<Option<Arc<Objects>>>,
    updating: Mutex<()>,
}

impl ObjectView {
    pub(super) fn new(repository: Repository) -> Self {
        Self {
            repository,
            snapshot: Mutex::new(None),
            updating: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Arc<Objects>>> {
        // Only complete snapshots are assigned. A panic during opening/refresh leaves the previous
        // snapshot (or None) intact, so recovering a poisoned publication lock is safe.
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn update_lock(&self) -> MutexGuard<'_, ()> {
        // A failed/panicking scan cannot publish a partial snapshot; see `lock`.
        self.updating
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn snapshot(&self) -> crate::Result<Arc<Objects>> {
        if let Some(objects) = &*self.lock() {
            return Ok(Arc::clone(objects));
        }
        let _updating = self.update_lock();
        if let Some(objects) = &*self.lock() {
            return Ok(Arc::clone(objects));
        }
        let objects = Arc::new(self.repository.objects()?);
        *self.lock() = Some(Arc::clone(&objects));
        Ok(objects)
    }

    fn refresh(&self, previous: &Arc<Objects>) -> crate::Result<Arc<Objects>> {
        let _updating = self.update_lock();
        let current = self
            .lock()
            .as_ref()
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::clone(previous));
        // Scan after this miss even if another caller already refreshed: objects may have been
        // published since that earlier scan. Cache hits can keep reading during this I/O.
        let mut refreshed = (*current).clone();
        refreshed.refresh(Default::default(), Default::default())?;
        let objects = Arc::new(refreshed);
        *self.lock() = Some(Arc::clone(&objects));
        Ok(objects)
    }

    pub(super) fn resolve(
        &self,
        revision: impl Display,
        kind: Option<ObjectKind>,
    ) -> crate::Result<ObjectId> {
        let revision = revision.to_string().parse()?;
        let resolve = |objects: &Objects| {
            let resolver = Resolver::new(&self.repository, objects)?;
            match kind {
                Some(kind) => resolver.resolve_to(&revision, kind),
                None => resolver.resolve(&revision),
            }
        };
        let objects = self.snapshot()?;
        match resolve(&objects) {
            Err(error) if revision_missing(&error) => {
                let refreshed = self.refresh(&objects)?;
                Ok(resolve(&refreshed)?)
            }
            result => Ok(result?),
        }
    }

    pub(super) fn read_commit(&self, id: ObjectId) -> crate::Result<Commit> {
        self.read(id, |objects, id| objects.read_commit(id))
    }

    pub(super) fn read_tree(&self, id: ObjectId) -> crate::Result<Tree> {
        self.read(id, |objects, id| objects.read_tree(id))
    }

    pub(super) fn read_blob(&self, id: ObjectId) -> crate::Result<Vec<u8>> {
        self.read(id, |objects, id| objects.read_blob(id))
    }

    fn read<T, E: Into<crate::Error>>(
        &self,
        id: ObjectId,
        read: impl Fn(&Objects, ObjectId) -> Result<T, E>,
    ) -> crate::Result<T> {
        let objects = self.snapshot()?;
        match read(&objects, id).map_err(Into::into) {
            Err(error) if missing(&error) => {
                let refreshed = self.refresh(&objects)?;
                read(&refreshed, id).map_err(Into::into)
            }
            result => result,
        }
    }
}

fn missing(error: &crate::Error) -> bool {
    matches!(error.downcast_ref(), Some(crate::ReadBlobError::Missing(_)))
        || matches!(error.downcast_ref(), Some(crate::ReadTreeError::Missing(_)))
        || matches!(
            error.downcast_ref(),
            Some(crate::ReadCommitError::Missing(_))
        )
}

fn revision_missing(error: &RevisionError) -> bool {
    matches!(
        error,
        RevisionError::NotFound(_) | RevisionError::MissingObject(_)
    ) || matches!(error, RevisionError::Peel(error) if matches!(*error.source, crate::PeelFailure::Missing))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::{InitKind, ObjectFormat};

    fn git(repository: &Repository, args: &[&str]) {
        let mut command = Command::new("git");
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        let output = command
            .current_dir(repository.git_dir())
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repository.git_dir().join("absent"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_newer_snapshot_does_not_skip_refresh_after_a_later_miss() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(
            ObjectFormat::Sha1,
            directory.path().join("repo"),
            InitKind::Bare,
        )
        .unwrap();
        let view = ObjectView::new(repository);
        let original = view.snapshot().unwrap();
        let intermediate = view.refresh(&original).unwrap();
        let id = view
            .repository
            .loose_objects()
            .write_blob(b"published after the scan")
            .unwrap();
        git(
            &view.repository,
            &["update-ref", "refs/tags/blob", &id.to_string()],
        );
        git(&view.repository, &["repack", "-ad"]);
        git(&view.repository, &["prune-packed"]);
        assert!(original.read(id).unwrap().is_none());
        assert!(intermediate.read(id).unwrap().is_none());

        let refreshed = view.refresh(&original).unwrap();

        assert_eq!(
            refreshed.read_blob(id).unwrap(),
            b"published after the scan"
        );
    }

    #[test]
    fn cached_content_reads_do_not_need_the_refresh_gate() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Repository::init(
            ObjectFormat::Sha1,
            directory.path().join("repo"),
            InitKind::Bare,
        )
        .unwrap();
        let id = repository.loose_objects().write_blob(b"cached").unwrap();
        let view = ObjectView::new(repository);
        view.snapshot().unwrap();
        let _refresh_in_progress = view.update_lock();

        assert_eq!(view.read_blob(id).unwrap(), b"cached");
    }
}
