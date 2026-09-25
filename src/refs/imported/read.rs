use std::fs::File;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use super::ImportedRecord;
use crate::refs::store::{check_path, io_error};
use crate::refs::{Backend, RefName, ReferenceError, References};
use crate::{ObjectFormat, ObjectId};

/// Budgets for one imported history scan. All include incomplete retained prefixes.
///
/// Files are read in chunks of at most 8 KiB with cancellation between chunks and records.
/// Reftable additionally uses [`References::with_reftable_limits`] for encoded stack bytes,
/// decompression and aggregate records; a bounded table decode is one cancellation interval.
/// Payload budgets exclude allocator overhead and fixed record structs, bounded by `records`.
#[derive(Clone, Copy, Debug)]
pub struct ReflogLimits {
    /// Maximum files bytes read, or decoded payload bytes visited for the selected binary log.
    pub bytes: usize,
    /// Maximum bytes in one file record or one binary record's fields.
    pub record_bytes: usize,
    /// Maximum aggregate retained payload bytes.
    pub retained_bytes: usize,
    /// Maximum retained records, including malformed and incomplete records.
    pub records: usize,
}

impl Default for ReflogLimits {
    fn default() -> Self {
        Self {
            bytes: 64 * 1024 * 1024,
            record_bytes: 2 * 1024 * 1024,
            retained_bytes: 64 * 1024 * 1024,
            records: 100_000,
        }
    }
}

/// Why reading stopped. EOF alone does not establish a complete usable root set.
#[derive(Debug)]
pub enum ReflogReadEnd {
    /// All available records were read; inspect [`ImportedReflog::is_complete`] for corruption.
    Eof,
    /// A named budget prevented further reading/retention. No continuation is implied.
    Limit(&'static str),
    /// Cooperative cancellation was observed.
    Cancelled,
    /// An I/O failure occurred; retained prefixes and earlier records remain inspectable.
    Io(io::Error),
}

/// An owned bounded history, in oldest-to-newest order, with explicit scan completion.
///
/// No retention or expiry policy is applied. Zero IDs are excluded from recoverable roots; IDs
/// are not checked for existence or object kind. Files reads are live, not repository-wide GC
/// snapshots: cooperating writers may append and independent writers may replace/truncate files.
/// Even a complete scan requires the caller's repository coordination before collecting objects.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::refs::{ImportedReflog, ReflogLimits};
/// use girt::{ObjectFormat, ObjectId};
/// let format = ObjectFormat::Sha1;
/// let tip = ObjectId::Sha1([1; 20]);
/// let bytes = format!(
///     "{} {tip} A <a@b> 18446744073709551615 +0000\timported\n",
///     ObjectId::null(format)
/// );
/// let log = ImportedReflog::read(
///     format,
///     bytes.as_bytes(),
///     ReflogLimits::default(),
///     &AtomicBool::new(false),
/// );
/// assert!(log.is_complete());
/// assert_eq!(log.records()[0].fields()?.seconds, i128::from(u64::MAX));
/// assert_eq!(log.recoverable_roots().collect::<Vec<_>>(), [tip]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct ImportedReflog {
    records: Vec<ImportedRecord>,
    end: ReflogReadEnd,
}

impl ImportedReflog {
    /// Reads newline-delimited files data from a caller-owned reader without unbounded buffering.
    ///
    /// The reader may already buffer data; these limits bound requests made here. No extra byte is
    /// read beyond a byte budget to prove EOF, so a file ending exactly at that budget reports a
    /// limit. I/O, cancellation and limit stops return retained data rather than discard roots.
    pub fn read(
        format: ObjectFormat,
        mut reader: impl Read,
        limits: ReflogLimits,
        cancel: &AtomicBool,
    ) -> Self {
        let mut result = Self {
            records: Vec::new(),
            end: ReflogReadEnd::Eof,
        };
        let mut line = Vec::new();
        let mut retained = 0;
        let mut consumed = 0;
        let mut buffer = [0u8; 8192];
        'read: loop {
            if cancel.load(Ordering::Relaxed) {
                result.end = ReflogReadEnd::Cancelled;
                break;
            }
            let allowance = limits
                .bytes
                .saturating_sub(consumed)
                .min(limits.retained_bytes.saturating_sub(retained));
            if allowance == 0 {
                result.end = ReflogReadEnd::Limit("bytes or retained bytes");
                break;
            }
            let count = match reader.read(&mut buffer[..allowance.min(8192)]) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    result.end = ReflogReadEnd::Io(error);
                    break;
                }
            };
            consumed += count;
            for &byte in &buffer[..count] {
                if cancel.load(Ordering::Relaxed) {
                    result.end = ReflogReadEnd::Cancelled;
                    break 'read;
                }
                if result.records.len() == limits.records {
                    result.end = ReflogReadEnd::Limit("records");
                    break 'read;
                }
                if line.len() == limits.record_bytes {
                    result.end = ReflogReadEnd::Limit("record bytes");
                    break 'read;
                }
                line.push(byte);
                retained += 1;
                if byte == b'\n' {
                    result.records.push(ImportedRecord::File {
                        format,
                        bytes: std::mem::take(&mut line),
                    });
                }
            }
        }
        if !line.is_empty() {
            result.records.push(ImportedRecord::File {
                format,
                bytes: line,
            });
        }
        result
    }

    /// Retained records, including corrupt lines and any partial final record.
    pub fn records(&self) -> &[ImportedRecord] {
        &self.records
    }

    /// Returns why the scan stopped, independently of individual record interpretation.
    pub fn end(&self) -> &ReflogReadEnd {
        &self.end
    }

    /// True only after EOF and successful interpretation of every retained record.
    ///
    /// False forbids treating recovered IDs as an exhaustive root set. Preserve candidates and
    /// repair/retry or defer GC; never silently omit an unread or uninterpretable portion.
    pub fn is_complete(&self) -> bool {
        matches!(self.end, ReflogReadEnd::Eof) && self.records.iter().all(|r| r.fields().is_ok())
    }

    /// Returns conservative candidates, including IDs recovered from malformed and partial lines.
    ///
    /// Duplicates remain in file order. See [`Self::is_complete`] before making retention
    /// decisions.
    pub fn recoverable_roots(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.records
            .iter()
            .flat_map(ImportedRecord::recoverable_roots)
    }

    pub(crate) fn binary(
        records: impl Iterator<Item = crate::refs::reftable::LogRecord>,
        limits: ReflogLimits,
        cancel: &AtomicBool,
    ) -> Self {
        let mut result = Self {
            records: Vec::new(),
            end: ReflogReadEnd::Eof,
        };
        let mut retained = 0usize;
        for record in records {
            if cancel.load(Ordering::Relaxed) {
                result.end = ReflogReadEnd::Cancelled;
                break;
            }
            let Some(value) = record.value else {
                continue;
            };
            let size = value
                .name
                .len()
                .saturating_add(value.email.len())
                .saturating_add(value.message.len())
                .saturating_add(2 * value.old.format().digest_len() + 18);
            if result.records.len() == limits.records {
                result.end = ReflogReadEnd::Limit("records");
                break;
            }
            if size > limits.record_bytes {
                result.end = ReflogReadEnd::Limit("record bytes");
                break;
            }
            if size > limits.bytes.saturating_sub(retained)
                || size > limits.retained_bytes.saturating_sub(retained)
            {
                result.end = ReflogReadEnd::Limit("bytes or retained bytes");
                break;
            }
            retained += size;
            result.records.push(ImportedRecord::Reftable {
                update_index: record.update_index,
                value,
            });
        }
        result
    }
}

impl References<'_> {
    /// Reads bounded imported history for one name, without canonical append validation.
    ///
    /// Returns `None` for an absent files log or a binary log with no live records. Routes shared
    /// and per-worktree names like reference operations. Files preserve exact bytes; binary data
    /// preserves decoded fields after tombstones and precedence. Binary stack budgets remain
    /// separate from the selected-log budgets; both the snapshot and retained output can coexist.
    ///
    /// # Errors
    ///
    /// Opening/path failures and reftable snapshot failures return an error, never an empty root
    /// set. Files I/O after opening and selected-log limits/cancellation are reported in the owned
    /// result. Check [`ImportedReflog::is_complete`] before considering roots complete.
    pub fn imported_reflog(
        &self,
        name: &RefName,
        limits: ReflogLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<ImportedReflog>, ReferenceError> {
        if self.repository.reference_backend() == Backend::Reftable {
            return crate::refs::reftable::backend::imported_reflog(self, name, limits, cancel);
        }
        let path = self.reflog_path(name)?;
        check_path(&path)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&path, error)),
        };
        Ok(Some(ImportedReflog::read(
            self.repository.object_format(),
            file,
            limits,
            cancel,
        )))
    }
}
