//! A pinned pack index's bytes, mapped read-only where the platform allows it.
//!
//! Mapping lets a lookup touch only the pages it reads, as Git does, instead of copying the whole
//! index into the heap. The mapped pages are clean and shared with the page cache. Truncating the
//! file in place while it is mapped makes a later access fault with `SIGBUS`; published indexes
//! are immutable, so that requires the same contract violation Git also does not survive.
//!
//! Windows reads the index into memory instead. A mapped file there cannot be deleted until the
//! mapping is gone, which would stop Git's repack from retiring a pack a girt reader holds.
use std::fmt;
use std::ops::Deref;

use super::{Artifact, Error};

/// The bytes of one pinned pack index.
pub(crate) struct IndexBytes(Inner);

enum Inner {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Mapped {
        start: std::ptr::NonNull<u8>,
        len: usize,
    },
    Owned(Vec<u8>),
}

// SAFETY: a mapping is a private, read-only view owned by this value and unmapped only when it
// drops, so sharing or sending it is as safe as sharing or sending the `&[u8]` it derefs to.
#[cfg(any(target_os = "macos", target_os = "linux"))]
unsafe impl Send for IndexBytes {}
// SAFETY: as for `Send`; nothing writes through the mapping.
#[cfg(any(target_os = "macos", target_os = "linux"))]
unsafe impl Sync for IndexBytes {}

impl IndexBytes {
    /// Maps the whole pinned artifact, or reads it where mapping is unavailable.
    pub(crate) fn of(artifact: &Artifact) -> Result<Self, Error> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if artifact.len != 0 {
            use rustix::mm::{MapFlags, ProtFlags, mmap};
            // SAFETY: a null hint lets the kernel choose an address, so the mapping overlaps no
            // existing memory. The length is the file's size when it was pinned, and the offset
            // is zero. The pages are read-only and private, so nothing in this process writes
            // them. Another process can still change the file in place, which the
            // immutable-artifact contract forbids, as Git's mapped indexes require too.
            let mapped = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    artifact.len,
                    ProtFlags::READ,
                    MapFlags::PRIVATE,
                    &*artifact.file,
                    0,
                )
            };
            // A file system that cannot map files is still readable.
            if let Ok(start) = mapped
                && let Some(start) = std::ptr::NonNull::new(start.cast::<u8>())
            {
                // Tests count a mapping as loading the whole artifact, as reading it would.
                #[cfg(test)]
                artifact
                    .bytes_read
                    .fetch_add(artifact.len, std::sync::atomic::Ordering::Relaxed);
                return Ok(Self(Inner::Mapped {
                    start,
                    len: artifact.len,
                }));
            }
        }
        artifact
            .bytes(0, artifact.len)
            .map(|bytes| Self(Inner::Owned(bytes)))
    }

    #[cfg(test)]
    pub(crate) fn is_mapped(&self) -> bool {
        !matches!(self.0, Inner::Owned(_))
    }
}

impl Deref for IndexBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match &self.0 {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            // SAFETY: the mapping covers `len` readable bytes from `start` and lives as long as
            // `self`, which this borrow cannot outlive.
            Inner::Mapped { start, len } => unsafe {
                std::slice::from_raw_parts(start.as_ptr(), *len)
            },
            Inner::Owned(bytes) => bytes,
        }
    }
}

impl Drop for IndexBytes {
    fn drop(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Inner::Mapped { start, len } = self.0 {
            // SAFETY: `start` and `len` describe a mapping this value created and still owns.
            // Every borrow of it has ended, because borrows cannot outlive `self`. A failure
            // leaves only an unused mapping, so it is ignored.
            let _ = unsafe { rustix::mm::munmap(start.as_ptr().cast(), len) };
        }
    }
}

impl fmt::Debug for IndexBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexBytes")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}
