use std::sync::atomic::AtomicBool;

use super::{ContentMerge, ContentOptions, RenameError, TreeMerge, TreeMergeOptions};
use crate::{ObjectId, Objects};

/// Caller-owned content merging selected by repository-relative byte path.
///
/// Implement this interface to compose attribute lookup with [`super::Driver`], or invoke a
/// custom implementation under caller-owned process and trust policy. The example preserves named
/// drivers as `CustomRequired`; callers matching Git's undefined-driver fallback should check
/// `merge.<name>.driver` in their configuration snapshot and select text when absent. Only paths
/// requiring three-way regular-file comparison invoke it; unchanged-side and structural resolutions
/// do not. Rename alignment supplies destination paths. Commit merging uses the same instance for
/// virtual bases and the final merge. Options remain plain data and hold no callback.
///
/// Library computation installs no objects. Callback side effects, cancellation of any custom
/// process and external state consistency belong to the caller. Returning resolved bytes is an
/// explicit resolution decision; return structured conflicts to preserve unresolved input.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::attributes::{Attributes, Case, Limits, Source};
/// use girt::merge::{ContentMerge, ContentOptions, Driver, TreeDriver};
///
/// struct AttributeDriver(Attributes);
/// impl TreeDriver for AttributeDriver {
///     fn merge(
///         &mut self,
///         path: &[u8],
///         base: &[u8],
///         ours: &[u8],
///         theirs: &[u8],
///         options: ContentOptions,
///         cancel: &AtomicBool,
///     ) -> Result<ContentMerge, Box<dyn std::error::Error + Send + Sync>> {
///         let selected = Driver::from_attribute(self.0.get(path, b"merge")?);
///         Ok(selected.merge(base, ours, theirs, options, cancel)?)
///     }
/// }
/// let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
/// attributes.add(Source::Directory(b""), b"*.list merge=union\n")?;
/// let mut driver = AttributeDriver(attributes);
/// let result = driver.merge(
///     b"names.list",
///     b"old\n",
///     b"ours\n",
///     b"theirs\n",
///     ContentOptions::default(),
///     &AtomicBool::new(false),
/// )?;
/// assert!(result.is_clean());
/// # Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
/// ```
pub trait TreeDriver {
    /// Merges one regular-file path, retaining unresolved original sides when needed.
    ///
    /// Inputs are borrowed only for this call. Output and errors must own their data. Names and
    /// payloads are byte-oriented; options carry the selected whitespace/favoring/limits policy.
    ///
    /// # Errors
    ///
    /// Return the original attribute/process/content failure. Tree merging retains it as the
    /// source of [`super::TreeError::Driver`] with the selected path and returns no partial tree.
    fn merge(
        &mut self,
        path: &[u8],
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        options: ContentOptions,
        cancel: &AtomicBool,
    ) -> Result<ContentMerge, Box<dyn std::error::Error + Send + Sync>>;
}

impl Objects {
    /// Merges renamed trees with explicit per-path content-driver dispatch.
    ///
    /// Uses [`Self::merge_trees_with`]'s path/inference policy. The driver receives destination
    /// paths after alignment and original base/side payloads; callback errors retain path/source.
    /// No implicit attribute lookup or shell command runs. See [`TreeDriver`] for composition.
    ///
    /// # Errors
    ///
    /// Returns [`RenameError`] for inference/tree failures, including path-driver errors.
    pub fn merge_trees_with_driver(
        &self,
        base: ObjectId,
        ours: ObjectId,
        theirs: ObjectId,
        options: TreeMergeOptions,
        cancel: &AtomicBool,
        driver: &mut dyn TreeDriver,
    ) -> Result<TreeMerge, RenameError> {
        super::renames::merge_with_memory_and_driver(
            self,
            [base, ours, theirs],
            options,
            cancel,
            &mut super::memory::Memory::default(),
            Some(driver),
        )
    }
}
