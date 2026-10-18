//! Reads ahead of the retention walk on worker threads.
//!
//! The walk visits identities in breadth-first order and applies every limit and kind check on
//! the calling thread. Reading and parsing an object depends only on its identity, so workers
//! can do it for identities already queued while the walk is still processing earlier ones. The
//! walk consumes results in queue order, which keeps its result and its first error independent
//! of the thread count.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Mutex, mpsc};

use super::{RetentionError, Visit};
use crate::ObjectId;

/// Reads and parses one object, independently of walk state.
pub(super) type Visitor<'a> = dyn Fn(ObjectId) -> Result<Visit, RetentionError> + Sync + 'a;

/// Objects each worker may have read ahead of the walk. Each result holds only an object's
/// edges, and the walk consumes them in order, so this bounds the memory held for results.
const AHEAD_PER_THREAD: usize = 64;

type Outcome = Result<Result<Visit, RetentionError>, Box<dyn Any + Send>>;

/// The walk's queue of identities, with reads dispatched ahead to workers when there are any.
pub(super) struct Reader<'a> {
    visit: &'a Visitor<'a>,
    /// Queued identities in walk order, with their sequence numbers and whether they are read.
    queue: VecDeque<(u64, ObjectId, bool)>,
    next_seq: u64,
    workers: Option<Workers>,
}

struct Workers {
    tasks: mpsc::Sender<(u64, ObjectId)>,
    results: mpsc::Receiver<(u64, Outcome)>,
    /// Results that arrived before the walk reached them.
    ready: HashMap<u64, Outcome>,
    /// Every queued read before this sequence number has been dispatched.
    dispatched_to: u64,
    /// Dispatched reads whose results the walk has not consumed yet.
    in_flight: usize,
    capacity: usize,
}

/// Runs `body` with a reader that uses up to `max_threads` workers, fewer when the host has
/// fewer, and none when fewer than two are allowed or available.
pub(super) fn with_reader<T>(
    max_threads: usize,
    visit: &Visitor<'_>,
    body: impl FnOnce(&mut Reader<'_>) -> Result<T, RetentionError>,
) -> Result<T, RetentionError> {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(max_threads);
    if threads < 2 {
        return body(&mut Reader {
            visit,
            queue: VecDeque::new(),
            next_seq: 0,
            workers: None,
        });
    }
    let (tasks, queue) = mpsc::channel::<(u64, ObjectId)>();
    let (done, results) = mpsc::channel();
    let queue = Mutex::new(queue);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let done = done.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let task = queue.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    let Ok((seq, id)) = task else {
                        return;
                    };
                    // A panic is handed to the walk, which would otherwise wait for this result.
                    let outcome = catch_unwind(AssertUnwindSafe(|| visit(id)));
                    if done.send((seq, outcome)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(done);
        let mut reader = Reader {
            visit,
            queue: VecDeque::new(),
            next_seq: 0,
            workers: Some(Workers {
                tasks,
                results,
                ready: HashMap::new(),
                dispatched_to: 0,
                in_flight: 0,
                capacity: threads * AHEAD_PER_THREAD,
            }),
        };
        // Dropping the reader closes the task queue, so idle workers stop and the scope joins.
        body(&mut reader)
    })
}

impl Reader<'_> {
    /// Queues `id` at the back of the walk; `read` says whether the walk needs its content.
    pub(super) fn push(&mut self, id: ObjectId, read: bool) {
        self.queue.push_back((self.next_seq, id, read));
        self.next_seq += 1;
    }

    /// The next queued identity, with its read result if it was queued to be read.
    pub(super) fn next(&mut self) -> Option<(ObjectId, Option<Result<Visit, RetentionError>>)> {
        let Some(workers) = &mut self.workers else {
            let (_, id, read) = self.queue.pop_front()?;
            return Some((id, read.then(|| (self.visit)(id))));
        };
        workers.dispatch(&self.queue);
        let (seq, id, read) = self.queue.pop_front()?;
        if !read {
            return Some((id, None));
        }
        // `dispatch` sent this read: it is the earliest queued, so nothing else can be in flight
        // ahead of it.
        workers.in_flight -= 1;
        let outcome = match workers.ready.remove(&seq) {
            Some(outcome) => outcome,
            None => loop {
                // Workers hold the result senders until the task queue closes, which only
                // dropping this reader does, so receiving cannot fail while a read is in flight.
                let (arrived, outcome) = workers
                    .results
                    .recv()
                    .expect("walk workers outlive their reader");
                if arrived == seq {
                    break outcome;
                }
                workers.ready.insert(arrived, outcome);
            },
        };
        Some((
            id,
            Some(outcome.unwrap_or_else(|panic| resume_unwind(panic))),
        ))
    }
}

impl Workers {
    /// Sends queued reads to the workers, in walk order, up to the read-ahead capacity.
    fn dispatch(&mut self, queue: &VecDeque<(u64, ObjectId, bool)>) {
        let Some(&(first, _, _)) = queue.front() else {
            return;
        };
        let start = usize::try_from(self.dispatched_to.saturating_sub(first))
            .map_or(queue.len(), |start| start.min(queue.len()));
        for &(seq, id, read) in queue.range(start..) {
            if self.in_flight == self.capacity {
                return;
            }
            if read {
                // Workers stop only when the task queue closes, so sending cannot fail here.
                if self.tasks.send((seq, id)).is_err() {
                    return;
                }
                self.in_flight += 1;
            }
            self.dispatched_to = seq + 1;
        }
    }
}
