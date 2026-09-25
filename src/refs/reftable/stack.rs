use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, Limits, Table};
use crate::ObjectFormat;
use crate::refs::ReferenceError;
use crate::refs::store::{Lock, check_path, io_error, malformed};

/// Bounds shared by every table in one stack operation.
#[derive(Clone, Copy, Debug)]
pub struct StackLimits {
    /// Maximum table count and simultaneously pinned file handles.
    pub tables: usize,
    /// Maximum bytes in `tables.list`.
    pub list_bytes: usize,
    /// Aggregate encoded, decoded and record limits, plus per-block/string limits.
    pub records: Limits,
}

impl Default for StackLimits {
    fn default() -> Self {
        Self {
            tables: 1024,
            list_bytes: 1024 * 1024,
            records: Limits::default(),
        }
    }
}

/// An owned merged generation of a reftable stack.
///
/// Opening pins all listed files before reading their immutable contents. Once decoded, no file
/// handles remain. Existing snapshots remain usable after publication or compaction. A missing
/// listed file fails the single opening attempt; the caller may explicitly retry. In-place table
/// mutation, adversarial path replacement and network filesystems are outside this contract.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Merged records, including tombstones. Newer tables override older records with the same
    /// key.
    pub table: Table,
    pub(super) names: Vec<String>,
    pub(super) used_bytes: usize,
    pub(super) used_records: usize,
    pub(super) used_decoded: usize,
}

impl Snapshot {
    /// Reads a stack directory using one bounded attempt.
    ///
    /// An absent `tables.list` denotes an empty stack. Names must be single ASCII file components
    /// ending in `.ref` or `.log`. Cancellation is checked between file opens, table decodes and
    /// merges; one bounded table decode may finish before cancellation is observed.
    ///
    /// # Errors
    ///
    /// Reports I/O, malformed names/order/data, format mismatch, cancellation and exhausted
    /// aggregate budgets. No filesystem mutation or automatic retry occurs.
    pub fn read(
        directory: &Path,
        format: ObjectFormat,
        limits: StackLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, ReferenceError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "reftable.snapshot", outcome = "incomplete", failure_class = tracing::field::Empty);
        let operation = || Self::read_inner(directory, format, limits, cancel);
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::reference);
        result
    }

    fn read_inner(
        directory: &Path,
        format: ObjectFormat,
        limits: StackLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, ReferenceError> {
        cancelled(cancel)?;
        let directory = fs::canonicalize(directory).map_err(|error| io_error(directory, error))?;
        let directory = directory.as_path();
        let list_path = directory.join("tables.list");
        let bytes = match read_file(&list_path, limits.list_bytes) {
            Ok(bytes) => bytes,
            Err(ReferenceError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        let names = list(&bytes, limits, &list_path)?;
        let mut files = Vec::new();
        for name in &names {
            cancelled(cancel)?;
            let path = directory.join(name);
            check_path(&path)?;
            let file = File::open(&path).map_err(|error| io_error(&path, error))?;
            files.push((path, file));
        }
        let mut references = BTreeMap::new();
        let mut logs = BTreeMap::new();
        let mut remaining = limits.records;
        let mut min = 0;
        let mut max = 0;
        for (index, (path, mut file)) in files.into_iter().enumerate() {
            cancelled(cancel)?;
            let bytes = read_handle(&mut file, &path, remaining.bytes)?;
            let (table, record_count, decoded_bytes) = Table::decode_usage(&bytes, remaining)?;
            if table.format != format {
                return Err(Error::Malformed("stack hash format").into());
            }
            if index != 0 && table.min_update_index <= max {
                return Err(Error::Malformed("stack update order").into());
            }
            if index == 0 {
                min = table.min_update_index;
            }
            max = table.max_update_index;
            remaining.bytes -= bytes.len();
            remaining.records -= record_count;
            remaining.decoded_bytes -= decoded_bytes;
            for record in table.references {
                references.insert(record.name.clone(), record);
            }
            for record in table.logs {
                logs.insert(
                    (record.name.clone(), std::cmp::Reverse(record.update_index)),
                    record,
                );
            }
        }
        cancelled(cancel)?;
        Ok(Self {
            table: Table {
                format,
                min_update_index: min,
                max_update_index: max,
                references: references.into_values().collect(),
                logs: logs.into_values().collect(),
            },
            names,
            used_bytes: limits.records.bytes - remaining.bytes,
            used_records: limits.records.records - remaining.records,
            used_decoded: limits.records.decoded_bytes - remaining.decoded_bytes,
        })
    }
}

/// Completed compaction and obsolete files that could not be removed.
///
/// The replacement stack is already visible on success. Failed obsolete-file cleanup never
/// reverses publication. Retrying compaction is safe but is not an obsolete-file cleanup service.
#[derive(Debug)]
pub struct Compaction {
    /// Number of input tables replaced.
    pub input_tables: usize,
    /// Obsolete file paths and unlink failures; callers may inspect them before later cleanup.
    pub retained: Vec<(PathBuf, std::io::Error)>,
}

/// Compacts a complete stack while excluding cooperating writers and compactors.
///
/// Preserves all live reflog records and reference update indexes. Full-stack tombstones can be
/// discarded. Holds `tables.list.lock` and each input table's `.lock` until publication and cleanup
/// finish. No expiration, fsync, automatic retries or stale-lock stealing occurs. Cancellation
/// before list replacement leaves the old stack; after replacement cleanup runs to completion.
///
/// # Errors
///
/// Errors preserve the old stack. An unlisted immutable table may remain if list publication fails;
/// readers ignore it. Lock cleanup is best effort and process termination can leave stale locks.
/// Successful publication with failed obsolete-file cleanup is returned as
/// [`Compaction::retained`].
pub fn compact(
    directory: &Path,
    format: ObjectFormat,
    limits: StackLimits,
    cancel: &AtomicBool,
) -> Result<Compaction, ReferenceError> {
    #[cfg(feature = "tracing")]
    let span = tracing::debug_span!(target: "girt", "reftable.compact", outcome = "incomplete", failure_class = tracing::field::Empty);
    let operation = || compact_inner(directory, format, limits, cancel);
    #[cfg(feature = "tracing")]
    let result = span.in_scope(operation);
    #[cfg(not(feature = "tracing"))]
    let result = operation();
    #[cfg(feature = "tracing")]
    crate::trace::finish(&span, &result, crate::trace::reference);
    result
}

fn compact_inner(
    directory: &Path,
    format: ObjectFormat,
    limits: StackLimits,
    cancel: &AtomicBool,
) -> Result<Compaction, ReferenceError> {
    cancelled(cancel)?;
    let directory = fs::canonicalize(directory).map_err(|error| io_error(directory, error))?;
    let directory = directory.as_path();
    let lock = Lock::acquire(directory.join("tables.list"))?;
    let mut snapshot = Snapshot::read(directory, format, limits, cancel)?;
    let mut table_locks = Vec::new();
    for name in &snapshot.names {
        table_locks.push(Lock::acquire(directory.join(name))?);
    }
    let input_tables = snapshot.names.len();
    if input_tables == 0 {
        return Ok(Compaction {
            input_tables,
            retained: Vec::new(),
        });
    }
    snapshot
        .table
        .references
        .retain(|record| record.target.is_some());
    snapshot.table.logs.retain(|record| record.value.is_some());
    let bytes = snapshot.table.encode(limits.records)?;
    cancelled(cancel)?;
    publish(&lock, &[], &snapshot.table, &bytes, limits)?;
    let mut retained = Vec::new();
    for name in &snapshot.names {
        let path = directory.join(name);
        if let Err(error) = fs::remove_file(&path) {
            retained.push((path, error));
        }
    }
    Ok(Compaction {
        input_tables,
        retained,
    })
}

pub(super) fn publish(
    lock: &Lock,
    names: &[String],
    table: &Table,
    bytes: &[u8],
    limits: StackLimits,
) -> Result<(), ReferenceError> {
    publish_with(lock, names, table, bytes, limits, || Ok(()))
}

pub(super) fn publish_with(
    lock: &Lock,
    names: &[String],
    table: &Table,
    bytes: &[u8],
    limits: StackLimits,
    before_list: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), ReferenceError> {
    if names.len() >= limits.tables {
        return Err(Error::Limit("stack table count").into());
    }
    let directory = lock.destination.parent().unwrap();
    let prefix = format!(
        "0x{:012x}-0x{:012x}-",
        table.min_update_index, table.max_update_index
    );
    let mut temporary = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".ref")
        .tempfile_in(directory)
        .map_err(|error| io_error(directory, error))?;
    temporary
        .write_all(bytes)
        .map_err(|error| io_error(temporary.path(), error))?;
    let name = temporary
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let mut list = names.join("\n").into_bytes();
    if !list.is_empty() {
        list.push(b'\n');
    }
    list.extend_from_slice(name.as_bytes());
    list.push(b'\n');
    if list.len() > limits.list_bytes {
        return Err(Error::Limit("stack list bytes").into());
    }
    // Retain the uniquely named immutable file before making it reachable from the list.
    let (_file, _path) = temporary
        .keep()
        .map_err(|error| io_error(directory, error.error))?;
    before_list().map_err(|error| io_error(&lock.destination, error))?;
    lock.publish_retaining_lock(&list)
}

fn list(bytes: &[u8], limits: StackLimits, path: &Path) -> Result<Vec<String>, ReferenceError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let body = bytes
        .strip_suffix(b"\n")
        .ok_or_else(|| malformed(path, "unterminated stack list"))?;
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    for line in body.split(|b| *b == b'\n') {
        if names.len() == limits.tables {
            return Err(Error::Limit("stack table count").into());
        }
        if line.is_empty()
            || !line
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            || line.starts_with(b".")
            || !(line.ends_with(b".ref") || line.ends_with(b".log"))
        {
            return Err(malformed(path, "invalid stack table filename"));
        }
        let name = std::str::from_utf8(line).unwrap().to_owned();
        if !seen.insert(name.clone()) {
            return Err(malformed(path, "duplicate stack table"));
        }
        names.push(name);
    }
    Ok(names)
}

fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>, ReferenceError> {
    check_path(path)?;
    let mut file = File::open(path).map_err(|error| io_error(path, error))?;
    read_handle(&mut file, path, limit)
}

fn read_handle(file: &mut File, path: &Path, limit: usize) -> Result<Vec<u8>, ReferenceError> {
    let length = file
        .metadata()
        .map_err(|error| io_error(path, error))?
        .len();
    if length > limit as u64 {
        return Err(Error::Limit("stack bytes").into());
    }
    let mut bytes = Vec::new();
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    if bytes.len() > limit {
        return Err(Error::Limit("stack bytes").into());
    }
    Ok(bytes)
}

pub(super) fn cancelled(cancel: &AtomicBool) -> Result<(), ReferenceError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ReferenceError::Cancelled)
    } else {
        Ok(())
    }
}
