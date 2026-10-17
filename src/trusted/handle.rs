use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use super::reader::ObjectView;
use crate::config::ConfigInputs;
use crate::refs::{Dereference, Expected, RefEdit, RefName, Reflog, Target};
use crate::workflow::{self, Environment, FetchOptions, NoCallbacks, PushOptions};
use crate::{Commit, LooseObjects, ObjectId, ObjectKind, Repository, Signature};

/// An owning, default-policy handle for a trusted repository.
///
/// Construct with [`Self::discover`] for Git-default configuration and process environment, or
/// [`Self::new`] for an explicitly configured repository. Shared reads use immutable object
/// snapshots and a synchronized lazy cache. This type is `Send + Sync`; `Log` owns its reader
/// state and can outlive the handle. See the [module contract](super).
#[derive(Debug)]
pub struct Handle {
    pub(super) reader: Arc<ObjectView>,
    loose: LooseObjects,
    environment: Environment,
}

/// A convenience-policy refusal or a switch that failed after checkout.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The supplied byte path contains invalid components.
    #[error("invalid repository-relative path {path:?}")]
    InvalidPath {
        /// Original path bytes.
        path: Vec<u8>,
    },
    /// A component was absent from a tree.
    #[error("path {path:?} is absent from tree {tree}")]
    PathAbsent {
        /// Original path bytes.
        path: Vec<u8>,

        /// Tree missing the component.
        tree: ObjectId,
    },
    /// A path component has the wrong mode for this lookup.
    #[error("path {path:?} requires {wanted:?}, found {found:?}")]
    PathKind {
        /// Original path bytes.
        path: Vec<u8>,

        /// Required kind at this component.
        wanted: ObjectKind,

        /// Actual entry kind.
        found: ObjectKind,
    },
    /// The requested remote has no configuration record.
    #[error("remote {name:?} is not configured")]
    RemoteNotConfigured {
        /// Requested remote name.
        name: String,
    },
    /// A URL-only remote has no fetch mapping; HEAD-only fetching is outside this prototype.
    #[error("remote {name:?} has no fetch refspecs; use workflow::fetch with explicit options")]
    NoFetchRefspecs {
        /// Requested remote name.
        name: String,
    },
    /// Recognized operation metadata requires explicit continuation or abort.
    #[error("finish or abort the current repository operation first")]
    OperationInProgress,
    /// A commit cannot be ordered without a committer date.
    #[error("commit {id} has no committer date")]
    MissingCommitter {
        /// Commit being walked.
        id: ObjectId,
    },
    /// Files and index have changed; HEAD publication failed. Inspect both reports before recovery.
    #[error("checkout completed but HEAD publication failed")]
    SwitchHead {
        /// Completed checkout effects.
        checkout: Box<crate::checkout::Report>,

        /// Reference and reflog publication effects.
        #[source]
        source: Box<crate::refs::TransactionError>,
    },
}

impl Handle {
    /// Owns `repository`, capturing the process environment for identity and transfers.
    ///
    /// No object or reference store is opened. Compose [`Repository::open_with_config`] first to
    /// choose configuration inputs explicitly; use [`Self::with_environment`] for isolated callers.
    pub fn new(repository: Repository) -> Self {
        let loose = repository.loose_objects();
        Self {
            reader: Arc::new(ObjectView::new(repository)),
            loose,
            environment: Environment::from_process(),
        }
    }

    /// Discovers a repository with [`ConfigInputs::git_default`] and
    /// [`Repository::discover_with_config`].
    ///
    /// # Errors
    ///
    /// Reports configuration and discovery errors before any mutation.
    pub fn discover(path: impl AsRef<Path>) -> crate::Result<Self> {
        let environment = Environment::from_process();
        let inputs = ConfigInputs::git_default(&environment)?;
        let repository = Repository::discover_with_config(path, &inputs)?;
        Ok(Self::new(repository).with_environment(environment))
    }

    /// Chooses the environment used by [`Signature`] and [`workflow`] operations.
    /// Repository configuration is unchanged; load it with matching inputs before construction.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }

    /// Borrows the explicit API for operations outside this prototype's policy.
    pub fn repository(&self) -> &Repository {
        &self.reader.repository
    }

    /// Resolves supported revision syntax through [`crate::revision::Resolver::resolve`].
    /// Accepts revision text or an [`ObjectId`]; other inputs are parsed from their [`Display`].
    ///
    /// # Errors
    ///
    /// Returns parse/resolution/storage errors. Unknown revisions and missing objects trigger one
    /// object refresh and retry; corruption and resource refusals do not.
    pub fn rev_parse(&self, revision: impl Display) -> crate::Result<ObjectId> {
        self.reader.resolve(revision, None)
    }

    /// Reads a byte path below a revision's tree using [`crate::revision::Resolver::resolve_to`],
    /// [`crate::Objects::read_tree`] and [`crate::Objects::read_blob`]. Paths are literal
    /// repository-relative components; revision path syntax is not interpolated into them. The
    /// revision accepts text or an [`ObjectId`], as in [`Self::rev_parse`].
    ///
    /// # Errors
    ///
    /// Reports invalid/absent paths, wrong kinds and storage failures. Each missing object gets
    /// one refresh and retry. Gitlinks are not traversed.
    pub fn blob_at(
        &self,
        revision: impl Display,
        path: impl AsRef<[u8]>,
    ) -> crate::Result<Vec<u8>> {
        let path = path.as_ref();
        if path.is_empty()
            || path
                .split(|b| *b == b'/')
                .any(|p| p.is_empty() || p == b"." || p == b".." || p.contains(&0))
        {
            return Err(Error::InvalidPath {
                path: path.to_vec(),
            }
            .into());
        }
        let mut id = self.reader.resolve(revision, Some(ObjectKind::Tree))?;
        let mut components = path.split(|b| *b == b'/').peekable();
        while let Some(component) = components.next() {
            let tree = self.reader.read_tree(id)?;
            let entry = tree
                .entries()
                .iter()
                .find(|e| e.name == component)
                .ok_or_else(|| Error::PathAbsent {
                    path: path.to_vec(),
                    tree: id,
                })?;
            let wanted = if components.peek().is_some() {
                ObjectKind::Tree
            } else {
                ObjectKind::Blob
            };
            if entry.mode.object_kind() != wanted {
                return Err(Error::PathKind {
                    path: path.to_vec(),
                    wanted,
                    found: entry.mode.object_kind(),
                }
                .into());
            }
            id = entry.id;
        }
        self.reader.read_blob(id)
    }

    /// Compares HEAD, index and raw working files through [`Repository::raw_status`].
    /// Untracked files are included without ignore processing.
    ///
    /// # Errors
    ///
    /// Returns raw-status platform, storage and resource failures. No mutation occurs.
    pub fn status(&self) -> Result<crate::status::Report, crate::status::Error> {
        self.reader.repository.raw_status(
            crate::status::Baseline::Head,
            crate::status::Untracked::RawFilesWithoutIgnores,
        )
    }

    /// Starts an empty snapshot through [`crate::tree::TreeEditor::new`].
    /// Supply stored subtrees explicitly with `set_entry`; this does not load HEAD or stage files.
    pub fn tree_editor(&self) -> crate::tree::TreeEditor<'_> {
        crate::tree::TreeEditor::new(&self.loose)
    }

    /// Writes a commit with configured [`Signature::author`] and [`Signature::committer`], then
    /// advances resolved HEAD conditionally through [`crate::refs::References::transaction`].
    /// Uses [`Commit::builder`] and [`LooseObjects::write_commit`]. The current HEAD is the sole
    /// parent; unborn HEAD creates a root. Appends a Git-style commit subject to HEAD and each
    /// symbolic alias/branch in its chain (only HEAD when detached), even if
    /// `core.logAllRefUpdates` is disabled. This deliberate policy always keeps a reflog for
    /// convenience commits. The index and working files remain unchanged; this does not stage
    /// paths. Recognized in-progress operation metadata is refused through
    /// [`Repository::operation_state`]; merge/rebase commits need explicit orchestration.
    /// Before switching, the index/files must represent the new HEAD tree: commit an already
    /// staged index, or materialize the supplied snapshot separately.
    ///
    /// # Errors
    ///
    /// Rejects missing/wrong-kind trees, invalid identities and concurrent HEAD changes. A commit
    /// object may remain after publication failure. Reference errors retain partial effects;
    /// inspect them before retrying. Caller exclusion must cover HEAD observation through
    /// publication.
    pub fn commit(&mut self, tree: ObjectId, message: impl AsRef<[u8]>) -> crate::Result<ObjectId> {
        self.require_idle()?;
        self.reader.read_tree(tree)?;
        let author = Signature::author(self.reader.repository.config(), &self.environment)?;
        let committer = Signature::committer(self.reader.repository.config(), &self.environment)?;
        let refs = self.reader.repository.references()?;
        let parent = refs.resolve(&RefName::HEAD)?.id;
        let expected = parent.map_or(Expected::Absent, |id| Expected::Value(Target::Direct(id)));
        let commit = Commit::builder(tree, author, message.as_ref())
            .committer(committer.clone())
            .parents(parent)
            .build()?;
        let prefix = if parent.is_some() {
            b"commit: ".as_slice()
        } else {
            b"commit (initial): ".as_slice()
        };
        let message = [prefix, &commit.subject()].concat();
        let id = self.loose.write_commit(&commit)?;
        let edit = RefEdit::new(
            RefName::HEAD,
            Some(Target::Direct(id)),
            expected,
            Reflog::Append { committer, message },
        );
        let edit = edit.with_dereference(Dereference::Follow);
        refs.transaction(&[edit])?;
        Ok(id)
    }

    /// Checks out an existing local branch through [`Repository::checkout_tree`] and changes
    /// stored HEAD through [`crate::refs::References::transaction`], appending a Git-style checkout
    /// reflog on HEAD even when `core.logAllRefUpdates` is disabled.
    /// Holds prepared HEAD/reference locks across checkout. Caller must exclude all other working
    /// tree and reference writers, including activity in linked worktrees; branch occupancy is not
    /// checked. [`Repository::operation_state`] refuses recognized in-progress operations. Raw
    /// checkout requires Linux/macOS and refuses dirty or obstructed paths. The index and files
    /// must already match HEAD: a preceding `commit(tree, message)` does not stage or check out
    /// that tree.
    ///
    /// # Errors
    ///
    /// Preparation and checkout failures preserve HEAD. Checkout failures carry partial file/index
    /// effects. [`Error::SwitchHead`] retains a completed checkout when HEAD publication fails;
    /// inspect the new index/files and reference effects before recovery. No rollback is attempted.
    pub fn switch(&mut self, branch: &str) -> crate::Result<crate::checkout::Report> {
        self.switch_with(branch, || {}, || {})
    }

    fn switch_with(
        &mut self,
        branch: &str,
        after_prepare: impl FnOnce(),
        after_checkout: impl FnOnce(),
    ) -> crate::Result<crate::checkout::Report> {
        self.require_idle()?;
        let branch_name = branch;
        let branch = RefName::branch(branch_name)?;
        let repository = &self.reader.repository;
        let refs = repository.references()?;
        let stored = refs.read(&RefName::HEAD)?;
        let from = match &stored {
            Some(Target::Symbolic(name)) => name
                .as_bytes()
                .strip_prefix(b"refs/heads/")
                .unwrap_or(name.as_bytes())
                .to_vec(),
            Some(Target::Direct(id)) => id.to_string().into_bytes(),
            None => Vec::new(),
        };
        let message = [
            b"checkout: moving from ".as_slice(),
            &from,
            b" to ",
            branch_name.as_bytes(),
        ]
        .concat();
        let committer = Signature::committer(repository.config(), &self.environment)?;
        let expected = stored.map_or(Expected::Absent, Expected::Value);
        let edit = RefEdit::new(
            RefName::HEAD,
            Some(Target::Symbolic(branch)),
            expected,
            Reflog::Append { committer, message },
        )
        .with_dereference(Dereference::Stored);
        let prepared = refs.prepare_transaction(&[edit])?;
        after_prepare();
        // Old and new symbolic chains are now locked. Read their tips under those same locks.
        let old = refs.resolve(&RefName::HEAD)?.id;
        let baseline = old
            .map(|id| self.reader.read_commit(id).map(|commit| commit.tree()))
            .transpose()?;
        let revision = format!("refs/heads/{branch_name}");
        let target = self.reader.resolve(&revision, Some(ObjectKind::Commit))?;
        let target_tree = self.reader.read_commit(target)?.tree();
        let checkout = self
            .reader
            .repository
            .checkout_tree(baseline, Some(target_tree))?;
        after_checkout();
        if let Err(source) = prepared.publish() {
            return Err(Error::SwitchHead {
                checkout: Box::new(checkout),
                source: Box::new(source),
            }
            .into());
        }
        Ok(checkout)
    }

    fn require_idle(&self) -> crate::Result<()> {
        if !self
            .reader
            .repository
            .operation_state(Default::default())?
            .is_empty()
        {
            return Err(Error::OperationInProgress.into());
        }
        Ok(())
    }

    /// Fetches a configured remote using its fetch refspecs and [`workflow::fetch`].
    /// Uses the captured environment and [`NoCallbacks`]; use `workflow` for prompts or progress.
    /// Requires at least one configured fetch refspec. `remote.<name>.tagopt`, remote/fetch prune
    /// settings, automatic tag following and depth configuration are not interpreted; use explicit
    /// [`FetchOptions`] through `workflow` to choose those policies.
    ///
    /// # Errors
    ///
    /// Returns configured-remote and transfer errors, preserving partial installation/publication
    /// reports. Shallow boundaries in this handle remain unchanged; reopen after shallow changes.
    pub fn fetch(&self, remote: &str) -> crate::Result<workflow::FetchOutcome> {
        let configured = crate::remote::ConfiguredRemote::find(
            self.reader.repository.config(),
            remote.as_bytes(),
        )?
        .ok_or_else(|| Error::RemoteNotConfigured {
            name: remote.to_owned(),
        })?;
        if configured.fetch_refspecs().is_empty() {
            return Err(Error::NoFetchRefspecs {
                name: remote.to_owned(),
            }
            .into());
        }
        let options = FetchOptions::new(configured.fetch_refspecs().iter().map(|r| r.to_bytes()));
        Ok(workflow::fetch(
            &self.reader.repository,
            remote,
            &options,
            &self.environment,
            &mut NoCallbacks,
        )?)
    }

    /// Pushes explicit leased commands by remote name through [`workflow::push`].
    /// Destination/lease selection stays with the caller; push.default is not interpreted.
    /// Check `all_succeeded()` on the returned report. Uses [`PushOptions`] with a configured
    /// [`Signature::committer`] for reflogs and no prompts.
    ///
    /// # Errors
    ///
    /// Preserves workflow partial/uncertain effects. Read remote state before retrying uncertainty.
    pub fn push(
        &self,
        remote: &str,
        commands: Vec<crate::push::PushCommand>,
    ) -> crate::Result<crate::push::PushReport> {
        let identity = Signature::committer(self.reader.repository.config(), &self.environment)?;
        let options = PushOptions::default().with_identity(identity);
        Ok(workflow::push(
            &self.reader.repository,
            remote,
            commands,
            &options,
            &self.environment,
            &mut NoCallbacks,
        )?)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::{InitKind, ObjectFormat};

    #[test]
    fn switch_locks_target_before_reading_and_retains_checkout_on_publication_failure() {
        let dir = tempfile::tempdir().unwrap();
        let repository =
            Repository::init(ObjectFormat::Sha1, dir.path(), InitKind::Worktree).unwrap();
        let environment = Environment::default()
            .with("GIT_AUTHOR_NAME", "A")
            .with("GIT_AUTHOR_EMAIL", "a@example.com")
            .with("GIT_COMMITTER_NAME", "A")
            .with("GIT_COMMITTER_EMAIL", "a@example.com");
        let mut handle = Handle::new(repository).with_environment(environment);
        let tree = handle.tree_editor().write().unwrap();
        let root = handle.commit(tree, "root").unwrap();
        handle
            .repository()
            .references()
            .unwrap()
            .transaction(&[RefEdit::create(
                RefName::branch("topic").unwrap(),
                root,
                Reflog::Preserve,
            )])
            .unwrap();
        let git_dir = handle.repository().git_dir().to_path_buf();

        let error = handle
            .switch_with(
                "topic",
                || {
                    assert!(git_dir.join("HEAD.lock").exists());
                    assert!(git_dir.join("refs/heads/topic.lock").exists());
                },
                || std::fs::remove_file(git_dir.join("HEAD.lock")).unwrap(),
            )
            .unwrap_err();

        let Error::SwitchHead {
            checkout,
            source: _,
        } = error.downcast::<Error>().unwrap()
        else {
            panic!("expected a post-checkout publication failure");
        };
        assert!(checkout.index_published);
        assert!(handle.repository().git_dir().join("index").exists());
        assert_eq!(
            handle.repository().head().unwrap().name,
            RefName::branch("main").unwrap()
        );
    }
}
