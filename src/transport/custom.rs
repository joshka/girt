//! Transports an application supplies for URL schemes girt does not handle itself.

use std::io::{Read, Write};

use super::RequestedProtocol;
use crate::remote::Destination;

/// Carries the Git protocol for one URL scheme, supplied by the application.
///
/// [`crate::workflow`] asks
/// [`TransferCallbacks::transport`](crate::workflow::TransferCallbacks::transport) for a transport
/// when a remote's URL has a scheme girt does not handle, such as `mem://repo` or `vault::team/
/// project`. The transport opens one [`Connection`] per service; girt then speaks the ordinary Git
/// pack protocol over it, as it does over SSH. An in-memory transport for tests is a pair of pipes
/// to `git upload-pack` or `git receive-pack`, or to a server written in Rust.
///
/// Like any unknown scheme, the URL must be allowed by `protocol.<scheme>.allow` (or
/// `GIT_ALLOW_PROTOCOL`): registering a transport does not override the user's protocol policy.
///
/// # Examples
///
/// A transport that serves `local::<path>` URLs by running `git upload-pack`:
///
/// ```no_run
/// use std::process::{Command, Stdio};
///
/// use girt::transport::{ConnectError, ConnectRequest, Connection, Transport};
///
/// struct GitProcess;
///
/// impl Transport for GitProcess {
///     fn connect(
///         &self,
///         request: &ConnectRequest<'_>,
///     ) -> Result<Box<dyn Connection>, ConnectError> {
///         let program = request.service().name();
///         let path = String::from_utf8_lossy(request.address()).into_owned();
///         let mut command = Command::new(program);
///         if let Some(protocol) = request.protocol().git_protocol() {
///             command.env("GIT_PROTOCOL", protocol);
///         }
///         let child = command
///             .arg(path)
///             .stdin(Stdio::piped())
///             .stdout(Stdio::piped())
///             .spawn()
///             .map_err(ConnectError::new)?;
///         Ok(Box::new(girt::transport::ChildConnection::new(child)))
///     }
/// }
/// ```
pub trait Transport: Send + Sync {
    /// Opens a connection to `request`'s service at its address.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectError`] when the service cannot be reached; the transfer then fails
    /// without having sent anything.
    fn connect(&self, request: &ConnectRequest<'_>) -> Result<Box<dyn Connection>, ConnectError>;
}

/// A full-duplex byte stream to one Git service, as `git upload-pack` or `git receive-pack`
/// would provide over SSH.
///
/// Reads return what the service writes; writes go to its input. girt reads and writes in turn,
/// so a blocking implementation works.
pub trait Connection: Read + Write + Send {
    /// Ends the connection after girt is done with it, and reports whether the service ended
    /// cleanly, as an SSH client reports the remote command's exit status.
    ///
    /// The default does nothing and succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectError`] when the service failed. girt keeps what it already received.
    fn finish(self: Box<Self>) -> Result<(), ConnectError> {
        Ok(())
    }
}

/// What a [`Transport`] is asked to connect to.
#[derive(Debug)]
pub struct ConnectRequest<'a> {
    pub(crate) destination: &'a Destination,
    pub(crate) service: Service,
    pub(crate) protocol: RequestedProtocol,
}

impl ConnectRequest<'_> {
    /// The remote's URL after rewrites, as configured.
    pub fn url(&self) -> &[u8] {
        self.destination.bytes()
    }

    /// The rest of the URL after `<scheme>::` or `<scheme>://`, unparsed.
    ///
    /// For `mem::/srv/repo` it is `/srv/repo`; for `vault://team/project` it is `team/project`.
    /// The transport decides what the address means.
    pub fn address(&self) -> &[u8] {
        let url = self.destination.bytes();
        let scheme = self.destination.helper_scheme().map_or(0, <[u8]>::len);
        let rest = &url[scheme..];
        rest.strip_prefix(b"::")
            .or_else(|| rest.strip_prefix(b"://"))
            .unwrap_or(rest)
    }

    /// The URL scheme the transport was registered for.
    pub fn scheme(&self) -> &[u8] {
        self.destination.helper_scheme().unwrap_or_default()
    }

    /// The Git service to connect to.
    pub fn service(&self) -> Service {
        self.service
    }

    /// The upload-pack protocol version girt asks for.
    ///
    /// Pass [`RequestedProtocol::git_protocol`] to the service the way SSH passes
    /// `GIT_PROTOCOL`, for example `GIT_PROTOCOL=version=2` in the environment of
    /// `git upload-pack`. A service that ignores it answers with v0, which girt accepts.
    pub fn protocol(&self) -> RequestedProtocol {
        self.protocol
    }
}

/// A Git service a [`Connection`] reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Service {
    /// `git-upload-pack`: discovery and fetch.
    UploadPack,
    /// `git-receive-pack`: push.
    ReceivePack,
}

impl Service {
    /// The service's program name, `git-upload-pack` or `git-receive-pack`.
    pub fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
        }
    }
}

/// A [`Transport`] or [`Connection`] failure, carrying the application's own error.
#[derive(Debug, thiserror::Error)]
#[error("transport: {0}")]
pub struct ConnectError(#[source] Box<dyn std::error::Error + Send + Sync>);

impl ConnectError {
    /// Wraps an application error.
    pub fn new(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self(error.into())
    }
}

/// A [`Connection`] over a child process's standard input and output.
///
/// [`Connection::finish`] closes its input, waits for it, and fails when it exits unsuccessfully.
#[derive(Debug)]
pub struct ChildConnection {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: std::process::ChildStdout,
    // Set once the child has been waited for.
    reaped: bool,
}

impl ChildConnection {
    /// Wraps a child started with piped standard input and output.
    ///
    /// # Panics
    ///
    /// Panics when the child's standard input or output was not piped.
    pub fn new(mut child: std::process::Child) -> Self {
        let input = child.stdin.take().expect("piped standard input");
        let output = child.stdout.take().expect("piped standard output");
        Self {
            child,
            input: Some(input),
            output,
            reaped: false,
        }
    }
}

impl Read for ChildConnection {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.output.read(buffer)
    }
}

impl Write for ChildConnection {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match &mut self.input {
            Some(input) => input.write(buffer),
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.input.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl Connection for ChildConnection {
    fn finish(mut self: Box<Self>) -> Result<(), ConnectError> {
        drop(self.input.take());
        let status = self.child.wait().map_err(ConnectError::new)?;
        self.reaped = true;
        if status.success() {
            Ok(())
        } else {
            Err(ConnectError::new(format!("service exited with {status}")))
        }
    }
}

impl Drop for ChildConnection {
    // A connection dropped without `finish` must not leave the service running.
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
