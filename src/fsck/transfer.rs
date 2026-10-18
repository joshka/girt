//! Received-pack policy reports, independent of object installation and reference publication.
use super::{Finding, Scope, Severity};
use crate::{ObjectId, ObjectKind};

/// Bounds for an optional full received-pack check.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct TransferLimits {
    /// Maximum received objects, including materialized external delta bases.
    pub max_objects: usize,
    /// Maximum total decoded payload bytes (default 4 GiB).
    pub max_bytes: u64,
    /// Maximum retained findings.
    pub max_findings: usize,
    /// Pack/index snapshot bounds; also bounds the temporary artifact copy.
    pub packs: crate::PackLimits,
    /// Per-object decoding bounds.
    pub read: crate::ReadLimits,
}
impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            max_objects: 1 << 24,
            max_bytes: 1 << 32,
            max_findings: 1 << 20,
            packs: Default::default(),
            read: Default::default(),
        }
    }
}

/// One received object's configured diagnostic.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TransferFinding {
    /// Received object identity.
    pub id: ObjectId,
    /// Decoded kind.
    pub kind: ObjectKind,
    /// Message ID and effective severity.
    pub finding: Finding,
}

/// Completed optional received-pack check. A successful check does not install or publish anything.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct TransferReport {
    /// Objects decoded during this check.
    pub checked_objects: usize,
    /// Total payload bytes decoded.
    pub decoded_bytes: u64,
    /// Configured findings in object-ID order.
    pub findings: Vec<TransferFinding>,
    /// Unknown transfer-policy IDs that Git treats as configuration warnings.
    pub unknown_messages: Vec<Vec<u8>>,
}
impl TransferReport {
    /// Whether policy requires refusing the incoming pack.
    pub fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| matches!(finding.finding.severity, Severity::Error | Severity::Fatal))
    }
}

/// Received-pack policy or decoding failed before installation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransferError {
    /// A configured boolean is invalid.
    #[error(transparent)]
    Config(#[from] Box<crate::config::InvalidValue>),
    /// Message policy or skip-list loading failed.
    #[error(transparent)]
    Policy(#[from] super::PolicyError),
    /// Existing pack verification or object decoding failed.
    #[error(transparent)]
    Read(#[from] Box<crate::ObjectReadError>),
    /// Copying a private transfer artifact failed.
    #[error(transparent)]
    Artifact(#[from] Box<crate::fetch::FetchError>),
    /// Temporary-file I/O failed.
    #[error("received-pack fsck I/O")]
    Io(#[from] std::io::Error),
    /// A complete typed report requires refusing the pack.
    #[error("received pack failed fsck policy")]
    Rejected(Box<TransferReport>),
    /// An aggregate bound was exhausted.
    #[error("received-pack fsck limit exceeded: {0}")]
    Limit(&'static str),
    /// Caller cancellation.
    #[error("received-pack fsck cancelled")]
    Cancelled,
}

/// Resolves operation-specific `fsckObjects`, falling back only to `transfer.fsckObjects`.
/// Repository scans are always enabled. An explicit operation-specific false overrides transfer
/// true.
///
/// # Errors
///
/// Returns an invalid configured boolean; does not parse message policy when checks are disabled.
pub fn transfer_enabled(
    config: &crate::Config,
    scope: Scope,
) -> Result<bool, crate::config::InvalidValue> {
    let section = match scope {
        Scope::Repository => return Ok(true),
        Scope::Fetch => "fetch",
        Scope::Receive => "receive",
    };
    match config.boolean(section, None, "fsckobjects")? {
        Some(enabled) => Ok(enabled),
        None => Ok(config
            .boolean("transfer", None, "fsckobjects")?
            .unwrap_or(false)),
    }
}
