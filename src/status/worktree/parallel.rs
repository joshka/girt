//! Work distribution for the working-tree walk: a shared queue of directories, the workers that
//! drain it, and the budgets they share.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, MutexGuard, PoisonError};

use rustix::fd::OwnedFd;
use rustix::fs::{Stat, fstat};

use super::{Found, Scan, Visited, Walk, io};
use crate::ignore::Ignore;
use crate::status::types::check;
use crate::status::{Error, UntrackedKind, UntrackedPath};

/// A directory waiting for a worker.
pub(super) struct Task {
    pub(super) open: Open,
    pub(super) path: Vec<u8>,
    pub(super) depth: usize,
    pub(super) walk: Walk,
    /// Rules in force for this directory's children, before its own `.gitignore`.
    pub(super) rules: Option<Arc<Ignore>>,
    /// Whether to list the directory if it turns out to be a nested repository.
    pub(super) listed: bool,
}

/// How a [`Task`] reaches its directory.
pub(super) enum Open {
    Root(Arc<OwnedFd>),
    /// Opened relative to the parent, then compared with the parent's `lstat` of `name`. Holding
    /// the parent descriptor keeps the path resolution independent of later renames.
    Child {
        parent: Arc<OwnedFd>,
        name: Vec<u8>,
        stat: Stat,
    },
}

pub(super) struct Queue {
    /// Last in, first out, so pending tasks share few distinct parent descriptors.
    pub(super) tasks: Vec<Task>,
    /// Tasks taken but not finished; the walk ends when this and `tasks` are both empty.
    pub(super) active: usize,
    /// The first error any worker returned.
    pub(super) failure: Option<Error>,
}

/// A repository-wide allowance shared by every worker.
pub(super) struct Budget(AtomicUsize);

impl Budget {
    pub(super) fn new(remaining: usize) -> Self {
        Self(AtomicUsize::new(remaining))
    }

    /// Takes `count` from the allowance, or leaves it unchanged and returns false.
    pub(super) fn take(&self, count: usize) -> bool {
        let mut remaining = self.0.load(Ordering::Relaxed);
        loop {
            let Some(left) = remaining.checked_sub(count) else {
                return false;
            };
            match self.0.compare_exchange_weak(
                remaining,
                left,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => remaining = current,
            }
        }
    }

    pub(super) fn charge(&self, count: usize, label: &'static str) -> Result<(), Error> {
        if self.take(count) {
            Ok(())
        } else {
            Err(Error::Limit(label))
        }
    }

    pub(super) fn remaining(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// Finishes a taken task even if it panics, so other workers stop waiting for it.
struct Taken<'s, 'a>(&'s Scan<'a>);

impl Drop for Taken<'_, '_> {
    fn drop(&mut self) {
        let mut queue = self.0.lock();
        queue.active -= 1;
        if std::thread::panicking() {
            self.0.stopped.store(true, Ordering::Relaxed);
        }
        if queue.active == 0 || self.0.stopped.load(Ordering::Relaxed) {
            self.0.ready.notify_all();
        }
    }
}

impl Scan<'_> {
    /// Walks every queued directory on up to `threads` workers, the caller's thread included.
    pub(super) fn run(&self, threads: usize) -> Result<Found, Error> {
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(threads)
            .max(1);
        let mut found = Found::default();
        std::thread::scope(|scope| {
            // A worker that cannot be spawned only reduces parallelism.
            let workers: Vec<_> = (1..threads)
                .filter_map(|_| {
                    std::thread::Builder::new()
                        .spawn_scoped(scope, || {
                            let mut found = Found::default();
                            self.work(&mut found);
                            found
                        })
                        .ok()
                })
                .collect();
            self.work(&mut found);
            for worker in workers {
                match worker.join() {
                    Ok(other) => found.extend(other),
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
        });
        match self.lock().failure.take() {
            Some(error) => Err(error),
            None => Ok(found),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn work(&self, found: &mut Found) {
        while let Some(task) = self.next() {
            let taken = Taken(self);
            if let Err(error) = self.task(found, task) {
                let mut queue = self.lock();
                if !self.stopped.swap(true, Ordering::Relaxed) {
                    queue.failure = Some(error);
                }
            }
            drop(taken);
        }
    }

    fn next(&self) -> Option<Task> {
        let mut queue = self.lock();
        loop {
            if self.stopped.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(task) = queue.tasks.pop() {
                queue.active += 1;
                return Some(task);
            }
            if queue.active == 0 {
                return None;
            }
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub(super) fn push(&self, task: Task) {
        self.lock().tasks.push(task);
        self.ready.notify_one();
    }

    /// Checks for cancellation, or for another worker's failure.
    pub(super) fn check(&self) -> Result<(), Error> {
        if self.stopped.load(Ordering::Relaxed) {
            // The failing worker's error is reported instead.
            return Err(Error::Cancelled);
        }
        check(self.cancel)
    }

    fn task(&self, found: &mut Found, task: Task) -> Result<(), Error> {
        let Task {
            open,
            path,
            depth,
            walk,
            rules,
            listed,
        } = task;
        match open {
            Open::Root(fd) => {
                let before = fstat(&*fd).map_err(|source| io(&path, source))?;
                self.directory(found, &fd, &before, &path, depth, walk, rules)?;
            }
            Open::Child { parent, name, stat } => {
                let visited =
                    self.child_directory(found, &parent, &name, &path, &stat, depth, walk, rules)?;
                if listed && visited == Visited::Repository && self.selects_directory(&path) {
                    found.listed.push(UntrackedPath {
                        path,
                        kind: UntrackedKind::Repository,
                    });
                }
            }
        }
        Ok(())
    }
}
