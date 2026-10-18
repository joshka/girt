//! One operation-owned header window shared across every pack on the calling thread, and a
//! bounded per-thread block cache for ordinary content reads outside any operation.
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
/// The guard cannot cross threads. Ordinary object reads outside a scope use the bounded content
/// cache instead.
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

/// Compressed bytes for ordinary content reads outside a [`HeaderScope`].
///
/// History order revisits separate pack regions. Retaining several aligned blocks avoids one
/// branch displacing another's bytes. Slots are shared across packs on this thread, keyed by
/// pinned artifact identity and block start. A collision replaces one slot; oversized or
/// cross-block requests read directly. A region is admitted after two misses in that slot, so an
/// isolated read does not fetch a full block. Each thread retains at most 4 MiB until it exits;
/// a pool of N active reader threads can therefore retain N × 4 MiB.
pub(super) const CONTENT_CAPACITY: usize = 64 * 1024;
const CONTENT_SLOTS: usize = 64;
#[cfg(test)]
pub(super) const CONTENT_BUDGET: usize = CONTENT_CAPACITY * CONTENT_SLOTS;

#[derive(Default)]
struct ContentBlock {
    artifact: u64,
    start: usize,
    bytes: Vec<u8>,
    pending: Option<(u64, usize)>,
}

thread_local! {
    static CONTENT: RefCell<[ContentBlock; CONTENT_SLOTS]> =
        RefCell::new(std::array::from_fn(|_| ContentBlock::default()));
}

fn content(artifact: &Artifact, start: usize, end: usize) -> Result<Vec<u8>, Error> {
    let block_start = start / CONTENT_CAPACITY * CONTENT_CAPACITY;
    if end - block_start > CONTENT_CAPACITY {
        return artifact.bytes(start, end);
    }
    if start == end {
        return Ok(Vec::new());
    }

    CONTENT.with(|blocks| {
        let mut blocks = blocks.borrow_mut();
        let slot =
            (start / CONTENT_CAPACITY).wrapping_add(artifact.window_id as usize) % CONTENT_SLOTS;
        let block = blocks
            .get_mut(slot)
            .ok_or(Error::Corrupt("pack content window slot"))?;
        if block.artifact != artifact.window_id || block.start != block_start {
            let key = (artifact.window_id, block_start);
            if block.pending != Some(key) {
                // Isolated misses keep their small read and leave cached history regions intact.
                // Admit a block only after another miss in the same region establishes reuse.
                block.pending = Some(key);
                return artifact.bytes(start, end);
            }
            // Publish only complete fills. Reserve the full capacity even when EOF clips the
            // first fill, so reusing the slot later cannot grow it beyond the cache budget.
            block.artifact = 0;
            block.start = block_start;
            if block.bytes.capacity() == 0 {
                block.bytes = Vec::with_capacity(CONTENT_CAPACITY);
            }
            let count = CONTENT_CAPACITY.min(artifact.len - block_start);
            block.bytes.resize(count, 0);
            artifact.read_at(block_start, &mut block.bytes)?;
            block.artifact = artifact.window_id;
        }
        block
            .bytes
            .get(start - block_start..end - block_start)
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
    CONTENT.with(|blocks| {
        blocks
            .borrow()
            .iter()
            .map(|block| block.bytes.capacity())
            .sum()
    })
}

#[cfg(test)]
pub(super) fn content_max_slot_capacity() -> usize {
    CONTENT.with(|blocks| {
        blocks
            .borrow()
            .iter()
            .map(|block| block.bytes.capacity())
            .max()
            .unwrap_or(0)
    })
}
