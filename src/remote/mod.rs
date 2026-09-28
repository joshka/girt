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
//! - [`RemoteConfig`] edits named remote configuration. [`CredentialSession`] discovers credentials
//!   only through an application-approved helper lifecycle.
//!
//! Transport choice, URL rewriting, implicit branch selection, tag following, pruning, mirror
//! policy and other remote options remain the caller's responsibility.
//! This is not a complete interpretation of `git fetch <remote>` or `git push <remote>`.
//!
//! See `examples/remote_plan.rs` for configuration, advertisement selection and push-command
//! planning. [`crate::fetch::FetchRequest`] composes fetch refspecs with transport and publication.

mod config;
mod configured_refspec;
mod configured_remote;
mod configured_url;
mod credential;
mod edit;
mod endpoint;
pub use edit::{RemoteConfig, RemoteEditError, RemoteKey};
mod name;
mod refspec;
mod url;

pub use config::{Remote, RemoteError, RemoteUrls};
pub use configured_refspec::{ConfiguredRefspec, ConfiguredRefspecError, ConfiguredRefspecKind};
pub use configured_remote::{ConfiguredRemote, ConfiguredRemoteError, ConfiguredRemoteRecord};
pub use configured_url::{ConfiguredUrlError, normalize_configured_url};
pub use credential::{
    Credential, CredentialContext, CredentialError, CredentialHelper, CredentialProgram,
    CredentialSession, Prompt, askpass,
};
pub use endpoint::{Destination, EndpointError, Protocol, ProtocolEnvironment};
pub use name::{InvalidRemoteName, RemoteNameError, validate_name};
pub use refspec::{
    Direction, Mapping, MappingError, RefSource, Refspec, RefspecError, Refspecs, RefspecsError,
};
pub use url::{ParsedUrl, UrlError};
