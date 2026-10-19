use super::MaintenanceTask;
use crate::Config;
use crate::config::InvalidValue;

/// A supported configured maintenance strategy, without selecting or scheduling tasks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MaintenanceStrategy {
    /// Git's incremental strategy. Implicit task selection remains unsupported.
    Incremental,
}

/// A numeric automatic-task threshold, without observing the repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AutoThreshold {
    /// Zero disables the automatic task.
    Disabled,
    /// A negative configured value requests the task regardless of its count.
    Always,
    /// Run when the task-specific count reaches this positive threshold.
    Count(u32),
}

/// A consulted per-task maintenance setting is invalid.
///
/// The task supplies the subsection context missing from the underlying generic config error.
#[derive(Debug, thiserror::Error)]
#[error("invalid maintenance configuration for task '{task}'")]
#[non_exhaustive]
pub struct TaskConfigError {
    /// Task whose setting was consulted.
    pub task: MaintenanceTask,
    /// Invalid scalar value, preserving its source diagnostic.
    #[source]
    pub source: InvalidValue,
}

/// Lazy maintenance policy over an already resolved configuration snapshot.
///
/// Each accessor validates only its consulted settings. Explicit task selection need not read
/// enabled flags, strategy or automatic-invocation policy. This view performs no I/O, threshold
/// observations, scheduling or maintenance. A caller can use [`Config::values`] to read repeated
/// `maintenance.repo` entries independently; this view never registers repositories or a scheduler.
#[derive(Clone, Copy, Debug)]
pub struct MaintenanceConfig<'a> {
    config: &'a Config,
}

impl<'a> MaintenanceConfig<'a> {
    /// Borrows a caller-resolved snapshot without interpreting any settings.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::maintenance::{AutoThreshold, MaintenanceConfig, MaintenanceTask};
    /// let config = Config::parse(b"[maintenance \"loose-objects\"]\nauto = 1k\n")?;
    /// let policy = MaintenanceConfig::new(&config);
    /// assert_eq!(
    ///     policy.auto_threshold(MaintenanceTask::LooseObjects)?,
    ///     Some(AutoThreshold::Count(1024))
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    /// Resolves automatic invocation policy for caller-opted-in command triggers.
    ///
    /// An explicit `maintenance.auto` wins. Otherwise `gc.auto` must be positive (default 6700);
    /// zero and negative values suppress command triggers, matching Git's compatibility fallback.
    ///
    /// This does not control an explicit `maintenance run --auto`; that mode still checks each
    /// selected task's threshold. Reading this setting inserts no triggers or background work.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for a malformed consulted boolean or a malformed/out-of-range
    /// signed 32-bit `gc.auto`. An explicit boolean leaves `gc.auto` unconsulted.
    pub fn command_auto(&self) -> Result<bool, InvalidValue> {
        if let Some(enabled) = self.config.boolean("maintenance", None, "auto")? {
            return Ok(enabled);
        }
        let value = self.config.integer("gc", None, "auto")?.unwrap_or(6700);
        let value = i32::try_from(value).map_err(|_| {
            InvalidValue::new(
                "gc",
                "auto",
                self.config.value("gc", None, "auto").flatten(),
            )
        })?;
        Ok(value > 0)
    }

    /// Reports the explicitly configured, supported strategy without expanding a task set.
    ///
    /// Absence is preserved rather than supplying Git 2.56's unsupported default strategy.
    /// Even `incremental` does not enable implicit task selection or scheduling in girt.
    /// Explicit task selection need not consult this setting.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for an implicit assignment, empty value or unsupported strategy,
    /// including `geometric` and `gc`. Their default task sets require separate support.
    pub fn strategy(&self) -> Result<Option<MaintenanceStrategy>, InvalidValue> {
        match self.config.value("maintenance", None, "strategy") {
            None => Ok(None),
            Some(Some(b"incremental")) => Ok(Some(MaintenanceStrategy::Incremental)),
            Some(value) => Err(InvalidValue::new("maintenance", "strategy", value)),
        }
    }

    /// Reads a task's explicit enabled flag, preserving absence.
    ///
    /// No default task set is inferred. An explicit [`super::TaskSelection`] overrides this flag;
    /// a runner should not consult it for explicitly selected tasks.
    ///
    /// # Errors
    ///
    /// Returns [`TaskConfigError`] if the consulted flag is not a Git boolean.
    pub fn task_enabled(&self, task: MaintenanceTask) -> Result<Option<bool>, TaskConfigError> {
        self.config
            .boolean("maintenance", Some(task.to_string().as_bytes()), "enabled")
            .map_err(|source| TaskConfigError { task, source })
    }

    /// Reads the numeric automatic threshold for a supported count-based task.
    ///
    /// Defaults are 100 loose objects, 100 reachable commits absent from the commit graph, and
    /// 10 packs for incremental repack. A positive count is inclusive. All negative values mean
    /// always, and zero disables the task. Values use Git integer syntax and signed 32-bit bounds.
    ///
    /// Returns `None` for gc, prefetch and pack-refs without consulting their `.auto` settings:
    /// these tasks use their own built-in checks, rather than this numeric configuration.
    /// This method does not count objects, commits or packs, nor decide whether work is due.
    ///
    /// # Errors
    ///
    /// Returns [`TaskConfigError`] for a malformed or out-of-range consulted threshold.
    pub fn auto_threshold(
        &self,
        task: MaintenanceTask,
    ) -> Result<Option<AutoThreshold>, TaskConfigError> {
        let default = match task {
            MaintenanceTask::CommitGraph | MaintenanceTask::LooseObjects => 100,
            MaintenanceTask::IncrementalRepack => 10,
            MaintenanceTask::Gc | MaintenanceTask::Prefetch | MaintenanceTask::PackRefs => {
                return Ok(None);
            }
        };
        let name = task.to_string();
        let subsection = Some(name.as_bytes());
        let invalid = |source| TaskConfigError { task, source };
        let value = self
            .config
            .integer("maintenance", subsection, "auto")
            .map_err(invalid)?
            .unwrap_or(default);
        let value = i32::try_from(value).map_err(|_| {
            invalid(InvalidValue::new(
                "maintenance",
                "auto",
                self.config
                    .value("maintenance", subsection, "auto")
                    .flatten(),
            ))
        })?;
        Ok(Some(match value {
            0 => AutoThreshold::Disabled,
            value if value < 0 => AutoThreshold::Always,
            value => AutoThreshold::Count(value as u32),
        }))
    }
}
