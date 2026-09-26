//! Named remote configuration and pure refspec mapping.
//!
//! [`Remote::find`] reads the four URL/refspec keys from an explicit [`crate::Config`] snapshot.
//! [`Refspecs`] maps supplied resolved references without I/O, revision lookup, or update
//! permission. Transport choice, URL rewriting, implicit branch selection, tag
//! following, pruning, mirror policy and other remote options remain the caller's responsibility.
//! This is not a complete interpretation of `git fetch <remote>` or `git push <remote>`.
//!
//! See `examples/remote_plan.rs` for configuration, advertisement selection and push-command
//! planning. [`CredentialSession`] provides separate application-approved credential discovery.
//! [`crate::fetch::FetchRequest`] composes fetch refspecs with transport and publication.

mod config;
mod credential;
mod edit;
mod endpoint;
pub use edit::{RemoteConfig, RemoteEditError, RemoteKey};
mod refspec;

pub use config::{Remote, RemoteError};
pub use credential::{
    Credential, CredentialContext, CredentialError, CredentialHelper, CredentialProgram,
    CredentialSession, Prompt, askpass,
};
pub use endpoint::{Destination, EndpointError, Protocol, ProtocolEnvironment};
pub use refspec::{
    Direction, Mapping, MappingError, RefSource, Refspec, RefspecError, Refspecs, RefspecsError,
};
