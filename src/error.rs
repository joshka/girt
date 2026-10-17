use std::error::Error as StdError;
use std::fmt;

/// Any girt failure, for applications that propagate errors with `?` across modules.
///
/// Every public girt error type converts into this one, so a function that opens a repository,
/// resolves a revision and reads an object can return [`girt::Result`](crate::Result) instead of
/// boxing or mapping each failure. The conversion boxes the domain error unchanged: display shows
/// that error's message and [`source`](StdError::source) continues with its causes, so a reporter
/// that walks the chain prints each layer once, exactly as it would for the domain error.
///
/// This wraps the explicit, typed error each operation returns. Decide recovery from that typed
/// error where the call is made, or recover it later with [`downcast_ref`](Self::downcast_ref) or
/// [`downcast`](Self::downcast). Errors that carry partial effects, such as
/// [`fetch::FetchFinishError`](crate::fetch::FetchFinishError), keep those reports inside the box.
///
/// ```no_run
/// use girt::revision::Resolver;
/// use girt::{OpenError, Repository};
///
/// fn head(path: &str) -> girt::Result<girt::ObjectId> {
///     let repo = Repository::open(path)?;
///     let objects = repo.objects()?;
///     Ok(Resolver::new(&repo, &objects)?.resolve(&"HEAD".parse()?)?)
/// }
///
/// match head("repo") {
///     Ok(id) => println!("{id}"),
///     Err(error) if error.is::<OpenError>() => eprintln!("not a repository: {error}"),
///     Err(error) => eprintln!("{error}"),
/// }
/// ```
pub struct Error(Box<dyn StdError + Send + Sync + 'static>);

/// A [`std::result::Result`] whose error defaults to the crate-level [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Whether the boxed domain error has type `E`.
    #[must_use]
    pub fn is<E: StdError + 'static>(&self) -> bool {
        self.0.is::<E>()
    }

    /// Borrows the boxed domain error as `E`, or `None` when it has another type.
    #[must_use]
    pub fn downcast_ref<E: StdError + 'static>(&self) -> Option<&E> {
        self.0.downcast_ref()
    }

    /// Takes the boxed domain error as `E`, returning `self` unchanged when it has another type.
    ///
    /// # Errors
    ///
    /// Returns the original error when the domain error is not an `E`.
    pub fn downcast<E: StdError + 'static>(self) -> Result<E, Self> {
        self.0.downcast().map(|error| *error).map_err(Self)
    }

    /// Borrows the boxed domain error for inspection without naming its type.
    #[must_use]
    pub fn get_ref(&self) -> &(dyn StdError + Send + Sync + 'static) {
        &*self.0
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

// Transparent: this wrapper adds no context, so the chain continues with the domain error's causes.
impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.source()
    }
}

macro_rules! from_domain_errors {
    ($($(#[$attr:meta])* $error:ty),+ $(,)?) => {
        $(
            $(#[$attr])*
            impl From<$error> for Error {
                fn from(error: $error) -> Self {
                    Self(Box::new(error))
                }
            }
        )+
    };
}

impl<E: StdError + Send + Sync + 'static> From<crate::commit::SignError<E>> for Error {
    fn from(error: crate::commit::SignError<E>) -> Self {
        Self(Box::new(error))
    }
}

// Keep this list in step with the public error types; `lists_every_public_error_type` checks it.
from_domain_errors! {
    std::io::Error,
    crate::checkout::Error,
    crate::checkout::Failure,
    crate::clone::CloneError,
    crate::clone::CloneFailure,
    crate::clone::ClonePlanError,
    crate::clone::CloneTransferError,
    crate::CommitError,
    crate::config::ConfigError,
    crate::config::EditError,
    crate::config::InvalidValue,
    crate::config::ResolveError,
    crate::config::ResolveFailure,
    crate::content_diff::ContentReadError,
    crate::content_diff::DiffError,
    crate::fetch::FetchError,
    crate::fetch::FetchFinishError,
    crate::fetch::FetchFinishFailure,
    crate::fetch::FetchPlanError,
    crate::fetch::FetchShallowError,
    crate::fetch::FetchUpdateError,
    crate::fetch::FetchWorkflowError,
    crate::fetch::RetainedFetchError,
    crate::fetch::RetainedFetchFinishError,
    crate::HistoryError,
    crate::ignore::Error,
    crate::ignore::LoadError,
    crate::index::Error,
    crate::index::SparseError,
    crate::index::StorageError,
    crate::index::WriteTreeError,
    crate::tree::TreeWriteError,
    crate::tree_diff::Error,
    crate::locks::CleanupFailure,
    crate::loose::Error,
    crate::ObjectFormatError,
    crate::ParseObjectIdError,
    crate::ParseObjectIdPrefixError,
    crate::ObjectReadError,
    crate::ReadBlobError,
    crate::ReadCommitError,
    crate::ReadTagError,
    crate::ReadTreeError,
    crate::ResolvePrefixError,
    crate::pack::IndexRebuildError,
    crate::PackWriteError,
    crate::PeelError,
    crate::PeelFailure,
    crate::push::PushError,
    crate::push::PushFailure,
    crate::refs::InvalidRefName,
    crate::refs::PackRefsError,
    crate::refs::ReferenceError,
    crate::refs::ReflogInterpretationError,
    crate::refs::TransactionError,
    crate::refs::reftable::Error,
    crate::remote::ConfiguredRefspecError,
    crate::remote::ConfiguredRemoteError,
    crate::remote::ConfiguredUrlError,
    crate::remote::CredentialError,
    crate::remote::EndpointError,
    crate::remote::InvalidRemoteName,
    crate::remote::MappingError,
    crate::remote::RefspecError,
    crate::remote::RefspecsError,
    crate::remote::RemoteEditError,
    crate::remote::RemoteError,
    crate::remote::RemoteNameError,
    crate::remote::UrlError,
    crate::remote::UserUrlError,
    crate::remote::UserUrlParseError,
    crate::remote::UserUrlPathError,
    crate::ColocationError,
    crate::CreateWorktreeError,
    crate::InitError,
    crate::OpenError,
    crate::OperationCleanupError,
    crate::OperationError,
    crate::ReflogPolicyError,
    crate::ShallowError,
    crate::WorktreeAdminError,
    crate::WorktreeError,
    crate::retention::ExpireCause,
    crate::retention::ExpireFailure,
    crate::retention::MaintenanceCause,
    crate::retention::MaintenanceFailure,
    crate::retention::PruneCause,
    crate::retention::PruneFailure,
    crate::retention::ReflogExpiryError,
    crate::retention::RepackError,
    crate::retention::RetentionError,
    crate::retention::RetireCause,
    crate::retention::RetireFailure,
    crate::revision::ParseRevisionError,
    crate::revision::RevisionError,
    crate::rewrites::Error,
    crate::status::Error,
    crate::TagError,
    #[cfg(feature = "http")]
    crate::transport::http::HttpError,
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    crate::transport::ssh::SshError,
    crate::TreeError,
    crate::TreeCompareError,
    crate::workflow::TransferError,
    crate::workflow::CloneError,
    crate::trusted::Error,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::refs::RefName;

    /// Open errors keep their display and causes, and remain recoverable by type.
    #[test]
    fn preserves_display_source_chain_and_type() {
        let directory = tempfile::tempdir().unwrap();
        let open = crate::Repository::open(directory.path().join("missing")).unwrap_err();
        let display = open.to_string();
        let cause = open.source().map(ToString::to_string);

        let error = Error::from(open);

        assert_eq!(error.to_string(), display);
        assert_eq!(error.source().map(ToString::to_string), cause);
        assert!(error.is::<crate::OpenError>());
        assert!(error.downcast_ref::<crate::TreeError>().is_none());
        let error = error.downcast::<crate::TreeError>().unwrap_err();
        assert!(error.downcast::<crate::OpenError>().is_ok());
    }

    /// `?` converts unrelated domain errors in one function.
    #[test]
    fn question_mark_crosses_modules() {
        fn parse(name: &str) -> Result<RefName> {
            let _ = "HEAD".parse::<crate::revision::Revision>()?;
            Ok(RefName::new(name)?)
        }

        let error = parse("refs/heads/a..b").unwrap_err();

        assert!(error.is::<crate::refs::InvalidRefName>());
    }

    /// Every public error type a domain module derives is listed in `from_domain_errors!`.
    #[test]
    fn lists_every_public_error_type() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let list = std::fs::read_to_string(root.join("error.rs")).unwrap();
        let list = list.split("from_domain_errors! {").nth(1).unwrap();
        let mut listed: Vec<String> = list
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .map(|line| line.trim().trim_end_matches(',').to_owned())
            .collect();
        // Generic error conversions are implemented separately from the monomorphic macro.
        listed.push("crate::commit::SignError".to_owned());
        let mut missing = Vec::new();
        for (path, name) in public_error_types(&root) {
            // Generic names repeat across modules, so each listing matches one top-level module.
            let module = path
                .strip_prefix(&root)
                .unwrap()
                .components()
                .next()
                .unwrap();
            let module = module.as_os_str().to_str().unwrap().trim_end_matches(".rs");
            let position = listed.iter().position(|listed| match name.as_str() {
                "Error" | "Failure" => {
                    listed.starts_with(&format!("crate::{module}::"))
                        && listed.ends_with(&format!("::{name}"))
                }
                _ => listed.ends_with(&format!("::{name}")),
            });
            match position {
                Some(position) => drop(listed.swap_remove(position)),
                None => missing.push(format!("{}: {name}", path.display())),
            }
        }
        assert_eq!(missing, Vec::<String>::new());
    }

    /// Finds `pub` types deriving `thiserror::Error` under `directory`.
    fn public_error_types(directory: &Path) -> Vec<(std::path::PathBuf, String)> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(public_error_types(&path));
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            found.extend(scan(&text).into_iter().map(|name| (path.clone(), name)));
        }
        found
    }

    /// Names the `pub` items whose derive list, on one line or several, includes `Error`.
    fn scan(text: &str) -> Vec<String> {
        let mut names = Vec::new();
        for derive in text.split("#[derive(").skip(1) {
            let Some((list, rest)) = derive.split_once(")]") else {
                continue;
            };
            let derives_error = list
                .split(',')
                .any(|name| matches!(name.trim(), "Error" | "thiserror::Error"));
            // The derived item is the first enum or struct declaration after its attributes.
            let item = rest.lines().map(str::trim).find(|line| {
                let line = line.strip_prefix("pub ").unwrap_or(line);
                line.starts_with("enum ") || line.starts_with("struct ") || line.starts_with("pub(")
            });
            let public = item.and_then(|line| {
                ["pub enum ", "pub struct "]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
            });
            if let (true, Some(rest)) = (derives_error, public) {
                let end = rest
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                names.push(rest[..end].to_owned());
            }
        }
        names
    }

    /// The scan sees multiline derives and skips private and non-error items.
    #[test]
    fn scan_finds_multiline_and_single_line_derives() {
        let text = "#[derive(\n    Debug,\n    thiserror::Error,\n)]\n#[non_exhaustive]\n\
                    pub enum Multi {}\n#[derive(Debug, Error)]\n/// Doc.\npub struct Single;\n\
                    #[derive(Debug, thiserror::Error)]\npub(crate) enum Private {}\n\
                    #[derive(Debug)]\npub struct Plain;\n";

        assert_eq!(scan(text), ["Multi", "Single"]);
    }
}
