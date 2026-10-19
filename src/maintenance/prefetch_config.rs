//! Admission of an already-resolved configuration before maintenance remote resolution.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Config;

/// Independent caps for config inspection and retained remote names, without reading sources.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InventoryLimits {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub max_remotes: usize,
}

impl Default for InventoryLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_bytes: 256 * 1024 * 1024,
            max_remotes: 4096,
        }
    }
}

/// One exact remote name, borrowed from its first config entry.
#[derive(Debug)]
pub(crate) struct RemoteEntry<'a> {
    pub name: &'a [u8],
    pub skip: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InventoryError {
    #[error("prefetch inventory limit: {0}")]
    Limit(&'static str),
    #[error("prefetch inventory cancelled")]
    Cancelled,
    #[error("invalid remote {key} boolean at config entry {entry}")]
    Boolean { key: &'static str, entry: usize },
}

/// Borrows names in first-entry order, validating every skip-flag occurrence.
///
/// Both skip spellings update the same flag in config order. Empty remote headers do not count,
/// matching the existing remote inventory. Every entry is charged, including unrelated sections;
/// logical bytes count its names, value and 128 bytes of bookkeeping. Limits precede parsing and
/// retained allocations for that entry. Cancellation is checked on entry and every 64 entries.
///
/// This admits neither URL resolution nor refspec parsing. Those later phases need their own
/// budgets, including repeated config scans and URL rewrite work. There is no I/O or mutation.
pub(crate) fn inventory<'a>(
    config: &'a Config,
    limits: InventoryLimits,
    cancel: &AtomicBool,
) -> Result<Vec<RemoteEntry<'a>>, InventoryError> {
    let mut remotes: Vec<RemoteEntry<'_>> = Vec::new();
    let mut positions = BTreeMap::new();
    let mut bytes = 0usize;
    if cancel.load(Ordering::Relaxed) {
        return Err(InventoryError::Cancelled);
    }
    for (index, entry) in config.entries().iter().enumerate() {
        if index % 64 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(InventoryError::Cancelled);
        }
        if index >= limits.max_entries {
            return Err(InventoryError::Limit("config entries"));
        }
        for amount in [
            128,
            entry.section.len(),
            entry.subsection.as_ref().map_or(0, Vec::len),
            entry.name.len(),
            entry.value.as_ref().map_or(0, Vec::len),
        ] {
            bytes = bytes
                .checked_add(amount)
                .filter(|&total| total <= limits.max_bytes)
                .ok_or(InventoryError::Limit("config bytes"))?;
        }
        if !entry.section.eq_ignore_ascii_case(b"remote") {
            continue;
        }
        let Some(name) = entry.subsection.as_deref() else {
            continue;
        };
        let position = match positions.get(name) {
            Some(&position) => position,
            None => {
                if remotes.len() >= limits.max_remotes {
                    return Err(InventoryError::Limit("remotes"));
                }
                let position = remotes.len();
                positions.insert(name, position);
                remotes.push(RemoteEntry { name, skip: false });
                position
            }
        };
        let key = if entry.name.eq_ignore_ascii_case(b"skipfetchall") {
            "skipFetchAll"
        } else if entry.name.eq_ignore_ascii_case(b"skipdefaultupdate") {
            "skipDefaultUpdate"
        } else {
            continue;
        };
        remotes[position].skip = crate::config::values::boolean(entry.value.as_deref()).ok_or(
            InventoryError::Boolean {
                key,
                entry: index + 1,
            },
        )?;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(InventoryError::Cancelled);
    }
    Ok(remotes)
}
