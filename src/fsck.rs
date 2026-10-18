//! Integrity diagnostics for stored Git object payloads.
//!
//! Parsing an object establishes readable structure; fsck also checks conventions that ordinary
//! readers preserve, such as tree ordering and reserved names. Findings retain Git message IDs
//! for policy and reporting. This module does not repair or rewrite payloads.
//!
//! ```
//! use girt::{ObjectFormat, ObjectKind};
//!
//! let findings = girt::fsck::check_object(ObjectFormat::Sha1, ObjectKind::Blob, b"contents");
//! assert!(findings.is_empty());
//! ```

use std::collections::BTreeSet;

use crate::{ObjectFormat, ObjectKind, Tree};

mod message;
mod policy;

pub use message::{MessageId, Severity};
pub use policy::{Policy, PolicyError, PolicyLimits, Scope};

/// One payload defect. Repeated instances of the same category are reported once per object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Finding {
    /// Stable category for configuration and machine-readable reports.
    pub message_id: MessageId,
    /// Effective severity after strict mode and configured overrides.
    pub severity: Severity,
}

/// Checks one already bounded payload without resolving its object references.
///
/// Blobs have no structural constraints. Trees are checked for framing, ordering, duplicate and
/// reserved names, null IDs and mode spellings. Commit and tag checks validate graph headers,
/// required identities and readable dates. The complete Git fsck message catalogue is not covered.
/// Memory is proportional to the supplied payload; bound storage reads before calling this.
/// Results use default, non-strict reporting severities, are sorted by category and contain no
/// duplicate categories. Use [`Policy::check_object`] for configured or strict checks.
pub fn check_object(format: ObjectFormat, kind: ObjectKind, data: &[u8]) -> Vec<Finding> {
    check_payload(format, kind, data, 4096, false)
}

fn check_payload(
    format: ObjectFormat,
    kind: ObjectKind,
    data: &[u8],
    max_name: usize,
    strict: bool,
) -> Vec<Finding> {
    let mut ids = BTreeSet::new();
    match kind {
        ObjectKind::Blob => {}
        ObjectKind::Commit => match crate::Commit::parse(format, data) {
            Err(_) => {
                ids.insert(MessageId::BadCommit);
            }
            Ok(commit) => {
                check_identity(commit.author(), MessageId::MissingAuthor, &mut ids);
                check_identity(commit.committer(), MessageId::MissingCommitter, &mut ids);
            }
        },
        ObjectKind::Tag => match crate::Tag::parse(format, data) {
            Err(_) => {
                ids.insert(MessageId::BadTagObject);
            }
            Ok(tag) => match tag.tagger() {
                Ok(Some(identity)) if identity.date().is_err() => {
                    ids.insert(MessageId::BadDate);
                }
                Ok(None) => {
                    ids.insert(MessageId::MissingTaggerEntry);
                }
                Err(_) => {
                    ids.insert(MessageId::BadName);
                }
                _ => {}
            },
        },
        ObjectKind::Tree => check_tree(format, data, &mut ids, max_name, strict),
    }
    ids.into_iter()
        .map(|message_id| Finding {
            message_id,
            severity: message_id.severity(false),
        })
        .collect()
}

fn check_identity(
    identity: Result<Option<crate::IdentityRef<'_>>, crate::CommitError>,
    missing: MessageId,
    ids: &mut BTreeSet<MessageId>,
) {
    match identity {
        Ok(None) => {
            ids.insert(missing);
        }
        Err(_) => {
            ids.insert(MessageId::BadName);
        }
        Ok(Some(identity)) if identity.date().is_err() => {
            ids.insert(MessageId::BadDate);
        }
        _ => {}
    }
}

fn check_tree(
    format: ObjectFormat,
    data: &[u8],
    ids: &mut BTreeSet<MessageId>,
    max_name: usize,
    strict: bool,
) {
    let Ok(tree) = Tree::parse(format, data) else {
        ids.insert(MessageId::BadTree);
        return;
    };
    let mut names = BTreeSet::new();
    let mut previous = None;
    let mut rest = data;
    for entry in tree.entries() {
        if !names.insert(entry.name.as_slice()) {
            ids.insert(MessageId::DuplicateEntries);
        }
        if previous.is_some_and(|previous: &crate::TreeEntry| previous.git_cmp(entry).is_gt()) {
            ids.insert(MessageId::TreeNotSorted);
        }
        previous = Some(entry);
        let name = entry.name.as_slice();
        if name.len() > max_name {
            ids.insert(MessageId::LargePathname);
        }
        match name {
            b"" => {
                ids.insert(MessageId::EmptyName);
            }
            b"." => {
                ids.insert(MessageId::HasDot);
            }
            b".." => {
                ids.insert(MessageId::HasDotdot);
            }
            _ => {}
        }
        if name.contains(&b'/') {
            ids.insert(MessageId::FullPathname);
        }
        if crate::worktree_names::Protection::ALL.refuses_component(name) {
            ids.insert(MessageId::HasDotgit);
        }
        if entry.id.is_null() {
            ids.insert(MessageId::NullSha1);
        }
        if let Some((mode, tail)) = crate::bytes::split_once(rest, b' ') {
            if mode.starts_with(b"0") {
                ids.insert(MessageId::ZeroPaddedFilemode);
            }
            let numeric = mode.iter().try_fold(0u32, |value, byte| {
                value
                    .checked_mul(8)?
                    .checked_add(u32::from(byte.checked_sub(b'0')?))
            });
            if (strict && numeric == Some(0o100664))
                || !matches!(
                    numeric,
                    Some(0o40000 | 0o100644 | 0o100664 | 0o100755 | 0o120000 | 0o160000)
                )
            {
                ids.insert(MessageId::BadFilemode);
            }
            if let Some((_, tail)) = crate::bytes::split_once(tail, 0) {
                rest = tail.get(format.digest_len()..).unwrap_or_default();
            }
        }
    }
}

/// Bounds and choices for a read-only repository scan.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Root discovery and aggregate graph bounds. Recent-object cutoffs do not affect roots.
    /// `max_bytes` bounds all decoded payloads, including blobs (default 4 GiB); raise it for
    /// repositories whose total payload exceeds that size. This bounds work, not retained memory.
    pub roots: crate::retention::RetentionPolicy,
    /// Object inventory bounds.
    pub listing: crate::objects::ListLimits,
    /// Per-object decoding bounds.
    pub read: crate::ReadLimits,
    /// Skip payload convention checks while still decoding and following graph edges.
    pub connectivity_only: bool,
    /// Promote default warnings to errors, without changing explicit severity overrides.
    pub strict: bool,
    /// Explicit policy, or repository `fsck.*` configuration when absent.
    pub policy: Option<Policy>,
    /// Base for relative skip-list paths; defaults to the worktree or bare Git directory.
    pub skip_list_base: Option<std::path::PathBuf>,
}

/// A defect associated with a stored object or one of its dependencies.
#[derive(Debug)]
#[non_exhaustive]
pub enum ObjectFinding {
    /// A payload convention or format defect.
    Payload {
        /// Object containing the defect.
        id: crate::ObjectId,
        /// Typed payload diagnostic.
        finding: Finding,
    },
    /// An object named by a root or object edge is absent.
    Missing {
        /// Missing identity.
        id: crate::ObjectId,
    },
    /// A typed edge points to an object of another kind.
    WrongKind {
        /// Edge target.
        id: crate::ObjectId,
        /// Kind required by the source object.
        expected: ObjectKind,
        /// Kind actually stored.
        actual: ObjectKind,
    },
    /// Storage failed identity, compression or bounded decoding checks.
    Read {
        /// Object being read.
        id: crate::ObjectId,
        /// Original storage failure.
        source: Box<crate::ObjectReadError>,
    },
}

/// Completed integrity observation. Concurrent writers can invalidate this snapshot immediately.
#[derive(Debug)]
#[non_exhaustive]
pub struct Report {
    /// Payload, storage and connectivity defects; ordering is not an API guarantee.
    pub findings: Vec<ObjectFinding>,
    /// Objects reached from refs, reflogs, indexes and worktree HEADs.
    pub reachable: BTreeSet<crate::ObjectId>,
    /// Stored objects outside that closure.
    pub unreachable: BTreeSet<crate::ObjectId>,
    /// Unreachable objects that no other stored object references.
    pub dangling: BTreeSet<crate::ObjectId>,
}

impl ObjectFinding {
    /// Effective severity; missing, mistyped and unreadable objects always remain errors.
    pub fn severity(&self) -> Severity {
        match self {
            Self::Payload { finding, .. } => finding.severity,
            _ => Severity::Error,
        }
    }
}

impl Report {
    /// Whether a completed scan reports corruption requiring a failing command exit status.
    pub fn has_errors(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| matches!(finding.severity(), Severity::Error | Severity::Fatal))
    }
}

/// A scan could not establish complete inventory or root discovery.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Configured message policy or skip-list loading failed.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// Storage inventory or pack verification failed.
    #[error(transparent)]
    Objects(#[from] crate::ObjectReadError),
    /// Root discovery failed; do not interpret an incomplete root set as unreachability.
    #[error(transparent)]
    Roots(#[from] Box<crate::retention::RetentionError>),
    /// An auxiliary index could not be loaded or disagreed with stored objects.
    #[error("invalid repository accelerator")]
    Accelerator(#[source] Box<crate::Error>),
    /// The caller cancelled the scan.
    #[error("integrity scan cancelled")]
    Cancelled,
    /// An aggregate graph or decoded-byte bound stopped the scan.
    #[error("integrity scan limit exceeded: {0}")]
    Limit(&'static str),
}

impl crate::Repository {
    /// Checks visible objects and connectivity using default bounds.
    ///
    /// Uses [`Self::fsck_with`] with default options and a never-set cancellation flag.
    ///
    /// # Errors
    ///
    /// Returns inventory/root failures rather than claiming a complete report.
    pub fn fsck(&self) -> Result<Report, Error> {
        self.fsck_with(
            &Options::default(),
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    /// Decodes all visible objects, checks payloads and follows typed graph edges from all roots.
    ///
    /// Includes alternate stores and skips external gitlinks and declared shallow parent edges.
    /// Pack checksums and CRCs are verified before inventory. No accelerator supplies graph truth.
    /// Root discovery currently uses conservative retention policy, including pseudorefs; partial
    /// clone, precious-object and recent-object-hook repositories explicitly refuse completion.
    /// Findings do not stop other object checks. No repository data is changed.
    ///
    /// # Errors
    ///
    /// Returns storage inventory, root discovery, cancellation and aggregate resource failures.
    /// A result with findings is a completed scan, not proof of integrity.
    pub fn fsck_with(
        &self,
        options: &Options,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Report, Error> {
        use std::collections::BTreeMap;
        use std::sync::atomic::Ordering;

        let configured;
        let policy = if let Some(policy) = &options.policy {
            policy
        } else {
            let base = options
                .skip_list_base
                .as_deref()
                .or(self.worktree())
                .unwrap_or(self.git_dir());
            configured = Policy::from_config(
                self.config(),
                Scope::Repository,
                self.object_format(),
                base,
                PolicyLimits::default(),
            )?;
            &configured
        };
        let roots = self
            .integrity_roots(&options.roots, cancel)
            .map_err(Box::new)?;
        let objects = self.objects_controlled(
            options.roots.packs,
            crate::AlternateLimits::default(),
            cancel,
        )?;
        objects.verify_packs(cancel)?;
        if let Some(graph) =
            crate::commit_graph::CommitGraph::open(self.object_dir(), self.object_format())
                .map_err(|error| Error::Accelerator(Box::new(error.into())))?
        {
            graph
                .verify_with(&objects, options.read)
                .map_err(|error| Error::Accelerator(Box::new(error.into())))?;
        }
        match self.verify_multi_pack_index() {
            Ok(()) | Err(crate::multi_pack_index::Error::MissingIndex) => {}
            Err(error) => return Err(Error::Accelerator(Box::new(error.into()))),
        }
        let inventory = objects.object_ids_controlled(options.listing, cancel)?;
        let mut report = Report {
            findings: Vec::new(),
            reachable: BTreeSet::new(),
            unreachable: BTreeSet::new(),
            dangling: BTreeSet::new(),
        };
        let mut graph = BTreeMap::new();
        let mut edge_count = 0usize;
        let mut bytes = 0u64;
        for id in inventory {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let object = match objects.read_controlled(id, options.read, cancel) {
                Ok(Some(object)) => object,
                Ok(None) => {
                    report.findings.push(ObjectFinding::Missing { id });
                    continue;
                }
                Err(source) => {
                    report.findings.push(ObjectFinding::Read {
                        id,
                        source: Box::new(source),
                    });
                    continue;
                }
            };
            bytes = bytes
                .checked_add(object.data().len() as u64)
                .ok_or(Error::Limit("decoded bytes"))?;
            if bytes > options.roots.max_bytes {
                return Err(Error::Limit("decoded bytes"));
            }
            if !options.connectivity_only {
                report.findings.extend(
                    policy
                        .check_object(id, object.kind(), object.data(), options.strict)
                        .into_iter()
                        .map(|finding| ObjectFinding::Payload { id, finding }),
                );
            }
            let mut edges = Vec::new();
            let shallow = object.kind() == ObjectKind::Commit && self.shallow_roots().contains(id);
            let parsed: Result<(), crate::edges::Error> =
                crate::edges::visit(id, object.kind(), object.data(), |target, kind| {
                    if !(shallow && kind == ObjectKind::Commit) {
                        edges.push((target, kind));
                    }
                    Ok(())
                });
            if parsed.is_err() && options.connectivity_only {
                let message_id = match object.kind() {
                    ObjectKind::Tree => MessageId::BadTree,
                    ObjectKind::Tag => MessageId::BadTagObject,
                    _ => MessageId::BadCommit,
                };
                report.findings.push(ObjectFinding::Payload {
                    id,
                    finding: Finding {
                        message_id,
                        severity: Severity::Error,
                    },
                });
            }
            edge_count = edge_count
                .checked_add(edges.len())
                .ok_or(Error::Limit("graph edges"))?;
            if edge_count > options.roots.max_edges {
                return Err(Error::Limit("graph edges"));
            }
            graph.insert(id, (object.kind(), edges));
        }
        let mut missing: BTreeSet<_> = report
            .findings
            .iter()
            .filter_map(|finding| match finding {
                ObjectFinding::Read { id, .. } | ObjectFinding::Missing { id } => Some(*id),
                _ => None,
            })
            .collect();
        let mut referenced = BTreeSet::new();
        for (_, edges) in graph.values() {
            for &(id, expected) in edges {
                referenced.insert(id);
                match graph.get(&id) {
                    Some((actual, _)) if *actual != expected => {
                        report.findings.push(ObjectFinding::WrongKind {
                            id,
                            expected,
                            actual: *actual,
                        });
                    }
                    None if missing.insert(id) => {
                        report.findings.push(ObjectFinding::Missing { id })
                    }
                    _ => {}
                }
            }
        }
        let mut pending: Vec<_> = roots.into_iter().collect();
        while let Some(id) = pending.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let Some((_, edges)) = graph.get(&id) else {
                if missing.insert(id) {
                    report.findings.push(ObjectFinding::Missing { id });
                }
                continue;
            };
            if report.reachable.insert(id) {
                pending.extend(edges.iter().map(|&(id, _)| id));
            }
        }
        report.unreachable.extend(
            graph
                .keys()
                .filter(|id| !report.reachable.contains(*id))
                .copied(),
        );
        report
            .dangling
            .extend(report.unreachable.difference(&referenced).copied());
        Ok(report)
    }
}
