//! Exclusive, opt-in foreground ownership of a caller's controlling terminal.
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rustix::fs::{OFlags, fcntl_getfl};
use rustix::process::{Pid, Signal, getpgrp, getsid, kill_process_group};
use rustix::termios::{
    LocalModes, OptionalActions, QueueSelector, Termios, tcflush, tcgetattr, tcgetpgrp, tcgetsid,
    tcsetattr, tcsetpgrp,
};

use super::SshError;

// A process has one controlling terminal. Serialize even independently prepared attachments.
static ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(super) static FAIL_RESUME: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(super) static FAIL_FLUSH: AtomicBool = AtomicBool::new(false);

/// An opt-in lease of the caller's controlling terminal for ordinary OpenSSH authentication.
///
/// Attach with [`super::SshRemote::with_terminal`]. The caller must exclusively coordinate terminal
/// I/O, job control and child reaping for the operation, including other threads and libraries.
/// Only one girt terminal session may run in the process. OpenSSH retains its usual authentication
/// and host-key policy; Git protocol stdin/stdout and captured diagnostic stderr stay separate.
/// Terminal prompts through `/dev/tty` go directly to this terminal.
///
/// The caller must keep polling the paired [`TerminalEvents`] while awaiting the operation, and
/// choose how to suspend its own job after receiving a stop. No caller callback runs inside the
/// transport. Dropping the event receiver or a stop permit cancels the session. Cancellation and
/// explicit deadlines remain active while stopped; this API adds no default prompt deadline.
///
/// The caller must not concurrently change foreground groups, terminal settings, signal handlers
/// or masks used for job control. Descendants escaping the owned process group remain outside the
/// cleanup contract. Abrupt process termination cannot run the restoration guard.
pub struct ForegroundTerminal {
    shared: Arc<Shared>,
}

/// A bounded, nonblocking receiver for terminal stops from its paired attachment.
///
/// Poll alongside the SSH future. A stop is published only after the caller's terminal attributes
/// and foreground process group have been restored. Dropping this receiver cancels any active
/// session and prevents subsequent launches with its attachment.
pub struct TerminalEvents {
    shared: Arc<Shared>,
}

/// Single-use permission to resume a stopped SSH session after the caller regains the foreground.
///
/// Dropping this value cancels its session. A permit from a completed session cannot affect a later
/// session on the same attachment. It does not resume or suspend the caller's own process group.
pub struct StoppedTerminal {
    shared: Arc<Shared>,
    generation: u64,
    signal: i32,
    granted: bool,
}

struct Shared {
    fd: OwnedFd,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    generation: u64,
    active: bool,
    receiver_gone: bool,
    cancelled: bool,
    stop: Option<i32>,
    resume: bool,
}

impl Shared {
    fn state(&self) -> Result<MutexGuard<'_, State>, SshError> {
        self.state.lock().map_err(|_| SshError::Cancelled)
    }
}

impl std::fmt::Debug for ForegroundTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForegroundTerminal").finish_non_exhaustive()
    }
}

impl ForegroundTerminal {
    /// Validates an owned descriptor for this process's foreground controlling terminal.
    ///
    /// Performs no terminal mutation or process launch. Rechecks these conditions immediately
    /// before every launch. Supply a read/write descriptor opened by the caller; no implicit
    /// `/dev/tty` lookup occurs. The paired receiver must remain alive through the operation.
    ///
    /// # Errors
    ///
    /// Rejects a non-controlling terminal, a background caller, `TOSTOP`, and terminal I/O errors.
    pub fn prepare(fd: OwnedFd) -> Result<(Self, TerminalEvents), SshError> {
        snapshot(&fd)?;
        let shared = Arc::new(Shared {
            fd,
            state: Mutex::new(State::default()),
        });
        Ok((
            Self {
                shared: shared.clone(),
            },
            TerminalEvents { shared },
        ))
    }

    pub(super) fn acquire(&self) -> Result<Lease, SshError> {
        let mut state = self.shared.state()?;
        if state.receiver_gone || state.cancelled {
            return Err(SshError::Cancelled);
        }
        if state.active || ACTIVE.swap(true, Ordering::AcqRel) {
            return Err(SshError::Configuration("terminal already leased"));
        }
        let (caller, attributes) = match snapshot(&self.shared.fd) {
            Ok(value) => value,
            Err(error) => {
                ACTIVE.store(false, Ordering::Release);
                return Err(error);
            }
        };
        state.generation = state.generation.wrapping_add(1);
        state.active = true;
        state.stop = None;
        state.resume = false;
        Ok(Lease {
            shared: self.shared.clone(),
            caller,
            attributes,
            suspended: None,
            restored: false,
        })
    }
}

impl TerminalEvents {
    /// Takes the next stop permit without waiting or invoking application code.
    ///
    /// `None` means no stop is pending; operation completion is reported by the SSH future.
    ///
    /// # Errors
    ///
    /// Returns cancellation if an earlier stop permit was dropped or internal state was poisoned.
    pub fn try_next_stop(&mut self) -> Result<Option<StoppedTerminal>, SshError> {
        let mut state = self.shared.state()?;
        if state.cancelled {
            return Err(SshError::Cancelled);
        }
        Ok(state.stop.take().map(|signal| StoppedTerminal {
            shared: self.shared.clone(),
            generation: state.generation,
            signal,
            granted: false,
        }))
    }
}

impl Drop for TerminalEvents {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.receiver_gone = true;
            state.cancelled = true;
        }
    }
}

impl StoppedTerminal {
    /// Returns the OS signal number that stopped the SSH process.
    pub fn signal(&self) -> i32 {
        self.signal
    }

    /// Grants permission to resume after checking that the caller is currently foreground.
    ///
    /// The transport rechecks foreground ownership before restoring SSH attributes, transferring
    /// the terminal and continuing its process group. This method does not itself resume a process.
    ///
    /// # Errors
    ///
    /// Rejects a stale/cancelled permit, a background caller, `TOSTOP` or terminal I/O failure.
    /// A rejected permit cancels its still-active session; it cannot be retried.
    pub fn resume(mut self) -> Result<(), SshError> {
        let mut state = self.shared.state()?;
        if !state.active || state.generation != self.generation || state.cancelled {
            return Err(SshError::Cancelled);
        }
        snapshot(&self.shared.fd)?;
        state.resume = true;
        self.granted = true;
        Ok(())
    }
}

impl Drop for StoppedTerminal {
    fn drop(&mut self) {
        if !self.granted
            && let Ok(mut state) = self.shared.state.lock()
            && state.active
            && state.generation == self.generation
        {
            state.cancelled = true;
        }
    }
}

/// Armed before spawn, so even an exec failure restores the caller's terminal.
pub(super) struct Lease {
    shared: Arc<Shared>,
    caller: Pid,
    attributes: Termios,
    suspended: Option<Termios>,
    restored: bool,
}

impl Lease {
    pub(super) fn prepare_command(&self, command: &mut Command) -> Result<(), SshError> {
        let fd = self.shared.fd.as_raw_fd();
        let blocked = ttou_set();
        // SAFETY: All captured data is prepared before fork and the descriptor is owned by this
        // live parent guard through spawn. The hook uses only async-signal-safe POSIX calls;
        // there is no allocation, lock, environment access, logging, or application callback.
        // The hook establishes its own group before handoff. Restore the thread's previous signal
        // mask before exec, including when foreground handoff fails.
        unsafe {
            command.pre_exec(move || child_handoff(fd, &blocked));
        }
        Ok(())
    }

    pub(super) fn check(&mut self, pid: Pid) -> Result<bool, SshError> {
        let resume = {
            let mut state = self.shared.state()?;
            if state.cancelled || state.receiver_gone {
                return Err(SshError::Cancelled);
            }
            std::mem::take(&mut state.resume)
        };
        if resume {
            snapshot(&self.shared.fd)?;
            let attributes = self.suspended.as_ref().ok_or(SshError::Cancelled)?;
            self.restored = false;
            with_sigttou_blocked(|| {
                tcsetattr(&self.shared.fd, OptionalActions::Now, attributes)?;
                #[cfg(test)]
                if FAIL_RESUME.swap(false, Ordering::Relaxed) {
                    return Err(rustix::io::Errno::IO);
                }
                tcsetpgrp(&self.shared.fd, pid)
            })?;
            kill_process_group(pid, Signal::CONT).map_err(io::Error::from)?;
            self.suspended = None;
        }
        Ok(self.suspended.is_some())
    }

    pub(super) fn stopped(&mut self, pid: Pid, signal: i32) -> Result<(), SshError> {
        kill_process_group(pid, Signal::STOP).map_err(io::Error::from)?;
        if self.suspended.is_some() {
            return Ok(());
        }
        self.suspended = Some(tcgetattr(&self.shared.fd).map_err(io::Error::from)?);
        self.restore(false)?;
        let mut state = self.shared.state()?;
        state.stop = Some(signal);
        Ok(())
    }

    pub(super) fn continued(&self, pid: Pid) -> Result<(), SshError> {
        // An outside SIGCONT cannot grant terminal ownership while a permit is outstanding.
        if self.suspended.is_some() {
            kill_process_group(pid, Signal::STOP).map_err(io::Error::from)?;
        }
        Ok(())
    }

    pub(super) fn restore(&mut self, completed: bool) -> Result<(), SshError> {
        if !self.restored {
            let mut returned = false;
            let result = with_sigttou_blocked(|| {
                // A killed/stopped password reader cannot consume its unfinished private input.
                // Also clear input on a clean exit that leaves echo disabled. Never touch input
                // again after a successful stop restoration has returned ownership to the caller.
                let ordinary_input = tcgetattr(&self.shared.fd).is_ok_and(|attributes| {
                    attributes.local_modes.contains(LocalModes::ECHO)
                        && attributes.input_modes == self.attributes.input_modes
                        && attributes.local_modes == self.attributes.local_modes
                });
                let input = if completed && ordinary_input {
                    Ok(())
                } else {
                    tcflush(&self.shared.fd, QueueSelector::IFlush)
                };
                #[cfg(test)]
                let input = if FAIL_FLUSH.swap(false, Ordering::Relaxed) {
                    Err(rustix::io::Errno::IO)
                } else {
                    input
                };
                // Attempt both restorations even if flushing or restoring attributes fails.
                let attributes = tcsetattr(&self.shared.fd, OptionalActions::Now, &self.attributes);
                let foreground = tcsetpgrp(&self.shared.fd, self.caller);
                returned = foreground.is_ok();
                input.and(attributes).and(foreground)
            });
            // Once ownership returned, even a flush/attribute/mask error must not cause Drop to
            // mutate the caller's new input or another foreground job's terminal state.
            self.restored = returned;
            result?;
        }
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.restore(false);
        if let Ok(mut state) = self.shared.state.lock() {
            state.active = false;
            state.stop = None;
            state.resume = false;
            // Permit cancellation belongs to the completed session, receiver loss is permanent.
            state.cancelled = state.receiver_gone;
        }
        ACTIVE.store(false, Ordering::Release);
    }
}

fn snapshot(fd: &OwnedFd) -> Result<(Pid, Termios), SshError> {
    if (fcntl_getfl(fd).map_err(io::Error::from)? & OFlags::ACCMODE) != OFlags::RDWR {
        return Err(SshError::Configuration(
            "terminal descriptor must be read/write",
        ));
    }
    let caller = getpgrp();
    let session = getsid(None).map_err(io::Error::from)?;
    if tcgetsid(fd).map_err(io::Error::from)? != session
        || tcgetpgrp(fd).map_err(io::Error::from)? != caller
    {
        return Err(SshError::Configuration(
            "caller does not own foreground terminal",
        ));
    }
    let attributes = tcgetattr(fd).map_err(io::Error::from)?;
    if attributes.local_modes.contains(LocalModes::TOSTOP) {
        return Err(SshError::Configuration("terminal TOSTOP is unsupported"));
    }
    Ok((caller, attributes))
}

fn ttou_set() -> libc::sigset_t {
    // SAFETY: sigemptyset initializes the writable set; SIGTTOU is a valid signal number.
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTTOU);
        set
    }
}

fn child_handoff(fd: RawFd, blocked: &libc::sigset_t) -> io::Result<()> {
    // SAFETY: The set and output mask are valid storage, and the descriptor is kept open by the
    // parent through spawn. These calls are async-signal-safe; errno is read immediately on error.
    unsafe {
        if libc::setpgid(0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut previous = std::mem::zeroed();
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, blocked, &mut previous);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let result = libc::tcsetpgrp(fd, libc::getpgrp());
        let error = (result != 0).then(io::Error::last_os_error);
        let restored = libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if let Some(error) = error {
            Err(error)
        } else if restored != 0 {
            Err(io::Error::from_raw_os_error(restored))
        } else {
            Ok(())
        }
    }
}

pub(super) fn with_sigttou_blocked(
    operation: impl FnOnce() -> rustix::io::Result<()>,
) -> io::Result<()> {
    let blocked = ttou_set();
    // SAFETY: The pointers refer to initialized signal sets. Only this thread's signal mask is
    // changed, and it is restored before returning. The closure contains only terminal syscalls.
    unsafe {
        let mut previous = std::mem::zeroed();
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let result = operation().map_err(io::Error::from);
        let restored = libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if restored != 0 {
            return result.and(Err(io::Error::from_raw_os_error(restored)));
        }
        result
    }
}
