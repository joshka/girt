use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Cancellation and an optional absolute deadline for an owned transport.
///
/// The following process/pipe guarantees apply to local adapters. With the `http` feature, the
/// HTTP adapter polls this same control during async network waits; `ssh` also uses it for async
/// OpenSSH pipe and exit waits. Each adapter module documents the
/// network-specific scope, runtime requirements and separate synchronous validation step.
///
/// Set `cancel` from another thread and leave it set until the operation returns. The deadline
/// expires at the caller's chosen [`Instant`] and is never reset by traffic. Checks begin before
/// path resolution/spawn and end when protocol completion and server exit have been observed.
/// Pipe reads, writes, and exit waits check at most every 20 ms while waiting, subject to OS
/// scheduling. This is not a hard real-time bound: path resolution, spawn, caller callbacks,
/// hashing, decoding, and other synchronous computation cannot be forcibly interrupted.
///
/// Cancellation wins when both controls are observed at one check. A completed protocol error or
/// status is not replaced by a later interruption. While waiting for exit, an already observable
/// exit takes precedence. A complete push report still needs EOF and successful server exit;
/// interruption before then retains its acknowledgements in an uncertain error.
///
/// On macOS/Linux, owned servers start in a new process group. Cleanup sends SIGKILL to that
/// group **before** reaping the direct child, including on success or unwinding. The unreaped
/// leader reserves the group ID. Callers must not reap girt's children through a global SIGCHLD
/// handler or use automatic child reaping. Descendants that deliberately leave the group are
/// outside this guarantee; this is lifecycle management for trusted servers, not a sandbox.
/// Signalable descendants are killed; elevated-privilege processes are outside the contract.
/// Only the direct child can be reaped by girt. Kernel-delayed exit
/// and reaping can extend elapsed time beyond the deadline. No I/O workers are created.
///
/// Server stderr is drained and discarded, rather than inherited: a blocked diagnostic sink
/// must not stall the operation. Fetch sideband progress and push status messages remain available.
/// Other OSes return an unsupported I/O error before starting a server. Generic protocol stream
/// APIs cannot enforce these guarantees; their owners must arrange interruption themselves.
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
