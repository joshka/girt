use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use super::{LogRecord, LogValue, RefRecord, Snapshot, StackLimits, Table, stack};
use crate::refs::store::{Lock, check_expected, conflicts};
use crate::refs::{
    Expected, LogOutcome, RefEdit, RefEditOutcome, RefName, RefOutcome, Reference, ReferenceError,
    References, Reflog, ReflogEntry, Target, TransactionError,
};
use crate::{ObjectId, Signature};

impl References<'_> {
    /// Applies a conditional batch with cooperative cancellation at stack boundaries.
    ///
    /// Reftable checks cancellation between opening/decoding tables and before each stack
    /// publication. Linked-worktree transactions can report already published common/private
    /// effects. Files-backend cancellation is checked only before the existing transaction starts.
    ///
    /// # Errors
    ///
    /// Returns [`TransactionError::Prepare`] before publication or [`TransactionError::Publish`]
    /// with exact completed effects. Never retry a partial result without inspecting current state.
    pub fn transaction_controlled(
        &self,
        edits: &[RefEdit],
        cancel: &AtomicBool,
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        stack::cancelled(cancel).map_err(|source| TransactionError::Prepare {
            operation: None,
            source,
        })?;
        if edits.is_empty() {
            return Ok(Vec::new());
        }
        if self.repository.reference_backend() == crate::refs::Backend::Reftable {
            prepare_controlled(self, edits, cancel)?.publish_controlled(cancel)
        } else {
            self.transaction(edits)
        }
    }

    /// Selects reftable stack budgets for subsequent operations on this handle.
    ///
    /// Files-backend operations are unaffected. Budgets apply separately to the common and private
    /// worktree stack; a linked-worktree operation may retain both bounded snapshots at once.
    pub fn with_reftable_limits(mut self, limits: StackLimits) -> Self {
        self.reftable_limits = limits;
        self
    }
}

fn directory(refs: &References<'_>, name: &RefName) -> PathBuf {
    let root = if name.per_worktree() {
        refs.repository.git_dir()
    } else {
        refs.repository.common_dir()
    };
    root.join("reftable")
}

fn snapshot(refs: &References<'_>, root: &std::path::Path) -> Result<Snapshot, ReferenceError> {
    Snapshot::read(
        root,
        refs.repository.object_format(),
        refs.reftable_limits,
        &AtomicBool::new(false),
    )
}

pub(crate) fn read(
    refs: &References<'_>,
    name: &RefName,
) -> Result<Option<Target>, ReferenceError> {
    let snapshot = snapshot(refs, &directory(refs, name))?;
    Ok(snapshot
        .table
        .references
        .into_iter()
        .find(|record| record.name == *name)
        .and_then(|record| record.target))
}

pub(crate) fn list(
    refs: &References<'_>,
    namespace: Option<&RefName>,
) -> Result<Vec<Reference>, ReferenceError> {
    let mut entries = BTreeMap::new();
    for root in directories(refs) {
        for record in snapshot(refs, &root)?.table.references {
            let Some(name) = record.name.reference_name() else {
                continue;
            };
            if record.name.as_bytes() == b"HEAD" || directory(refs, &name) != root {
                continue;
            }
            if namespace.is_some_and(|prefix| {
                record.name != *prefix
                    && !record
                        .name
                        .as_bytes()
                        .strip_prefix(prefix.as_bytes())
                        .is_some_and(|rest| rest.starts_with(b"/"))
            }) {
                continue;
            }
            if let Some(target) = record.target {
                entries.insert(name, target);
            }
        }
    }
    Ok(entries
        .into_iter()
        .map(|(name, target)| Reference { name, target })
        .collect())
}

pub(crate) fn reflog(
    refs: &References<'_>,
    name: &RefName,
) -> Result<Option<Vec<ReflogEntry>>, ReferenceError> {
    let snapshot = snapshot(refs, &directory(refs, name))?;
    let mut entries = Vec::new();
    for record in snapshot
        .table
        .logs
        .into_iter()
        .rev()
        .filter(|record| record.name == *name)
    {
        if let Some(value) = record.value {
            let seconds = i64::try_from(value.seconds).map_err(|_| {
                ReferenceError::Unsupported(
                    "binary reflog timestamp exceeds signed interpretation; use reftable Snapshot",
                )
            })?;
            let committer = Signature {
                name: value.name,
                email: value.email,
                seconds,
                offset_minutes: value.offset_minutes,
            };
            let message = value
                .message
                .strip_suffix(b"\n")
                .unwrap_or(&value.message)
                .to_vec();
            entries.push(ReflogEntry {
                old: value.old,
                new: value.new,
                committer,
                message,
            });
        }
    }
    Ok((!entries.is_empty()).then_some(entries))
}

pub(crate) fn single(
    refs: &References<'_>,
    name: &RefName,
    target: Option<Target>,
    expected: Expected,
    dereference: bool,
) -> Result<RefEditOutcome, ReferenceError> {
    let edit = RefEdit {
        name: name.clone(),
        dereference,
        target,
        expected,
        reflog: Reflog::Preserve,
    };
    let result = refs.transaction(&[edit]);
    match result {
        Ok(mut outcomes) => Ok(outcomes.remove(0)),
        // A no-reflog single edit publishes only one stack, so an error has no ref/log effects.
        Err(
            TransactionError::Prepare { source, .. } | TransactionError::Publish { source, .. },
        ) => Err(source),
    }
}

fn directories(refs: &References<'_>) -> BTreeSet<PathBuf> {
    [
        refs.repository.common_dir().join("reftable"),
        refs.repository.git_dir().join("reftable"),
    ]
    .into_iter()
    .collect()
}

struct Group {
    root: PathBuf,
    lock: Lock,
    snapshot: Snapshot,
    table: Table,
    bytes: Vec<u8>,
    logs: BTreeMap<RefName, LogOutcome>,
}

pub(crate) struct Prepared {
    groups: Vec<Group>,
    outcomes: Vec<RefEditOutcome>,
    limits: StackLimits,
}

pub(crate) fn prepare(
    refs: &References<'_>,
    edits: &[RefEdit],
) -> Result<Prepared, TransactionError> {
    prepare_controlled(refs, edits, &AtomicBool::new(false))
}

fn prepare_controlled(
    refs: &References<'_>,
    edits: &[RefEdit],
    cancel: &AtomicBool,
) -> Result<Prepared, TransactionError> {
    let batch = |source| TransactionError::Prepare {
        operation: None,
        source,
    };
    for (index, edit) in edits.iter().enumerate() {
        crate::refs::transaction::validate_edit(refs.repository.object_format(), edit).map_err(
            |source| TransactionError::Prepare {
                operation: Some(index),
                source,
            },
        )?;
    }
    let mut groups = Vec::new();
    // Lock both roots in deterministic path order, even when only the symbolic chain crosses roots.
    for root in directories(refs) {
        stack::cancelled(cancel).map_err(batch)?;
        let lock = Lock::acquire(root.join("tables.list")).map_err(batch)?;
        let snapshot = Snapshot::read(
            &root,
            refs.repository.object_format(),
            refs.reftable_limits,
            cancel,
        )
        .map_err(batch)?;
        let next = snapshot
            .table
            .max_update_index
            .checked_add(1)
            .ok_or_else(|| batch(super::Error::Limit("update index exhausted").into()))?;
        let table = Table {
            format: refs.repository.object_format(),
            min_update_index: next,
            max_update_index: next,
            references: Vec::new(),
            logs: Vec::new(),
        };
        groups.push(Group {
            root,
            lock,
            snapshot,
            table,
            bytes: Vec::new(),
            logs: BTreeMap::new(),
        });
    }
    let mut values = BTreeMap::new();
    for group in &groups {
        for record in &group.snapshot.table.references {
            let Some(name) = record.name.reference_name() else {
                continue;
            };
            if directory(refs, &name) == group.root
                && let Some(target) = &record.target
            {
                values.insert(name, target.clone());
            }
        }
    }
    let mut used = BTreeSet::new();
    let mut outcomes = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let error = |source| TransactionError::Prepare {
            operation: Some(index),
            source,
        };
        let chain = if edit.dereference || matches!(edit.reflog, Reflog::Append { .. }) {
            resolve(&values, &edit.name).map_err(error)?
        } else {
            vec![edit.name.clone()]
        };
        let name = if edit.dereference {
            chain.last().unwrap().clone()
        } else {
            edit.name.clone()
        };
        check_expected(values.get(&name).cloned(), edit.expected.clone()).map_err(error)?;
        for dependency in &chain {
            if !used.insert(dependency.clone()) {
                return Err(error(ReferenceError::Unsupported(
                    "overlapping reference operations",
                )));
            }
        }
        if values
            .keys()
            .chain(used.iter())
            .any(|other| conflicts(name.as_bytes(), other.as_bytes()))
        {
            return Err(error(ReferenceError::Conflict(directory(refs, &name))));
        }
        let old = terminal_id(&values, &chain, refs.repository.object_format());
        let new = match &edit.target {
            Some(Target::Direct(id)) => *id,
            None => ObjectId::null(refs.repository.object_format()),
            Some(Target::Symbolic(target)) if matches!(edit.reflog, Reflog::Append { .. }) => {
                let target_chain = resolve(&values, target).map_err(error)?;
                for dependency in &target_chain {
                    if !chain.contains(dependency) && !used.insert(dependency.clone()) {
                        return Err(error(ReferenceError::Unsupported(
                            "overlapping symbolic dependencies",
                        )));
                    }
                }
                terminal_id(&values, &target_chain, refs.repository.object_format())
            }
            Some(Target::Symbolic(_)) => ObjectId::null(refs.repository.object_format()),
        };
        let group = group_mut(&mut groups, &directory(refs, &name));
        group.table.references.push(RefRecord {
            name: name.clone().into(),
            update_index: group.table.max_update_index,
            target: edit.target.clone(),
            peeled: None,
        });
        let log_names = match &edit.reflog {
            Reflog::Preserve => Vec::new(),
            Reflog::Delete => vec![name.clone()],
            Reflog::Append { .. } if edit.dereference => chain,
            Reflog::Append { .. } => vec![name.clone()],
        };
        for log_name in &log_names {
            let group = group_mut(&mut groups, &directory(refs, log_name));
            match &edit.reflog {
                Reflog::Append { committer, message } => {
                    group.table.logs.push(LogRecord {
                        name: log_name.clone().into(),
                        update_index: group.table.max_update_index,
                        value: Some(LogValue {
                            old,
                            new,
                            name: committer.name.clone(),
                            email: committer.email.clone(),
                            seconds: committer.seconds as u64,
                            offset_minutes: committer.offset_minutes,
                            message: message.iter().copied().chain(*b"\n").collect(),
                        }),
                    });
                    group.logs.insert(log_name.clone(), LogOutcome::Appended);
                }
                Reflog::Delete => {
                    for record in &group.snapshot.table.logs {
                        if record.name == *log_name && record.value.is_some() {
                            group.table.logs.push(LogRecord {
                                name: log_name.clone().into(),
                                update_index: record.update_index,
                                value: None,
                            });
                        }
                    }
                    group.logs.insert(log_name.clone(), LogOutcome::Deleted);
                }
                Reflog::Preserve => {}
            }
        }
        outcomes.push(RefEditOutcome {
            name,
            reference: RefOutcome::Unchanged,
            logs: log_names
                .into_iter()
                .map(|name| (name, LogOutcome::NotAttempted))
                .collect(),
        });
    }
    for group in &mut groups {
        group.table.references.sort_by(|a, b| a.name.cmp(&b.name));
        group.table.logs.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| b.update_index.cmp(&a.update_index))
        });
        if !group.table.references.is_empty() || !group.table.logs.is_empty() {
            group.bytes = group
                .table
                .encode(refs.reftable_limits.records)
                .map_err(|error| batch(error.into()))?;
            let limits = refs.reftable_limits;
            let (_, records, decoded) = Table::decode_usage(&group.bytes, limits.records)
                .map_err(|error| batch(error.into()))?;
            if group.snapshot.names.len() >= limits.tables
                || group.bytes.len()
                    > limits
                        .records
                        .bytes
                        .saturating_sub(group.snapshot.used_bytes)
                || records
                    > limits
                        .records
                        .records
                        .saturating_sub(group.snapshot.used_records)
                || decoded
                    > limits
                        .records
                        .decoded_bytes
                        .saturating_sub(group.snapshot.used_decoded)
                || group
                    .snapshot
                    .names
                    .iter()
                    .map(|name| name.len() + 1)
                    .sum::<usize>()
                    + 128
                    > limits.list_bytes
            {
                return Err(batch(
                    super::Error::Limit("resulting stack budget; compact or raise limits").into(),
                ));
            }
        }
    }
    Ok(Prepared {
        groups,
        outcomes,
        limits: refs.reftable_limits,
    })
}

fn group_mut<'a>(groups: &'a mut [Group], root: &std::path::Path) -> &'a mut Group {
    groups.iter_mut().find(|group| group.root == root).unwrap()
}

fn resolve(
    values: &BTreeMap<RefName, Target>,
    name: &RefName,
) -> Result<Vec<RefName>, ReferenceError> {
    let mut chain = Vec::new();
    let mut current = name.clone();
    loop {
        if chain.contains(&current) {
            return Err(ReferenceError::Cycle(current));
        }
        chain.push(current.clone());
        match values.get(&current) {
            Some(Target::Symbolic(next)) => {
                if chain.len() > 32 {
                    return Err(ReferenceError::Depth(32));
                }
                current = next.clone();
            }
            _ => return Ok(chain),
        }
    }
}

fn terminal_id(
    values: &BTreeMap<RefName, Target>,
    chain: &[RefName],
    format: crate::ObjectFormat,
) -> ObjectId {
    match values.get(chain.last().unwrap()) {
        Some(Target::Direct(id)) => *id,
        _ => ObjectId::null(format),
    }
}

impl Prepared {
    pub(crate) fn publish(self) -> Result<Vec<RefEditOutcome>, TransactionError> {
        self.publish_controlled(&AtomicBool::new(false))
    }

    fn publish_controlled(
        self,
        cancel: &AtomicBool,
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        self.publish_with(cancel, || {})
    }

    fn publish_with(
        mut self,
        cancel: &AtomicBool,
        mut after_group: impl FnMut(),
    ) -> Result<Vec<RefEditOutcome>, TransactionError> {
        for group in &self.groups {
            if let Err(source) = stack::cancelled(cancel) {
                return Err(TransactionError::Publish {
                    outcomes: self.outcomes,
                    source,
                });
            }
            if !group.bytes.is_empty()
                && let Err(source) = stack::publish(
                    &group.lock,
                    &group.snapshot.names,
                    &group.table,
                    &group.bytes,
                    self.limits,
                )
            {
                return Err(TransactionError::Publish {
                    outcomes: self.outcomes,
                    source,
                });
            }
            for outcome in &mut self.outcomes {
                if group
                    .table
                    .references
                    .iter()
                    .any(|record| record.name == outcome.name)
                {
                    outcome.reference = RefOutcome::Published;
                }
                for (name, result) in &mut outcome.logs {
                    if let Some(published) = group.logs.get(name) {
                        *result = published.clone();
                    }
                }
            }
            after_group();
        }
        Ok(self.outcomes)
    }
}

#[cfg(test)]
mod tests {
    use super::super::decode_tests::{fixture, git};
    use super::*;

    fn edit() -> RefEdit {
        RefEdit {
            name: RefName::new(b"HEAD").unwrap(),
            dereference: true,
            target: None,
            expected: Expected::Exists,
            reflog: Reflog::Append {
                committer: Signature {
                    name: b"Fixture".to_vec(),
                    email: b"a@b".to_vec(),
                    seconds: 1,
                    offset_minutes: 0,
                },
                message: b"delete".to_vec(),
            },
        }
    }

    #[test]
    fn prepared_transaction_excludes_git_writer() {
        let root = fixture("sha1");
        let repo = crate::Repository::open(root.path()).unwrap();
        let refs = repo.references().unwrap();
        let prepared = prepare(&refs, &[edit()]).unwrap();
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(["update-ref", "-d", "refs/heads/main"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            refs.resolve(&RefName::new(b"HEAD").unwrap(), 32)
                .unwrap()
                .id
                .is_some()
        );
        prepared.publish().unwrap();
        assert!(
            refs.resolve(&RefName::new(b"HEAD").unwrap(), 32)
                .unwrap()
                .id
                .is_none()
        );
    }

    #[test]
    fn cancellation_between_worktree_stacks_reports_published_branch() {
        let root = fixture("sha1");
        let linked = root.path().join("linked");
        git(
            root.path(),
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        let repo = crate::Repository::open(&linked).unwrap();
        let refs = repo.references().unwrap();
        let head = RefName::new(b"HEAD").unwrap();
        let before = refs.reflog(&head).unwrap();
        let prepared = prepare(&refs, &[edit()]).unwrap();
        let cancel = AtomicBool::new(false);
        let Err(TransactionError::Publish {
            outcomes,
            source: ReferenceError::Cancelled,
        }) = prepared.publish_with(&cancel, || {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed)
        })
        else {
            panic!("expected partial cancellation");
        };
        assert_eq!(outcomes[0].reference, RefOutcome::Published);
        assert_eq!(outcomes[0].logs[0].1, LogOutcome::NotAttempted);
        assert_eq!(outcomes[0].logs[1].1, LogOutcome::Appended);
        assert_eq!(refs.reflog(&head).unwrap(), before);
        assert!(refs.resolve(&head, 32).unwrap().id.is_none());
    }
}
