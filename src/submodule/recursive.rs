use super::{PopulateReport, UpdateOptions};
use crate::{Repository, workflow};

/// Child checkout policy and aggregate bounds for explicit recursive population.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RecursionOptions {
    /// Explicit identity, captured environment, filter permission and per-child checkout bounds.
    pub update: UpdateOptions,
    /// Maximum child nesting levels below the supplied parent; always capped at 64.
    pub max_depth: usize,
    /// Maximum selected children across the whole traversal, including none-method entries.
    pub max_submodules: usize,
}
impl RecursionOptions {
    /// Selects explicit child checkout policy, eight nesting levels and 1024 total children.
    pub fn new(update: UpdateOptions) -> Self {
        Self {
            update,
            max_depth: 8,
            max_submodules: 1024,
        }
    }
}

/// One selected child, in depth-first index order, with completed mutation phases.
#[derive(Debug)]
#[non_exhaustive]
pub struct RecursiveEntry {
    /// Gitlink path relative to the traversal's root worktree, using `/` separators.
    pub path: Vec<u8>,
    /// This operation published initialization config in the child's parent.
    pub initialized: bool,
    /// Completed population; a failed population retains its own report in the error instead.
    pub population: Option<PopulateReport>,
}

/// Completed or attempted selected children. No mutation is rolled back on later failure.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RecursiveReport {
    /// Depth-first observations and completed effects, including an incomplete final entry.
    pub entries: Vec<RecursiveEntry>,
}

/// Recursive population failure with all completed children and the failing root-relative path.
#[derive(Debug, thiserror::Error)]
#[error(
    "recursive submodule population failed at {:?}: {source}",
    String::from_utf8_lossy(path)
)]
pub struct RecursiveError {
    /// Completed/attempted children; inspect nested failures for additional partial effects.
    pub report: Box<RecursiveReport>,
    /// Root-relative path being inspected or mutated; empty identifies the initial parent.
    pub path: Vec<u8>,
    /// Refusal or failed phase.
    #[source]
    pub source: RecursiveFailure,
}

/// Recursive traversal refusal or failed child operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecursiveFailure {
    /// Worktree population is supported on Linux/macOS only.
    #[error("recursive submodule population requires Linux/macOS")]
    UnsupportedPlatform,
    /// The parent is bare or has no worktree/index for selecting child gitlinks.
    #[error("recursive submodule population requires a checked-out parent")]
    NoWorktree,
    /// A selected gitlink is unmerged; resolve the parent's index explicitly.
    #[error("cannot populate an unmerged gitlink")]
    Unmerged,
    /// A gitlink has no unique metadata definition.
    #[error("gitlink has no unique submodule definition")]
    Definition,
    /// Depth or total-child bound is exhausted; completed children remain in the report.
    #[error("recursive submodule {0} limit exceeded")]
    Limit(&'static str),
    /// Parent index changed while composing child operations.
    #[error("parent index changed during recursive population")]
    Changed,
    /// Configuration/metadata/index preparation failed, retaining its typed cause.
    #[error(transparent)]
    Prepare(Box<crate::Error>),
    /// Child population failed; inspect both its completed and nested partial phases.
    #[error(transparent)]
    Populate(Box<super::PopulateError>),
}

/// Initializes and populates selected index gitlinks with bounded recursive descent.
///
/// `superproject_url` is the caller-selected remote URL used to resolve relative child URLs.
/// Each child's initialized URL becomes the base for its nested definitions. The worktree/index
/// metadata source follows Git init; symlinked metadata is refused. Explicit `active` booleans and
/// activation pathspecs select children; otherwise all gitlinks are selected. None-method children
/// are initialized but never checked out or descended into. Custom update commands are refused.
/// Unlike Git's CLI recursive clone, the library never appends `submodule.active=.`; explicit
/// caller activation policy is honored without a hidden config write.
///
/// This explicitly authorizes transport. Children use `options.update`'s captured environment and
/// filter permission independently from any outer clone options, inherit only the parent's selected
/// non-local config, and add their own local sources. It composes init,
/// [`super::UpdatePlan::populate`] and their partial-effect/resource contracts. Depth is capped at
/// 64; the total-child bound applies across all levels. Per-child checkout budgets are not
/// aggregate memory/byte budgets. Workflow fetch currently lacks caller-controlled
/// bounds/cancellation; no cancellation is promised here.
///
/// The caller must exclude all hierarchy index/config/metadata/worktree writers and ancestor
/// replacement throughout. Config is refreshed after init, and parent indexes are checked again.
/// Other observations remain non-atomic. No rollback, external update command or hook is run.
///
/// # Errors
///
/// Returns typed unsupported-platform, metadata/selection, conflict and limit refusals. Completed
/// initialization/population effects survive failure and remain in [`RecursiveError::report`];
/// the source can retain more partial effects for the failing child. Inspect state under exclusive
/// ownership before recovery. A bare or no-checkout clone cannot use this traversal.
///
/// ```no_run
/// use girt::{Repository, Signature, submodule, workflow};
/// # fn populate(parent: &Repository, person: Signature) -> Result<(), Box<dyn std::error::Error>> {
/// let update = submodule::UpdateOptions::new(person)
///     .with_environment(workflow::Environment::from_process());
/// let mut policy = submodule::RecursionOptions::new(update);
/// policy.max_depth = 4;
/// let report = submodule::populate_recursive(
///     parent, b"https://example.com/team/project.git", &policy, &mut workflow::NoCallbacks,
/// )?;
/// println!("{} selected children", report.entries.len());
/// # Ok(())
/// # }
/// ```
pub fn populate_recursive(
    parent: &Repository,
    superproject_url: &[u8],
    options: &RecursionOptions,
    callbacks: &mut dyn workflow::TransferCallbacks,
) -> Result<RecursiveReport, RecursiveError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut walker = Walker {
            options,
            callbacks,
            report: RecursiveReport::default(),
            path: Vec::new(),
        };
        match walker.visit(parent, superproject_url, &[], 0) {
            Ok(()) => Ok(walker.report),
            Err(source) => Err(RecursiveError {
                report: Box::new(walker.report),
                path: walker.path,
                source,
            }),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (parent, superproject_url, options, callbacks);
        Err(RecursiveError {
            report: Box::default(),
            path: Vec::new(),
            source: RecursiveFailure::UnsupportedPlatform,
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Walker<'a> {
    options: &'a RecursionOptions,
    callbacks: &'a mut dyn workflow::TransferCallbacks,
    report: RecursiveReport,
    path: Vec<u8>,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Walker<'_> {
    fn visit(
        &mut self,
        parent: &Repository,
        url: &[u8],
        prefix: &[u8],
        depth: usize,
    ) -> Result<(), RecursiveFailure> {
        use super::{Commands, Modules, Source, Update, UpdateMethod};
        self.path = prefix.to_vec();
        if parent.worktree().is_none() {
            return Err(RecursiveFailure::NoWorktree);
        }
        let mut parent = parent.reopen().map_err(prepare)?;
        let limits = self.options.update.limits.index;
        let index = parent.read_index(limits).map_err(prepare)?;
        let Some(index) = index else {
            return Err(RecursiveFailure::NoWorktree);
        };
        let links: Vec<_> = index
            .entries()
            .iter()
            .filter(|e| e.mode == crate::index::Mode::Gitlink)
            .collect();
        if links.is_empty() {
            return Ok(());
        }
        let modules = Modules::read(
            &parent,
            &parent.objects().map_err(prepare)?,
            Source::WorktreeOrIndex,
        )
        .map_err(prepare)?
        .ok_or(RecursiveFailure::Definition)?;
        for link in links {
            self.path = joined(prefix, &link.path);
            let mut matching = modules
                .definitions()
                .iter()
                .filter(|d| d.path() == Some(link.path.as_slice()));
            let definition = matching.next().ok_or(RecursiveFailure::Definition)?;
            if matching.next().is_some() {
                return Err(RecursiveFailure::Definition);
            }
            if !selected(parent.config(), definition)? {
                continue;
            }
            if link.stage != crate::index::Stage::Normal {
                return Err(RecursiveFailure::Unmerged);
            }
            if depth >= self.options.max_depth.min(64) {
                return Err(RecursiveFailure::Limit("depth"));
            }
            if self.report.entries.len() >= self.options.max_submodules {
                return Err(RecursiveFailure::Limit("count"));
            }
            let entry = self.report.entries.len();
            self.report.entries.push(RecursiveEntry {
                path: self.path.clone(),
                initialized: false,
                population: None,
            });
            let initialized = definition
                .plan_init(parent.config(), url)
                .map_err(prepare)?
                .apply(&parent)
                .map_err(prepare)?;
            self.report.entries[entry].initialized = initialized;
            parent = parent.reopen().map_err(prepare)?;
            let plan = definition
                .plan_update(parent.config(), link.id, Commands::Refuse)
                .map_err(prepare)?;
            let skip = matches!(plan.method(), UpdateMethod::BuiltIn(Update::None));
            let report = plan
                .populate(&parent, &self.options.update, self.callbacks)
                .map_err(|error| RecursiveFailure::Populate(Box::new(error)))?;
            self.report.entries[entry].population = Some(report);
            if !skip {
                let path = super::status::checkout_path(&parent, plan.path())
                    .map_err(prepare)?
                    .ok_or(RecursiveFailure::NoWorktree)?;
                let child = Repository::open_with_config(path, parent.inherited_config())
                    .map_err(prepare)?;
                let next = self.path.clone();
                self.visit(&child, plan.url(), &next, depth + 1)?;
            }
            self.path = prefix.to_vec();
            if !parent
                .index_unchanged(Some(&index), limits)
                .map_err(prepare)?
            {
                return Err(RecursiveFailure::Changed);
            }
        }
        self.path = prefix.to_vec();
        if !parent
            .index_unchanged(Some(&index), limits)
            .map_err(prepare)?
        {
            return Err(RecursiveFailure::Changed);
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn joined(prefix: &[u8], path: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return path.to_vec();
    }
    [prefix, b"/", path].concat()
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prepare(error: impl Into<crate::Error>) -> RecursiveFailure {
    RecursiveFailure::Prepare(Box::new(error.into()))
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn selected(
    config: &crate::Config,
    definition: &super::Definition,
) -> Result<bool, RecursiveFailure> {
    if let Some(active) = config
        .boolean("submodule", Some(definition.name()), "active")
        .map_err(prepare)?
    {
        return Ok(active);
    }
    if config.values("submodule", None, "active").next().is_none() {
        return Ok(true);
    }
    super::plan::active(
        config,
        definition.name(),
        definition.path().ok_or(RecursiveFailure::Definition)?,
    )
    .map_err(prepare)
}
