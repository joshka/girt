//! Resource-controlled opening without ambient configuration discovery.
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{OpenError, Repository, RepositoryLocation, RepositoryMetadata, ShallowRoots};
use crate::config::{ConfigInputs, IncludePlacement, ResolveLimits};
use crate::refs::reftable::StackLimits;

/// Independent budgets for repository metadata and its configured sources.
///
/// Defaults allow 64 KiB of aggregate layout/HEAD/backlink input and 16 MiB of shallow data.
/// Configuration uses [`ResolveLimits::default`] to bound includes, expanded entries and matching;
/// reftable uses [`StackLimits::default`] to bound encoded/decoded stacks. These are resource
/// budgets, not filesystem isolation or a wall-clock deadline.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct OpenLimits {
    /// Aggregate bytes read from gitfiles, commondir, HEAD and backlinks, counting rereads.
    pub metadata_bytes: usize,
    /// Bytes read from the common shallow file.
    pub shallow_bytes: usize,
    /// Bootstrap and effective configuration resolution budgets, separately per pass.
    pub configuration: ResolveLimits,
    /// Reftable stack budgets for branch-conditional configuration context.
    pub references: StackLimits,
}

impl Default for OpenLimits {
    fn default() -> Self {
        Self {
            metadata_bytes: 64 * 1024,
            shallow_bytes: 16 * 1024 * 1024,
            configuration: ResolveLimits::default(),
            references: StackLimits::default(),
        }
    }
}

impl OpenLimits {
    /// Sets aggregate gitfile, commondir, HEAD and backlink bytes, including rereads.
    #[must_use]
    pub fn with_metadata_bytes(mut self, bytes: usize) -> Self {
        self.metadata_bytes = bytes;
        self
    }
    /// Sets the shallow metadata byte budget.
    #[must_use]
    pub fn with_shallow_bytes(mut self, bytes: usize) -> Self {
        self.shallow_bytes = bytes;
        self
    }
    /// Sets bootstrap and effective configuration budgets.
    #[must_use]
    pub fn with_configuration(mut self, limits: ResolveLimits) -> Self {
        self.configuration = limits;
        self
    }
    /// Sets the reftable stack budgets.
    #[must_use]
    pub fn with_references(mut self, limits: StackLimits) -> Self {
        self.references = limits;
        self
    }
}

/// Explicit bounded repository opening policy.
///
/// Defaults read only repository-local sources, as [`Repository::open`] does. Ambient Git
/// environment and global/system configuration are not consulted. Opening reads metadata only,
/// starts no background work and reads no object contents. Configure budgets with
/// [`Self::with_limits`], then call [`Self::open`]. Existing simple openers retain their trusted
/// metadata contract. This options value leaves configuration source policy separate from resource
/// budgets.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    limits: OpenLimits,
}

impl OpenOptions {
    /// Selects metadata, configuration, shallow and reftable budgets.
    #[must_use]
    pub fn with_limits(mut self, limits: OpenLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Opens one explicit repository path with cooperative cancellation and no writes.
    ///
    /// Uses the layouts and format/configuration validation of [`Repository::open`], without
    /// ancestor discovery. It additionally refuses nonregular layout files with
    /// [`OpenError::Unsupported`], where the trusted opener may instead report an I/O error.
    /// Caller-owned path input and filesystem calls are outside the byte/deadline contract.
    /// Exclude hostile path/file-type replacement during opening. Regular-file checks apply to
    /// gitfiles, commondir, HEAD and backlinks; configuration reads remain bounded synchronous
    /// phases. Each HEAD validation/context read is charged separately, and gitfiles can be read
    /// for both logical and canonical paths. Metadata size is checked before reads; growth uses
    /// one probe byte. Cancellation is checked between 8 KiB metadata chunks, around bounded
    /// configuration phases, and by the bounded shallow/reftable readers. A bounded parse or
    /// filesystem call can finish before the next check. No partial handle escapes on failure.
    ///
    /// # Errors
    ///
    /// Returns [`OpenError::Limit`] for the aggregate metadata budget and [`OpenError::Cancelled`]
    /// for observed cancellation, plus the layout, configuration and domain-specific budget
    /// errors of [`Repository::open`]. No repository files are created or changed.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, OpenLimits, OpenOptions, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let opened = OpenOptions::default()
    ///     .with_limits(OpenLimits::default().with_metadata_bytes(4096))
    ///     .open(repo.git_dir(), &AtomicBool::new(false))?;
    /// assert_eq!(opened.common_dir(), repo.common_dir());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open(
        &self,
        path: impl AsRef<Path>,
        cancel: &AtomicBool,
    ) -> Result<Repository, OpenError> {
        let mut control = Control::<true>::new(self.limits.metadata_bytes, cancel);
        control.phase()?;
        let location = RepositoryLocation::resolve_storage_with(
            path.as_ref(),
            false,
            None,
            None,
            &mut control,
        )?;
        let inputs = ConfigInputs::default().with_limits(self.limits.configuration);
        let metadata = RepositoryMetadata::read_location_with(
            &location,
            &inputs,
            self.limits.references,
            None,
            IncludePlacement::InPlace,
            &mut control,
        )?;
        control.phase()?;
        let shallow = ShallowRoots::read(
            metadata.common_dir.join("shallow"),
            metadata.object_format,
            self.limits.shallow_bytes,
            cancel,
        );
        control.phase()?;
        Ok(metadata.into_storage(shallow?))
    }
}

static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

pub(super) struct Control<'a, const CONTROLLED: bool> {
    remaining: usize,
    cancel: &'a AtomicBool,
}

impl Control<'static, false> {
    pub(super) fn trusted() -> Self {
        Self::new(0, &NEVER_CANCELLED)
    }
}

impl<'a, const CONTROLLED: bool> Control<'a, CONTROLLED> {
    fn new(remaining: usize, cancel: &'a AtomicBool) -> Self {
        Self { remaining, cancel }
    }

    #[inline]
    pub(super) fn phase(&self) -> Result<(), OpenError> {
        if CONTROLLED && self.cancel.load(Ordering::Relaxed) {
            return Err(OpenError::Cancelled);
        }
        Ok(())
    }

    pub(super) fn flag(&self) -> &AtomicBool {
        self.cancel
    }

    #[inline]
    pub(super) fn read(&mut self, path: &Path) -> Result<Vec<u8>, OpenError> {
        if !CONTROLLED {
            return super::read(path);
        }
        self.phase()?;
        let metadata = std::fs::metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                super::malformed(path, "missing required metadata")
            } else {
                super::io_error(path, error)
            }
        })?;
        if !metadata.is_file() {
            return Err(super::unsupported(path, "nonregular repository metadata"));
        }
        if metadata.len() > self.remaining as u64 {
            return Err(OpenError::Limit("metadata bytes"));
        }
        let file = std::fs::File::open(path).map_err(|error| super::io_error(path, error))?;
        let mut reader = file.take(self.remaining.saturating_add(1) as u64);
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            self.phase()?;
            let count = reader
                .read(&mut chunk)
                .map_err(|error| super::io_error(path, error))?;
            if count == 0 {
                break;
            }
            if count > self.remaining.saturating_sub(bytes.len()) {
                return Err(OpenError::Limit("metadata bytes"));
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        self.remaining -= bytes.len();
        self.phase()?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controlled_reader_charges_each_observation_and_checks_before_the_next() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("metadata");
        std::fs::write(&path, b"abcd").unwrap();
        let cancel = AtomicBool::new(false);
        let mut control = Control::<true>::new(8, &cancel);
        assert_eq!(control.read(&path).unwrap(), b"abcd");
        assert_eq!(control.read(&path).unwrap(), b"abcd");
        assert!(matches!(control.read(&path), Err(OpenError::Limit(_))));
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            control.read(&root.path().join("absent")),
            Err(OpenError::Cancelled)
        ));
    }

    #[test]
    fn trusted_specialization_ignores_budgets_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("metadata");
        std::fs::write(&path, b"abcd").unwrap();
        let cancel = AtomicBool::new(true);
        let mut control = Control::<false>::new(0, &cancel);
        assert_eq!(control.read(&path).unwrap(), b"abcd");
        assert!(control.phase().is_ok());
    }
    #[test]
    fn cancellation_after_location_resolution_precedes_bootstrap() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("r.git"),
            crate::InitKind::Bare,
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let mut control = Control::<true>::new(4096, &cancel);
        let location = RepositoryLocation::resolve_storage_with(
            repo.git_dir(),
            false,
            None,
            None,
            &mut control,
        )
        .unwrap();
        std::fs::write(repo.git_dir().join("config"), b"malformed").unwrap();
        cancel.store(true, Ordering::Relaxed);
        let result = RepositoryMetadata::read_location_with(
            &location,
            &ConfigInputs::default(),
            StackLimits::default(),
            None,
            IncludePlacement::InPlace,
            &mut control,
        );
        assert!(matches!(result, Err(OpenError::Cancelled)));
    }
}
