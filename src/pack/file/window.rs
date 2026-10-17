//! One operation-owned header window shared across every pack on the calling thread, and a small
//! per-thread read-ahead window for ordinary content reads outside any operation.
//! Worker threads need their own scope; future parallel scans do not inherit this thread's window.
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use super::{Artifact, Error};

const CAPACITY: usize = 256 * 1024;

#[derive(Default)]
struct Window {
    depth: usize,
    scans: usize,
    artifact: u64,
    start: usize,
    bytes: Vec<u8>,
    previous: Option<(u64, usize)>,
}

thread_local! {
    static WINDOW: RefCell<Window> = RefCell::new(Window::default());
}

/// Nested retention phases share one window; the outermost operation drops its allocation.
/// The guard cannot cross threads. Ordinary object reads outside a scope use the smaller content
/// window instead.
pub(crate) struct HeaderScope(bool, PhantomData<Rc<()>>);

impl HeaderScope {
    pub(crate) fn new() -> Self {
        WINDOW.with(|window| window.borrow_mut().depth += 1);
        Self(false, PhantomData)
    }

    /// Enables readahead only for an offset-ordered phase of an existing retention operation.
    pub(crate) fn ordered() -> Option<Self> {
        WINDOW.with(|window| {
            let mut window = window.borrow_mut();
            if window.depth == 0 {
                return None;
            }
            window.depth += 1;
            window.scans += 1;
            Some(Self(true, PhantomData))
        })
    }
}

impl Drop for HeaderScope {
    fn drop(&mut self) {
        WINDOW.with(|window| {
            let mut window = window.borrow_mut();
            if self.0 {
                window.scans -= 1;
            }
            window.depth -= 1;
            if window.depth == 0 {
                *window = Window::default();
            }
        });
    }
}

pub(super) fn bytes(artifact: &Artifact, start: usize, end: usize) -> Result<Vec<u8>, Error> {
    if start > end || end > artifact.len {
        return Err(Error::Corrupt("pack header range"));
    }
    WINDOW.with(|window| {
        let mut window = window.borrow_mut();
        if window.depth == 0 {
            drop(window);
            return content(artifact, start, end);
        }
        if end - start > CAPACITY {
            return artifact.bytes(start, end);
        }
        if start == end {
            return Ok(Vec::new());
        }
        let sequential = window.previous.is_some_and(|(id, offset)| {
            id == artifact.window_id && start >= offset && start - offset <= 64 * 1024
        });
        window.previous = Some((artifact.window_id, start));
        if window.artifact != artifact.window_id
            || start < window.start
            || end - window.start > window.bytes.len()
        {
            // Keep a scanning window intact when a delta base or graph edge needs scattered
            // bytes. Small packs fit entirely; large random misses retain the old 4 KiB behavior.
            if (window.scans == 0 || !sequential) && artifact.len > CAPACITY {
                return artifact.bytes(start, end);
            }
            // Invalidate before I/O: a failed fill must never leave an old window addressable.
            window.artifact = 0;
            window.start = if artifact.len <= CAPACITY { 0 } else { start };
            if window.bytes.capacity() == 0 {
                window.bytes = Vec::with_capacity(CAPACITY);
            }
            let count = CAPACITY.min(artifact.len - window.start);
            window.bytes.resize(count, 0);
            let offset = window.start;
            artifact.read_at(offset, &mut window.bytes)?;
            window.artifact = artifact.window_id;
        }
        window
            .bytes
            .get(start - window.start..end - window.start)
            .map(<[u8]>::to_vec)
            .ok_or(Error::Corrupt("pack header window range"))
    })
}

/// Read-ahead for ordinary content reads, which run outside any [`HeaderScope`].
///
/// Git writes commits, then trees, in traversal order, so a history walk reads entries that sit
/// close together and ascend. Once a read lands shortly after the previous one in the same pack,
/// one positioned read fills this window and later entries copy from it. Scattered reads, such as
/// a delta base or a checkout's trees, keep their single small read and leave the window intact.
/// Each thread retains at most this many bytes.
const CONTENT_CAPACITY: usize = 16 * 1024;

#[derive(Default)]
struct ContentWindow {
    artifact: u64,
    start: usize,
    bytes: Vec<u8>,
    previous: Option<(u64, usize)>,
}

thread_local! {
    static CONTENT: RefCell<ContentWindow> = RefCell::new(ContentWindow::default());
}

fn content(artifact: &Artifact, start: usize, end: usize) -> Result<Vec<u8>, Error> {
    CONTENT.with(|window| {
        let mut window = window.borrow_mut();
        let sequential = window.previous.is_some_and(|(id, offset)| {
            id == artifact.window_id && start >= offset && start - offset <= CONTENT_CAPACITY
        });
        window.previous = Some((artifact.window_id, start));
        let hit = window.artifact == artifact.window_id
            && start >= window.start
            && end - window.start <= window.bytes.len();
        if !hit {
            if !sequential || end - start > CONTENT_CAPACITY {
                return artifact.bytes(start, end);
            }
            // Invalidate before I/O: a failed fill must never leave an old window addressable.
            window.artifact = 0;
            window.start = start;
            if window.bytes.capacity() == 0 {
                window.bytes = Vec::with_capacity(CONTENT_CAPACITY);
            }
            let count = CONTENT_CAPACITY.min(artifact.len - start);
            window.bytes.resize(count, 0);
            artifact.read_at(start, &mut window.bytes)?;
            window.artifact = artifact.window_id;
        }
        window
            .bytes
            .get(start - window.start..end - window.start)
            .map(<[u8]>::to_vec)
            .ok_or(Error::Corrupt("pack content window range"))
    })
}

#[cfg(test)]
pub(super) fn allocated() -> usize {
    WINDOW.with(|window| window.borrow().bytes.capacity())
}

#[cfg(test)]
pub(super) fn content_allocated() -> usize {
    CONTENT.with(|window| window.borrow().bytes.capacity())
}
