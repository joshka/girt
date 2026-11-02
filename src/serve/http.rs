//! Git's smart HTTP protocol as request and response pieces, for any web server to carry.
//!
//! A smart HTTP server answers two kinds of request for each repository:
//!
//! - `GET <repo>/info/refs?service=git-upload-pack` (or `git-receive-pack`): the advertisement,
//!   written by [`Service::advertise`];
//! - `POST <repo>/git-upload-pack` (or `/git-receive-pack`): one request, answered by
//!   [`Service::respond`].
//!
//! With the `serve-async` feature, `Service::advertise_async` and `Service::respond_async` are
//! the entry points for async web servers; the synchronous methods suit blocking servers and CGI.
//!
//! Both responses carry [`Service::advertisement_content_type`] or
//! [`Service::result_content_type`] and the [`NO_CACHE_HEADERS`]. The client's `Git-Protocol`
//! header selects the protocol through [`Protocol::from_parameters`], and a request body sent
//! with `Content-Encoding: gzip` is decoded by [`request_body`]. Routing, authentication and
//! `http.receivePack` policy belong to the embedding server.
//!
//! # Examples
//!
//! Answer `GET /repo.git/info/refs?service=git-upload-pack` from a client asking for protocol v2:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::config::ConfigInputs;
//! use girt::serve::Protocol;
//! use girt::serve::http::{NO_CACHE_HEADERS, Service};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let path = directory.path().join("repo.git");
//! Repository::init(ObjectFormat::Sha1, &path, InitKind::Bare)?;
//!
//! let service = Service::from_query_value(b"git-upload-pack").ok_or("unknown service")?;
//! let protocol = Protocol::from_parameters(b"version=2");
//! let mut body = Vec::new();
//! service.advertise(
//!     &path,
//!     &ConfigInputs::default(),
//!     protocol,
//!     &mut body,
//!     &AtomicBool::new(false),
//! )?;
//!
//! assert_eq!(
//!     service.advertisement_content_type(),
//!     "application/x-git-upload-pack-advertisement"
//! );
//! assert_eq!(NO_CACHE_HEADERS[1], ("Pragma", "no-cache"));
//! assert!(body.starts_with(b"000eversion 2\n"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{Protocol, ReceivePack, ServeError, UploadPack};
use crate::config::ConfigInputs;
use crate::push::PushReport;
use crate::{Repository, packet};

/// The headers Git's `http-backend` sends so that no response is cached.
pub const NO_CACHE_HEADERS: [(&str, &str); 3] = [
    ("Expires", "Fri, 01 Jan 1980 00:00:00 GMT"),
    ("Pragma", "no-cache"),
    ("Cache-Control", "no-cache, max-age=0, must-revalidate"),
];

/// A smart HTTP service: fetch (`git-upload-pack`) or push (`git-receive-pack`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Service {
    /// Serves fetches and clones.
    UploadPack,
    /// Receives pushes.
    ReceivePack,
}

/// What a POST request did.
#[derive(Debug)]
#[non_exhaustive]
pub enum Outcome {
    /// A fetch request was answered.
    Fetch,
    /// A push was received; the report holds each reference's result.
    Push(PushReport),
}

impl Service {
    /// Reads the `service` query parameter of an `info/refs` request, or the last path segment of
    /// a POST request.
    ///
    /// ```
    /// use girt::serve::http::Service;
    ///
    /// assert_eq!(
    ///     Service::from_query_value(b"git-receive-pack"),
    ///     Some(Service::ReceivePack)
    /// );
    /// assert_eq!(Service::from_query_value(b"git-upload-archive"), None);
    /// ```
    pub fn from_query_value(value: &[u8]) -> Option<Self> {
        match value {
            b"git-upload-pack" => Some(Self::UploadPack),
            b"git-receive-pack" => Some(Self::ReceivePack),
            _ => None,
        }
    }

    /// The service name, as in URLs and content types.
    pub fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
        }
    }

    /// The `Content-Type` of an `info/refs` response.
    pub fn advertisement_content_type(self) -> &'static str {
        match self {
            Self::UploadPack => "application/x-git-upload-pack-advertisement",
            Self::ReceivePack => "application/x-git-receive-pack-advertisement",
        }
    }

    /// The `Content-Type` a POST request carries.
    pub fn request_content_type(self) -> &'static str {
        match self {
            Self::UploadPack => "application/x-git-upload-pack-request",
            Self::ReceivePack => "application/x-git-receive-pack-request",
        }
    }

    /// The `Content-Type` of a POST response.
    pub fn result_content_type(self) -> &'static str {
        match self {
            Self::UploadPack => "application/x-git-upload-pack-result",
            Self::ReceivePack => "application/x-git-receive-pack-result",
        }
    }

    /// Writes the body of an `info/refs` response for the repository at `path`.
    ///
    /// As with Git, a protocol v0 response starts with a `# service=` line and a flush; a client
    /// that asked for protocol v2 gets the advertisement alone.
    ///
    /// # Errors
    ///
    /// Returns failures to open the repository, read its references or write.
    pub fn advertise(
        self,
        path: &Path,
        config_inputs: &ConfigInputs,
        protocol: Protocol,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<(), ServeError> {
        if protocol == Protocol::V0 {
            let line = format!("# service={}\n", self.name());
            packet::packet(output, line.as_bytes(), cancel)?;
            packet::put(output, b"0000", cancel)?;
        }
        match self {
            Self::UploadPack => {
                let repository = open(path, config_inputs)?;
                let upload = UploadPack::new(&repository)?;
                match protocol {
                    Protocol::V0 => upload.advertise_v0(true, output, cancel),
                    Protocol::V2 => upload.advertise(Protocol::V2, output, cancel),
                }
            }
            Self::ReceivePack => {
                ReceivePack::new(path, config_inputs.clone()).advertise(output, cancel)
            }
        }
    }

    /// Answers one POST request for the repository at `path`, with default limits.
    ///
    /// Fetches are answered statelessly, so a protocol v0 request carries its wants and every
    /// `have` the client is still offering. Use [`respond_upload`] or [`respond_receive`] to set
    /// limits, policy or the reflog identity.
    ///
    /// # Errors
    ///
    /// Returns the failure that stopped the answer.
    pub fn respond(
        self,
        path: &Path,
        config_inputs: &ConfigInputs,
        protocol: Protocol,
        body: &mut impl Read,
        output: &mut impl Write,
        cancel: &AtomicBool,
    ) -> Result<Outcome, ServeError> {
        match self {
            Self::UploadPack => {
                let repository = open(path, config_inputs)?;
                let upload = UploadPack::new(&repository)?;
                respond_upload(&upload, protocol, body, output, cancel)?;
                Ok(Outcome::Fetch)
            }
            Self::ReceivePack => {
                let receive = ReceivePack::new(path, config_inputs.clone());
                Ok(Outcome::Push(respond_receive(
                    &receive, body, output, cancel,
                )?))
            }
        }
    }
}

#[cfg(feature = "serve-async")]
impl Service {
    /// Writes the body of an `info/refs` response to an async stream, as [`Self::advertise`]
    /// does, running the work on `executor`.
    ///
    /// Dropping the future cancels the work; see [`run_blocking`](super::run_blocking).
    ///
    /// # Errors
    ///
    /// Returns failures to open the repository, read its references or write, and
    /// [`ServeError::Executor`] when the executor dropped the work.
    pub async fn advertise_async(
        self,
        executor: impl super::BlockingExecutor,
        path: std::path::PathBuf,
        config_inputs: ConfigInputs,
        protocol: Protocol,
        output: impl tokio::io::AsyncWrite + Unpin,
    ) -> Result<(), ServeError> {
        super::run_blocking(
            executor,
            tokio::io::empty(),
            output,
            move |_, output, cancel| {
                self.advertise(&path, &config_inputs, protocol, output, cancel)
            },
        )
        .await
    }

    /// Answers one POST request from an async body to an async stream, as [`Self::respond`]
    /// does, running the work on `executor`.
    ///
    /// `content_encoding` is the request's `Content-Encoding` header, decoded as
    /// [`request_body`] does. The response streams to `output` while the pack is written, and a
    /// slow client stalls pack generation rather than growing a buffer. Dropping the future, or
    /// failing to write `output`, cancels the work.
    ///
    /// # Errors
    ///
    /// Returns the failure that stopped the answer, [`ServeError::Unsupported`] for an unknown
    /// content encoding and [`ServeError::Executor`] when the executor dropped the work.
    ///
    /// # Examples
    ///
    /// Answer a POST request in an axum handler, streaming the response body: see
    /// `examples/serve_http_axum.rs` for the full server.
    ///
    /// ```
    /// use std::path::PathBuf;
    ///
    /// use girt::config::ConfigInputs;
    /// use girt::serve::http::Service;
    /// use girt::serve::{Protocol, SpawnBlocking};
    /// use tokio::io::{AsyncRead, AsyncWrite};
    ///
    /// async fn post(
    ///     path: PathBuf,
    ///     protocol: Protocol,
    ///     body: impl AsyncRead + Unpin,
    ///     response: impl AsyncWrite + Unpin,
    /// ) -> Result<(), girt::serve::ServeError> {
    ///     Service::UploadPack
    ///         .respond_async(
    ///             SpawnBlocking,
    ///             path,
    ///             ConfigInputs::default(),
    ///             protocol,
    ///             None,
    ///             body,
    ///             response,
    ///         )
    ///         .await?;
    ///     Ok(())
    /// }
    /// ```
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors `respond` plus the executor and encoding"
    )]
    pub async fn respond_async(
        self,
        executor: impl super::BlockingExecutor,
        path: std::path::PathBuf,
        config_inputs: ConfigInputs,
        protocol: Protocol,
        content_encoding: Option<&[u8]>,
        body: impl tokio::io::AsyncRead + Unpin,
        output: impl tokio::io::AsyncWrite + Unpin,
    ) -> Result<Outcome, ServeError> {
        let gzip = match content_encoding {
            None | Some(b"identity") => false,
            Some(b"gzip" | b"x-gzip") => true,
            Some(_) => return Err(ServeError::Unsupported("request content encoding")),
        };
        super::run_blocking(executor, body, output, move |body, output, cancel| {
            let encoding = gzip.then_some(&b"gzip"[..]);
            let mut body = request_body(encoding, body)?;
            self.respond(&path, &config_inputs, protocol, &mut body, output, cancel)
        })
        .await
    }
}

/// Answers a `POST git-upload-pack` request with a configured [`UploadPack`].
///
/// # Errors
///
/// Returns the failure that stopped the answer.
pub fn respond_upload(
    upload: &UploadPack<'_>,
    protocol: Protocol,
    body: &mut impl Read,
    output: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<(), ServeError> {
    match protocol {
        Protocol::V2 => upload.respond(body, output, cancel),
        Protocol::V0 => upload.respond_v0(body, output, cancel),
    }
}

/// Answers a `POST git-receive-pack` request with a configured [`ReceivePack`].
///
/// # Errors
///
/// Returns the failure that stopped the answer.
pub fn respond_receive(
    receive: &ReceivePack,
    body: &mut impl Read,
    output: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<PushReport, ServeError> {
    receive.respond(body, output, cancel)
}

/// Wraps a request body for its `Content-Encoding`: `gzip` bodies are decoded, as Git clients
/// send large fetch requests, and others are read as they are.
///
/// # Errors
///
/// Returns [`ServeError::Unsupported`] for any other encoding.
pub fn request_body<'a, R: Read + 'a>(
    content_encoding: Option<&[u8]>,
    body: R,
) -> Result<Box<dyn Read + 'a>, ServeError> {
    match content_encoding {
        None | Some(b"identity") => Ok(Box::new(body)),
        Some(b"gzip" | b"x-gzip") => Ok(Box::new(flate2::read::GzDecoder::new(body))),
        Some(_) => Err(ServeError::Unsupported("request content encoding")),
    }
}

fn open(path: &Path, config_inputs: &ConfigInputs) -> Result<Repository, ServeError> {
    Repository::open_with_config(path, config_inputs)
        .map_err(|error| ServeError::Open(Box::new(error)))
}
