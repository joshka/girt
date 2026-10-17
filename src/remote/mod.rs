//! Remote configuration, refspecs, URLs and credentials.
//!
//! A *remote* is a named entry in a repository's configuration that says where another repository
//! lives and which references to exchange with it:
//!
//! ```text
//! [remote "origin"]
//!     url = https://example.com/project.git
//!     pushurl = ssh://git@example.com/project.git
//!     fetch = +refs/heads/*:refs/remotes/origin/*
//! ```
//!
//! This module reads and edits that configuration, maps reference names through refspecs,
//! interprets URLs and endpoints, and obtains credentials. It performs no network I/O and never
//! starts a transfer. [`crate::workflow`] composes these pieces into `git fetch`-like operations;
//! the core [`crate::fetch`], [`crate::push`] and [`crate::clone`] APIs take their results as
//! explicit inputs.
//!
//! # Which type to use
//!
//! | Task | Start with |
//! | --- | --- |
//! | Fetch or push by remote name, as Git does | [`crate::workflow`], which uses this module internally |
//! | Read a remote's URLs and refspecs to drive [`crate::fetch::FetchRequest`] | [`Remote`] |
//! | Choose the endpoint to connect to, applying `insteadOf`, `pushurl` and `protocol.allow` | [`Remote::fetch_destination`], [`Remote::push_destinations`] |
//! | List or display remotes as the user configured them | [`ConfiguredRemote`], [`ConfiguredRemoteRecord`], [`Remote::names`] |
//! | Read only a remote's URLs, even if its refspecs use syntax girt cannot map | [`RemoteUrls`] |
//! | Add, rename or remove a remote, or edit its URLs and refspecs | [`RemoteConfig`] |
//! | Map reference names through refspecs | [`Refspecs::map`], [`Refspecs::map_advertisement`] |
//! | Interpret a URL a user typed, or show a URL's host and path | [`canonicalize_user_url`], [`ParsedUrl`], [`parse_configured_url`] |
//! | Look up, save or forget credentials | [`CredentialSession`], [`CredentialStore`] |
//! | Check a name before creating a remote | [`validate_name`] |
//!
//! [`Remote::find`] parses every fetch and push refspec with the strict [`Refspec`] grammar and
//! fails if any is outside it, for example shorthand such as `fetch = main`. Such a remote cannot
//! drive a transfer, but [`RemoteUrls`], [`ConfiguredRemote`] and [`ConfiguredRemoteRecord`] can
//! still read and display it.
//!
//! The configuration readers ([`Remote`], [`RemoteUrls`], [`ConfiguredRemote`] and
//! [`ConfiguredRemoteRecord`]) take an explicit [`Config`](crate::Config) snapshot, usually
//! [`Repository::config`](crate::Repository::config), and read no files or environment variables
//! themselves. They return owned snapshots that do not change if the configuration file is edited
//! later. The other parts of the module differ: [`ParsedUrl`] borrows the bytes it parses,
//! [`canonicalize_user_url`] inspects the filesystem to resolve local paths, and
//! [`CredentialSession`] reads credential files and runs helper programs, blocking while it
//! does.
//!
//! # Refspecs
//!
//! A refspec says which references to transfer and what to call them on the other side. `git
//! fetch`'s default, `+refs/heads/*:refs/remotes/origin/*`, reads "for every branch on the remote,
//! update the local remote-tracking reference with the same suffix, even if it does not
//! fast-forward". The parts are:
//!
//! - `<src>:<dst>`: the source name on the sending side and the destination on the receiving side.
//!   A fetch maps remote names to local names; a push maps local names to remote names.
//! - `*`: one wildcard per side, matching any bytes (including `/`). The destination receives what
//!   the source matched.
//! - A leading `+`: *force intent*, meaning the destination may move to a commit that does not
//!   descend from its current value.
//! - A leading `^` (fetch only): exclude matching sources, as in `^refs/heads/wip/*`.
//! - A fetch source with no destination (`refs/heads/main`) downloads objects without recording a
//!   reference. A push of `:refs/heads/old` deletes the remote reference.
//!
//! girt has two refspec parsers because configurations contain more syntax than can be mapped
//! without further context. [`Refspec`] and [`Refspecs`] accept the subset that can be mapped
//! purely: full `refs/…` names, `HEAD` as a source, wildcards, exclusions and deletions. They are
//! what transfers use. [`ConfiguredRefspec`] accepts everything Git accepts in configuration,
//! including shorthand such as `main`, matching push (`:`), and object IDs, and describes it
//! without resolving it. Use it to show or validate configuration; use [`Refspecs`] to plan
//! transfers.
//!
//! Force intent is not permission. A `+` is recorded as [`ForceIntent::Requested`], but each
//! operation that publishes references decides separately whether to honor it:
//! [`crate::fetch::FetchRequest::prepare`] requires the caller to authorize each forced
//! destination by name, and [`crate::push::PushCommand`] takes an explicit
//! [`ForcePolicy`](crate::push::ForcePolicy). Mapping a refspec never reads objects or references,
//! so it cannot know whether an update would fast-forward.
//!
//! # URLs and endpoints
//!
//! A remote's `url` can be an `https://`, `ssh://` or `file://` URL, an scp-like `user@host:path`,
//! a local path, or a `<helper>::<address>` for a remote helper. Before connecting, Git rewrites it
//! with `url.<base>.insteadOf` (and `pushInsteadOf` for pushes without a `pushurl`) and checks
//! `protocol.allow`. [`Remote::fetch_destination`] and [`Remote::push_destinations`] do the same
//! and return a [`Destination`] whose [`Protocol`] selects the transport. By default local paths
//! and `file://` are allowed unless `GIT_PROTOCOL_FROM_USER=0` marks the operation as automatic,
//! remote helpers are refused, and the network protocols are allowed. A `GIT_ALLOW_PROTOCOL`
//! list, passed in through [`ProtocolEnvironment`], replaces that policy entirely.
//!
//! URLs can contain credentials (`https://user:token@host/…`), and query strings can contain
//! tokens. The byte accessors return URLs unchanged so a transfer can use them. Error messages in
//! this module never include URL bytes, and the `Debug` output of [`Destination`],
//! [`RemoteUrls`], [`ConfiguredRemote`], [`ConfiguredRemoteRecord`], [`ParsedUrl`] and the
//! credential types omits them. [`Remote`]'s `Debug` output does include its URLs, so don't log
//! it. To show a URL, build it from [`ParsedUrl::host`] and [`ParsedUrl::path`], which drop the
//! username and password, and remove any query string from the path yourself.
//!
//! # Credentials
//!
//! Git's credential model has three parts:
//!
//! - A *context* identifies what the credential is for: protocol, host and, if
//!   `credential.useHttpPath` is set, the repository path ([`CredentialContext`]).
//! - *Helpers*, configured with `credential.helper`, are programs that look up, save and erase
//!   credentials, such as `osxkeychain`, `manager` or `store`. Git asks them in order until one
//!   answers ([`CredentialHelper`]).
//! - If no helper answers, the user is prompted. After the server accepts or rejects the
//!   credential, every helper is told, so it can save or forget it.
//!
//! [`CredentialSession::fill`] runs that lookup and returns a session holding the credential;
//! [`CredentialSession::approve`] and [`CredentialSession::reject`] report the outcome.
//!
//! The session never runs a program just because configuration names it. A repository's
//! configuration can come from an untrusted clone, so the application maps each configured helper
//! name to a [`CredentialBackend`] it has approved: a [`CredentialProgram`] with an explicit
//! executable and arguments, or the native [`CredentialStore`]. A configured helper without a
//! mapping fails with [`CredentialError::UnapprovedHelper`] before anything runs.
//!
//! [`crate::workflow`] grants that approval automatically, the way Git does: a `!command` value
//! runs as a shell command, an absolute path runs that program, and any other name runs
//! `git-credential-<name>` from `PATH` or Git's exec path. Most applications therefore never
//! construct helpers themselves, but they must trust the configuration they give `workflow`, or
//! filter its `credential.helper` values first. The refusal above protects only applications that
//! call [`CredentialSession::fill`] with their own mappings.
//!
//! [`CredentialStore`] answers Git's `store` helper without starting a process, reading and
//! writing the same plain-text `~/.git-credentials` files with the same locking. It is useful for
//! tests and for environments where Git is not installed. Its files hold passwords unencrypted.
//!
//! # Errors and recovery
//!
//! - [`EndpointError::Refused`]: `protocol.allow`, `protocol.<name>.allow` or `GIT_ALLOW_PROTOCOL`
//!   forbids the remote's protocol. This is policy, not a malformed URL; show the user which
//!   setting applies rather than retrying.
//! - [`ConfiguredRemoteError::UnsupportedUrlSyntax`]: the URL is valid Git syntax that girt does
//!   not yet interpret. Treat the whole remote as unsupported, rather than as invalid.
//! - [`RemoteError::Refspec`]: a refspec is outside the subset that can drive a transfer; the
//!   remote can still be displayed with [`ConfiguredRemote`].
//! - [`CredentialError`]: a lookup was refused, failed or was cancelled. A successful
//!   [`CredentialSession::fill`] can still return an incomplete credential when no prompt is
//!   supplied, and helpers that exit unsuccessfully are listed in
//!   [`CredentialSession::helper_failures`] while later helpers still run. Approving a credential
//!   only reports that the server accepted it; it says nothing about whether the transfer finished.
//!
//! # Resolving where to connect
//!
//! Find a remote and resolve the endpoint a transfer would use. Resolution applies `insteadOf` and
//! protocol policy but performs no I/O; [`crate::workflow::fetch`] shows the transfer itself.
//!
//! ```
//! use girt::remote::{Protocol, ProtocolEnvironment, Remote, RemoteConfig};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let path = directory.path().join("project");
//! Repository::init(ObjectFormat::Sha1, &path, InitKind::Worktree)?;
//! let mut edit = Repository::open(&path)?.edit_config(1 << 20)?;
//! RemoteConfig::new(edit.document_mut()).add(
//!     b"origin",
//!     b"https://example.com/project.git",
//!     &[b"+refs/heads/*:refs/remotes/origin/*"],
//! )?;
//! edit.commit()?;
//!
//! let repo = Repository::open(&path)?;
//! let origin = Remote::find(repo.config(), b"origin")?.ok_or("no remote named origin")?;
//! let destination = origin.fetch_destination(repo.config(), &ProtocolEnvironment::default())?;
//! match destination.protocol() {
//!     Protocol::Local | Protocol::File => println!("local path {:?}", destination.local_path()?),
//!     Protocol::Https | Protocol::Http => println!("needs the `http` feature"),
//!     Protocol::Ssh => println!("needs the `ssh` feature"),
//!     _ => println!("unsupported transport"),
//! }
//! # assert_eq!(destination.protocol(), Protocol::Https);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Mapping a configured refspec
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
//! assert_eq!(mappings[0].force, girt::remote::ForceIntent::Requested);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # How jj uses this module
//!
//! jj's girt backend uses [`Remote::names`] and [`ConfiguredRemoteRecord`] to list remotes,
//! including ones without URLs, and [`ConfiguredRemote`] to show each remote's effective URLs
//! after `insteadOf` rewriting. It reads [`ConfiguredRefspec::kind`] to translate fetch refspecs
//! into bookmark patterns, refusing kinds it cannot represent. `jj git remote add` and `jj git
//! clone` pass the URL the user typed through [`canonicalize_user_url`], which makes local paths
//! absolute, and `jj git remote add` checks the name with [`validate_name`]. jj builds a browser
//! link for a remote from [`ParsedUrl::host`] and [`ParsedUrl::path`]. Transfers themselves go
//! through [`crate::workflow`].
//!
//! # Not covered
//!
//! Tag following (`remote.<name>.tagOpt` is reported but not acted on), mirror remotes,
//! `remote.<name>.prune`, `push.default` and other options that change what a bare `git fetch` or
//! `git push` would do remain the caller's policy. This module is not a complete interpretation
//! of `git fetch <remote>` or `git push <remote>`.

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
pub use endpoint::{Destination, EndpointError, Protocol, ProtocolEnvironment, ProtocolOrigin};
pub use name::{InvalidRemoteName, RemoteNameError, validate_name};
pub use refspec::{
    Direction, ForceIntent, Mapping, MappingError, RefSource, Refspec, RefspecError, Refspecs,
    RefspecsError,
};
pub use url::{ParsedUrl, UrlError};
pub use user_url::{UserUrlError, UserUrlParseError, UserUrlPathError, canonicalize_user_url};
