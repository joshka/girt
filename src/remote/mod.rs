//! Named remote configuration and pure refspec mapping.
//!
//! # URLs, mappings, and credentials
//!
//! - [`RemoteUrls::find`] reads URL values without parsing refspecs; [`Remote::find`] also loads
//!   fetch and push [`Refspecs`] from an explicit configuration snapshot.
//! - [`ConfiguredRefspec`] describes broader configured syntax, including shorthand and defaults,
//!   without resolving or executing it. It does not change the strict [`Refspecs`] mapping subset.
//! - [`ConfiguredRemote`] validates ordinary remote snapshots and configured refspecs, with an
//!   explicit compatibility boundary for uncharacterized URL syntax.
//! - [`Refspecs`] maps supplied sources to destinations without I/O or update authorization.
//!   [`Destination`] identifies a selected local or network endpoint separately.
//! - [`ParsedUrl`] exposes a URL's original bytes, host, and path for presentation, and expands
//!   local paths against an explicit base without accessing the filesystem.
//! - [`canonicalize_user_url`] validates user input and expands local symlinks while permitting
//!   nonexistent path suffixes; its result retains credentials for storage, not logging.
//! - [`parse_configured_url`] returns interpreted host/path components without filesystem access;
//!   scheme paths are decoded while local, file and scp paths remain literal.
//! - [`RemoteConfig`] edits named remote configuration. [`CredentialSession`] discovers credentials
//!   only through an application-approved helper lifecycle; [`CredentialStore`] answers Git's
//!   `store` helper natively.
//!
//! Transport choice, URL rewriting, implicit branch selection, tag following, pruning, mirror
//! policy and other remote options remain the caller's responsibility.
//! This is not a complete interpretation of `git fetch <remote>` or `git push <remote>`.
//!
//! See `examples/remote_plan.rs` for configuration, advertisement selection and push-command
//! planning. [`crate::fetch::FetchRequest`] composes fetch refspecs with transport and publication.
//!
//! # Example
//!
//! Map local tips through a remote's configured fetch refspec. Mapping is pure: it reads no
//! storage and contacts no server.
//!
//! ```
//! use girt::Config;
//! use girt::refs::RefName;
//! use girt::remote::{RefSource, Remote};
//!
//! let config = Config::parse(
//!     b"[remote \"origin\"]\n\
//!       url = https://example.invalid/project\n\
//!       fetch = +refs/heads/*:refs/remotes/origin/*\n",
//! )?;
//! let origin = Remote::find(&config, b"origin")?.expect("origin is configured");
//! assert_eq!(
//!     origin.fetch_url(),
//!     Some(&b"https://example.invalid/project"[..])
//! );
//!
//! let id = girt::ObjectFormat::Sha1.hash_object(girt::ObjectKind::Commit, b"example");
//! let sources = [RefSource {
//!     name: RefName::new("refs/heads/main")?,
//!     id,
//! }];
//! let mappings = origin.fetch_refspecs().map(&sources)?;
//! assert_eq!(
//!     mappings[0].destination,
//!     Some(RefName::new("refs/remotes/origin/main")?)
//! );
//! assert!(mappings[0].force);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod config;
mod configured_refspec;
mod configured_remote;
mod configured_url;
mod credential;
mod credential_store;
mod edit;
mod endpoint;
pub use edit::{RemoteConfig, RemoteEditError, RemoteKey};
mod name;
mod refspec;
mod url;
mod user_url;

pub use config::{Remote, RemoteError, RemoteUrls};
pub use configured_refspec::{ConfiguredRefspec, ConfiguredRefspecError, ConfiguredRefspecKind};
pub use configured_remote::{
    ConfiguredRemote, ConfiguredRemoteError, ConfiguredRemoteRecord, rewrite_configured_url,
};
pub use configured_url::{
    ConfiguredUrlError, ConfiguredUrlParts, normalize_configured_url, parse_configured_url,
};
pub use credential::{
    Credential, CredentialBackend, CredentialContext, CredentialError, CredentialHelper,
    CredentialProgram, CredentialSession, Prompt, askpass,
};
pub use credential_store::CredentialStore;
pub use endpoint::{Destination, EndpointError, Protocol, ProtocolEnvironment};
pub use name::{InvalidRemoteName, RemoteNameError, validate_name};
pub use refspec::{
    Direction, Mapping, MappingError, RefSource, Refspec, RefspecError, Refspecs, RefspecsError,
};
pub use url::{ParsedUrl, UrlError};
pub use user_url::{UserUrlError, UserUrlParseError, UserUrlPathError, canonicalize_user_url};
