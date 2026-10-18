use std::fmt;
use std::str::FromStr;

/// A supported, explicitly selected maintenance task.
///
/// Selecting a task does not run it, check its automatic threshold, or authorize deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaintenanceTask {
    /// Full garbage collection under a separately supplied retention policy and isolation.
    Gc,
    /// Write commit-graph metadata.
    CommitGraph,
    /// Fetch into `refs/prefetch/` without changing ordinary remote-tracking references.
    Prefetch,
    /// Pack loose objects and remove verified packed duplicates.
    LooseObjects,
    /// Expire redundant packs and publish a bounded incremental repack batch.
    IncrementalRepack,
    /// Pack eligible loose references.
    PackRefs,
}

impl fmt::Display for MaintenanceTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Gc => "gc",
            Self::CommitGraph => "commit-graph",
            Self::Prefetch => "prefetch",
            Self::LooseObjects => "loose-objects",
            Self::IncrementalRepack => "incremental-repack",
            Self::PackRefs => "pack-refs",
        })
    }
}

impl FromStr for MaintenanceTask {
    type Err = TaskSelectionError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "gc" => Ok(Self::Gc),
            "commit-graph" => Ok(Self::CommitGraph),
            "prefetch" => Ok(Self::Prefetch),
            "loose-objects" => Ok(Self::LooseObjects),
            "incremental-repack" => Ok(Self::IncrementalRepack),
            "pack-refs" => Ok(Self::PackRefs),
            _ => Err(TaskSelectionError::Unsupported(name.into())),
        }
    }
}

/// Explicit maintenance tasks in execution order; no work has been authorized or started.
///
/// Like Git, pack-refs is foreground work and precedes the remaining tasks. Other tasks retain
/// their requested order. Execution and automatic threshold checks are separate concerns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskSelection {
    tasks: Vec<MaintenanceTask>,
}

impl TaskSelection {
    /// Selects tasks without consulting repository configuration or changing storage.
    ///
    /// Explicit selection overrides `maintenance.<task>.enabled`. It also does not consult
    /// `maintenance.strategy` or `maintenance.auto`: the latter controls command-triggered runs,
    /// not an explicit `maintenance run --auto`. A runner must still apply each selected task's
    /// threshold when asked to run automatically.
    ///
    /// # Errors
    ///
    /// An empty list refuses Git's implicit default task set, which includes unsupported tasks
    /// in Git 2.56. Repeating a task is an error, matching Git's explicit task selection.
    ///
    /// ```
    /// use girt::maintenance::{MaintenanceTask, TaskSelection};
    /// let selected = TaskSelection::explicit(&[MaintenanceTask::CommitGraph])?;
    /// assert_eq!(selected.tasks(), &[MaintenanceTask::CommitGraph]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn explicit(tasks: &[MaintenanceTask]) -> Result<Self, TaskSelectionError> {
        if tasks.is_empty() {
            return Err(TaskSelectionError::DefaultTasks);
        }
        let mut selected = Vec::with_capacity(6);
        for &task in tasks {
            if selected.contains(&task) {
                return Err(TaskSelectionError::Duplicate(task));
            }
            selected.push(task);
        }
        if let Some(position) = selected
            .iter()
            .position(|task| *task == MaintenanceTask::PackRefs)
        {
            selected.remove(position);
            selected.insert(0, MaintenanceTask::PackRefs);
        }
        Ok(Self { tasks: selected })
    }

    /// Returns the selected tasks in execution order.
    pub fn tasks(&self) -> &[MaintenanceTask] {
        &self.tasks
    }
}

/// Task selection was refused before any repository work.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TaskSelectionError {
    /// Git's implicit default task set is not supported yet; select tasks explicitly.
    #[error("maintenance requires explicit task selection")]
    DefaultTasks,
    /// A task was selected more than once.
    #[error("maintenance task '{0}' was selected more than once")]
    Duplicate(MaintenanceTask),
    /// The requested task is not supported, including Git 2.56's newer default tasks.
    #[error("unsupported maintenance task '{0}'")]
    Unsupported(Box<str>),
}
