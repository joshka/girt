//! One operation-owned header window shared across every pack on the calling thread.
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
/// The guard cannot cross threads, and ordinary object reads outside a scope remain uncached.
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
        if window.depth == 0 || end - start > CAPACITY {
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

#[cfg(test)]
pub(super) fn allocated() -> usize {
    WINDOW.with(|window| window.borrow().bytes.capacity())
}
