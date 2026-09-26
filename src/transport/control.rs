use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Cancellation and an optional absolute deadline for an owned transport.
///
/// Native local adapters check this control between repository, graph, pack and publication steps.
/// The HTTP adapter polls it during async network waits; SSH uses it for async OpenSSH pipe and
/// exit waits. Each adapter documents its own scope and runtime requirements.
///
/// Set `cancel` from another thread and leave it set until the operation returns. The deadline
/// expires at the caller's chosen [`Instant`] and is never reset by traffic. This is not a hard
/// real-time bound: path resolution, caller callbacks, filesystem calls, hashing, decoding and
/// other synchronous computation cannot be forcibly interrupted. Network waits check at most
/// every 20 ms, subject to OS scheduling.
///
/// Cancellation wins when both controls are observed at one check. A completed protocol error or
/// status is not replaced by a later interruption. While waiting for exit, an already observable
/// exit takes precedence. A complete push report still needs EOF and successful server exit;
/// interruption before then retains its acknowledgements in an uncertain error.
///
/// SSH's owned process starts in a new process group on macOS/Linux and is cleaned up by its
/// adapter. Native local transfer starts no process or worker. No path is an execution sandbox.
///
/// Server stderr is drained and discarded, rather than inherited: a blocked diagnostic sink
/// must not stall the operation. Fetch sideband progress and push status messages remain available.
/// Generic protocol stream APIs cannot enforce interruption of caller-owned I/O.
#[derive(Debug, Clone, Copy)]
pub struct TransportControl<'a> {
    /// Cooperative flag, also used to interrupt owned pipe and server-exit waits.
    pub cancel: &'a AtomicBool,
    /// Absolute deadline; `None` imposes no time limit.
    pub deadline: Option<Instant>,
}

impl<'a> TransportControl<'a> {
    /// Controls cancellation without imposing a deadline.
    pub fn new(cancel: &'a AtomicBool) -> Self {
        Self {
            cancel,
            deadline: None,
        }
    }

    pub(crate) fn check(self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(io::Error::other(Interruption::Cancelled))
        } else if self.deadline.is_some_and(|end| Instant::now() >= end) {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                Interruption::Deadline,
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub(crate) enum Interruption {
    #[error("transport cancelled")]
    Cancelled,
    #[error("transport deadline expired")]
    Deadline,
}

pub(crate) fn interruption(error: &io::Error) -> Option<Interruption> {
    error.get_ref()?.downcast_ref::<Interruption>().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_wins_when_deadline_also_expired() {
        let cancel = AtomicBool::new(true);
        let control = TransportControl {
            cancel: &cancel,
            deadline: Some(Instant::now()),
        };
        let error = control.check().unwrap_err();
        assert!(matches!(
            interruption(&error),
            Some(Interruption::Cancelled)
        ));
        // Read::read_exact and Write::write_all must not automatically retry cancellation forever.
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
    }
}
