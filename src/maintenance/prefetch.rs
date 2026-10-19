//! Pure prefetch refspec preparation; transport and publication remain caller-controlled.
use std::sync::atomic::{AtomicBool, Ordering};

use crate::remote::{Direction, Refspec, Refspecs, RefspecsError};

/// Bounds for [`PrefetchRefspecs::parse`], including specifications that are skipped.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PrefetchLimits {
    /// Maximum input occurrences, including duplicates and negative selectors.
    pub max_specs: usize,
    /// Aggregate raw input bytes, before filtering.
    pub max_input_bytes: usize,
    /// Aggregate rewritten bytes, including retained negative selectors.
    /// The final parser additionally owns raw/source/destination fields; temporary and parsed
    /// bytes can coexist.
    pub max_output_bytes: usize,
}

impl Default for PrefetchLimits {
    fn default() -> Self {
        Self {
            max_specs: 1024,
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Why one valid positive fetch specification does not participate in prefetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PrefetchSkipReason {
    /// No tracking destination was specified, so no cache reference would retain fetched data.
    NoDestination,
    /// The literal source starts with `refs/tags/`, which Git's prefetch omits.
    /// Broad patterns such as `refs/*` can still select tags.
    TagSource,
}

/// A skipped occurrence, retaining its position in the original input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SkippedPrefetchSpec {
    /// Zero-based input occurrence.
    pub index: usize,
    /// Why this specification was omitted.
    pub reason: PrefetchSkipReason,
}

/// Prepared fetch specifications whose destinations are confined to `refs/prefetch/`.
///
/// Preparation reads no configuration, URL, advertisement or storage. It preserves negative
/// selectors and syntactic force intent, which does not itself authorize an update. Callers compose
/// [`crate::fetch::FetchRequest`] with explicit transfer/publication limits to validate objects
/// and conditionally update references.
/// Remote selection, pruning policy and scheduling are separate decisions.
#[derive(Clone, Debug)]
pub struct PrefetchRefspecs {
    refspecs: Refspecs,
    skipped: Vec<SkippedPrefetchSpec>,
}

/// Prefetch preparation failed; no plan or storage effects are returned.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrefetchRefspecsError {
    /// An input is malformed or outside the existing full-name fetch grammar.
    #[error("invalid prefetch input refspec")]
    Input(#[source] RefspecsError),
    /// Validation of the rewritten specification failed.
    #[error("invalid rewritten prefetch refspec")]
    Output(#[source] RefspecsError),
    /// Input occurrence count exceeded the caller's limit.
    #[error("prefetch refspec count limit exceeded")]
    SpecLimit,
    /// Aggregate raw input bytes exceeded the caller's limit.
    #[error("prefetch refspec input byte limit exceeded")]
    InputByteLimit,
    /// Aggregate rewritten bytes exceeded the caller's limit.
    #[error("prefetch refspec output byte limit exceeded")]
    OutputByteLimit,
    /// Cancellation was observed before a plan was returned.
    #[error("prefetch refspec preparation cancelled")]
    Cancelled,
}

impl PrefetchRefspecs {
    /// Validates and prepares raw fetch refspec values, like Git's `--prefetch` policy.
    ///
    /// Rewrites `refs/remotes/origin/*` destinations as `refs/prefetch/remotes/origin/*`.
    /// Positive specifications without a destination or with a literal `refs/tags/` source are
    /// skipped; broad sources and negative selectors keep their meaning. All inputs are validated
    /// before filtering. The supported grammar is the same full-name subset as [`Refspec`].
    ///
    /// Checks cancellation before input iteration, after batches of 64 inputs and around final
    /// validation. One bounded parse may finish before cancellation is observed. The iterator
    /// itself is caller-owned code and must return promptly.
    ///
    /// # Errors
    ///
    /// Returns [`PrefetchRefspecsError`] for malformed/unsupported input, exhausted bounds or
    /// cancellation. No transport is opened and no files are changed.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::maintenance::{PrefetchLimits, PrefetchRefspecs};
    ///
    /// let prepared = PrefetchRefspecs::parse(
    ///     ["+refs/heads/*:refs/remotes/origin/*"],
    ///     PrefetchLimits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(
    ///     prepared.refspecs().specs()[0].as_bytes(),
    ///     b"+refs/heads/*:refs/prefetch/remotes/origin/*"
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn parse<I, B>(
        values: I,
        limits: PrefetchLimits,
        cancel: &AtomicBool,
    ) -> Result<Self, PrefetchRefspecsError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        check(cancel)?;
        let mut input = values.into_iter();
        let mut count: usize = 0;
        let mut input_bytes: usize = 0;
        let mut output_bytes: usize = 0;
        let mut output = Vec::new();
        let mut skipped = Vec::new();
        loop {
            if count != 0 && count.is_multiple_of(64) {
                check(cancel)?;
            }
            let Some(value) = input.next() else {
                break;
            };
            if count >= limits.max_specs {
                return Err(PrefetchRefspecsError::SpecLimit);
            }
            let index = count;
            count += 1;
            let raw = value.as_ref();
            input_bytes = input_bytes
                .checked_add(raw.len())
                .filter(|total| *total <= limits.max_input_bytes)
                .ok_or(PrefetchRefspecsError::InputByteLimit)?;
            let spec = Refspec::parse(Direction::Fetch, raw)
                .map_err(|source| PrefetchRefspecsError::Input(RefspecsError { index, source }))?;
            let rewritten = if spec.is_negative() {
                charge_output(&mut output_bytes, raw.len(), limits)?;
                raw.to_vec()
            } else {
                let body = raw.strip_prefix(b"+").unwrap_or(raw);
                let Some(separator) = body.iter().position(|byte| *byte == b':') else {
                    skipped.push(SkippedPrefetchSpec {
                        index,
                        reason: PrefetchSkipReason::NoDestination,
                    });
                    continue;
                };
                let source = &body[..separator];
                let destination = &body[separator + 1..];
                let reason = if destination.is_empty() {
                    Some(PrefetchSkipReason::NoDestination)
                } else if source.starts_with(b"refs/tags/") {
                    Some(PrefetchSkipReason::TagSource)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    skipped.push(SkippedPrefetchSpec { index, reason });
                    continue;
                }
                let size = raw
                    .len()
                    .checked_add(b"prefetch/".len())
                    .ok_or(PrefetchRefspecsError::OutputByteLimit)?;
                charge_output(&mut output_bytes, size, limits)?;
                let mut bytes = Vec::with_capacity(size);
                if spec.force() {
                    bytes.push(b'+');
                }
                bytes.extend_from_slice(source);
                bytes.extend_from_slice(b":refs/prefetch/");
                // The input parser already requires a nonempty full refs/ destination.
                bytes.extend_from_slice(&destination[b"refs/".len()..]);
                bytes
            };
            output.push(rewritten);
        }
        check(cancel)?;
        let refspecs =
            Refspecs::parse(Direction::Fetch, output).map_err(PrefetchRefspecsError::Output)?;
        check(cancel)?;
        Ok(Self { refspecs, skipped })
    }

    /// Validated output, including preserved negative selectors. An all-negative list selects
    /// nothing; callers need not connect to a remote when there is no positive mapping.
    pub fn refspecs(&self) -> &Refspecs {
        &self.refspecs
    }

    /// Skipped positive occurrences, in input order.
    pub fn skipped(&self) -> &[SkippedPrefetchSpec] {
        &self.skipped
    }
}

fn charge_output(
    used: &mut usize,
    size: usize,
    limits: PrefetchLimits,
) -> Result<(), PrefetchRefspecsError> {
    *used = used
        .checked_add(size)
        .filter(|total| *total <= limits.max_output_bytes)
        .ok_or(PrefetchRefspecsError::OutputByteLimit)?;
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), PrefetchRefspecsError> {
    if cancel.load(Ordering::Relaxed) {
        Err(PrefetchRefspecsError::Cancelled)
    } else {
        Ok(())
    }
}
