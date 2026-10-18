//! Optional full payload-policy checks on a validated artifact, before any destination mutation.
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::ReceivedFetch;
use crate::fsck::{Policy, Scope, TransferError, TransferFinding, TransferLimits, TransferReport};

impl ReceivedFetch {
    /// Checks every received object against explicit fsck policy, using default bounds.
    ///
    /// Uses [`Self::check_fsck_with`] with no cancellation. Connectivity and pack identities were
    /// already validated during receive; this additionally checks payload conventions in strict
    /// mode.
    ///
    /// ```
    /// use girt::fetch::ReceivedFetch;
    /// use girt::fsck::{Policy, TransferError};
    ///
    /// fn inspect(received: &ReceivedFetch) -> Result<(), TransferError> {
    ///     let report = received.check_fsck(&Policy::default())?;
    ///     if report.has_errors() {
    ///         return Err(TransferError::Rejected(Box::new(report)));
    ///     }
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns artifact, decoding or limit failures; policy findings remain in the successful
    /// report.
    pub fn check_fsck(&self, policy: &Policy) -> Result<TransferReport, TransferError> {
        self.check_fsck_with(policy, TransferLimits::default(), &AtomicBool::new(false))
    }

    /// Verifies pack/index checksums, decodes every object and applies strict fsck policy.
    ///
    /// Stages a bounded private copy of the validated pack/index and reuses the ordinary pack
    /// verifier and decoder. This optional operation uses temporary disk space up to the artifact
    /// bounds and repeats decoding; disabled configured checks incur neither cost. No destination
    /// data changes. Skip lists cannot suppress storage, checksum, identity or decoding failures.
    ///
    /// # Errors
    ///
    /// Returns temporary I/O, corruption, cancellation or resource errors. A report with findings
    /// is complete; call [`TransferReport::has_errors`] before accepting it.
    pub fn check_fsck_with(
        &self,
        policy: &Policy,
        limits: TransferLimits,
        cancel: &AtomicBool,
    ) -> Result<TransferReport, TransferError> {
        let mut report = TransferReport {
            unknown_messages: policy.unknown_messages().to_vec(),
            ..Default::default()
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(TransferError::Cancelled);
        }
        if self.objects > limits.max_objects {
            return Err(TransferError::Limit("objects"));
        }
        if self.pack.is_empty() {
            return Ok(report);
        }
        if self
            .pack
            .len()
            .checked_add(self.index.len())
            .is_none_or(|bytes| bytes > limits.packs.max_bytes)
            || self.index.len() > limits.packs.max_index_bytes
        {
            return Err(TransferError::Limit("pack/index bytes"));
        }
        let directory = tempfile::tempdir()?;
        let index_path = directory.path().join("incoming.idx");
        {
            let mut pack = File::create(index_path.with_extension("pack"))?;
            self.pack.copy_to(&mut pack, cancel).map_err(Box::new)?;
            pack.flush()?;
        }
        std::fs::write(&index_path, &self.index)?;
        let mut pack_limits = limits.packs;
        let pack = crate::pack::FilePack::open(self.format, &index_path, &mut pack_limits, cancel)
            .map_err(Box::new)?;
        if pack.object_count() > limits.max_objects {
            return Err(TransferError::Limit("objects"));
        }
        pack.verify(cancel).map_err(Box::new)?;
        for position in 0..pack.object_count() {
            if cancel.load(Ordering::Relaxed) {
                return Err(TransferError::Cancelled);
            }
            let object = pack.read(position, limits.read, cancel).map_err(Box::new)?;
            let id = pack.identity_at(position);
            report.checked_objects += 1;
            report.decoded_bytes = report
                .decoded_bytes
                .checked_add(object.data().len() as u64)
                .ok_or(TransferError::Limit("decoded bytes"))?;
            if report.decoded_bytes > limits.max_bytes {
                return Err(TransferError::Limit("decoded bytes"));
            }
            for finding in policy.check_object(id, object.kind(), object.data(), true) {
                if report.findings.len() >= limits.max_findings {
                    return Err(TransferError::Limit("findings"));
                }
                report.findings.push(TransferFinding {
                    id,
                    kind: object.kind(),
                    finding,
                });
            }
        }
        Ok(report)
    }

    /// Enforces one configured transfer policy before installation or publication.
    ///
    /// Operation-specific false overrides `transfer.fsckObjects`; operation-specific message and
    /// skip-list settings never fall back to repository `fsck.*`. Relative skip paths use explicit
    /// `base`. Successful warning-only reports are returned for caller reporting.
    ///
    /// # Errors
    ///
    /// Returns configuration/decoding failures or [`TransferError::Rejected`] retaining every
    /// finding.
    pub fn enforce_fsck(
        &self,
        config: &crate::Config,
        scope: Scope,
        base: &Path,
        cancel: &AtomicBool,
    ) -> Result<Option<TransferReport>, TransferError> {
        if !crate::fsck::transfer_enabled(config, scope).map_err(Box::new)? {
            return Ok(None);
        }
        let policy = Policy::from_config(config, scope, self.format, base, Default::default())?;
        let report = self.check_fsck_with(&policy, Default::default(), cancel)?;
        if report.has_errors() {
            return Err(TransferError::Rejected(Box::new(report)));
        }
        Ok(Some(report))
    }
}
