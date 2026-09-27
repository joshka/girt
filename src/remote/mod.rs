//! Named remote configuration and pure refspec mapping.
//!
//! # URLs, mappings, and credentials
//!
//! - [`RemoteUrls::find`] reads URL values without parsing refspecs; [`Remote::find`] also loads
//!   fetch and push [`Refspecs`] from an explicit configuration snapshot.
//! - [`Refspecs`] maps supplied sources to destinations without I/O or update authorization.
//!   [`Destination`] identifies a selected local or network endpoint separately.
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
mod credential;
mod edit;
mod endpoint;
pub use edit::{RemoteConfig, RemoteEditError, RemoteKey};
mod refspec;

pub use config::{Remote, RemoteError, RemoteUrls};
pub use credential::{
    Credential, CredentialContext, CredentialError, CredentialHelper, CredentialProgram,
    CredentialSession, Prompt, askpass,
};
pub use endpoint::{Destination, EndpointError, Protocol, ProtocolEnvironment};
pub use refspec::{
    Direction, Mapping, MappingError, RefSource, Refspec, RefspecError, Refspecs, RefspecsError,
};
