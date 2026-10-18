use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::todo::{CommitToken, Instruction, MessageMode, Todo};
use super::{EmptyPolicy, Operation, Options, Replay, Status};
use crate::{Commit, ObjectId, ObjectIdPrefix, Objects, Signature};

/// Caller-owned interaction for todo messages and external commands.
///
/// Hooks may have side effects. The executor does not launch processes or change repository state
/// itself. A hook error stops execution; any earlier hook effects remain the caller's
/// responsibility.
pub trait TodoHooks {
    /// Supplies per-path content policy for this step; default uses built-in text/binary policy.
    /// Attribute snapshots and any custom-driver side effects remain caller-owned.
    fn content_driver(&mut self) -> Option<&mut dyn crate::merge::TreeDriver> {
        None
    }

    /// Edits a proposed message for reword, squash or fixup `-c`.
    fn message(&mut self, instruction: &Instruction, proposed: &[u8])
    -> Result<Vec<u8>, HookError>;
    /// Executes an explicit command against a proposed tip, which may exist only in memory.
    fn exec(&mut self, command: &[u8], tip: &Commit) -> Result<(), HookError>;
}

/// Original caller hook failure, retained as a source.
pub type HookError = Box<dyn std::error::Error + Send + Sync>;

/// A todo stop requiring caller action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TodoStop {
    /// Conflict or empty decision at this zero-based instruction index; inspect `replays.last()`.
    Replay(usize),
    /// An edit instruction completed; resume at the following zero-based index.
    Edit(usize),
    /// A break instruction completed; resume at the following zero-based index.
    Break(usize),
}

/// Computed todo progress and caller-owned publication intents.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TodoRun {
    /// Last completed proposed commit; may not exist in the object store.
    pub tip: Commit,
    /// Original single-step proposals in order, including any stopped replay.
    /// Squash/reword final metadata is in `commits`, rather than these unamended proposals.
    pub replays: Vec<Replay>,
    /// Proposed commits in creation order, including commits replaced by squash/fixup.
    pub commits: Vec<Commit>,
    /// Original input to final rewritten identity; squash/fixup rewrites earlier mappings too.
    pub rewritten: BTreeMap<ObjectId, ObjectId>,
    /// Explicit update-ref targets from the todo. No ref is changed.
    pub updates: BTreeMap<Vec<u8>, ObjectId>,
    /// Caller action required, or `None` when all instructions completed.
    pub stopped: Option<TodoStop>,
}

/// Todo execution failed without implicit repository publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TodoExecutionError {
    /// Recreated merge topology is parsed but not executed by the linear engine.
    #[error("todo merge execution requires generated-history merge-base support")]
    MergeUnsupported,
    /// Reset topology requires generated-history support.
    #[error("todo reset execution requires generated-history support")]
    ResetUnsupported,
    /// A fixup or squash has no preceding replay commit.
    #[error("todo fixup or squash has no preceding commit")]
    NoPrevious,
    /// Label has not been defined or was defined twice.
    #[error("todo label is unknown or duplicated")]
    Label(Vec<u8>),
    /// Reference update target is malformed.
    #[error("invalid todo reference name")]
    Reference(#[from] crate::refs::InvalidRefName),
    /// Commit token cannot be parsed in the repository object format.
    #[error("invalid todo object prefix")]
    Prefix(#[from] crate::ParseObjectIdPrefixError),
    /// Commit prefix is absent, ambiguous or unreadable.
    #[error("resolving todo commit")]
    Resolve(#[from] crate::objects::ResolvePrefixError),
    /// Reading the original source or destination failed.
    #[error("reading todo commit")]
    Read(#[from] crate::ReadCommitError),
    /// Single-commit replay failed.
    #[error("replaying todo commit")]
    Replay(#[from] super::Error),
    /// Proposed amended metadata is invalid.
    #[error("constructing todo commit")]
    Commit(#[from] crate::CommitError),
    /// Caller hook failed; earlier hook side effects may remain.
    #[error("todo caller hook failed")]
    Hook(#[source] HookError),
    /// Caller cancelled between instructions.
    #[error("todo execution cancelled")]
    Cancelled,
}

impl Objects {
    /// Executes a linear todo plan in memory, with explicit caller message and command hooks.
    ///
    /// Supports pick/reword/edit/squash/fixup/drop/exec/break/label/reset/update-ref. Merge
    /// commands are refused before any hook runs because this engine cannot resolve generated
    /// ancestry for recursive merge bases. Labels retain proposed commits, including unstored
    /// commits. Fixup and squash replace the preceding commit's tree/message while preserving
    /// its author and parents. A reset begins a new squash group. Drop records no rewrite
    /// mapping.
    ///
    /// No object, index, working file, state file or reference is installed by the executor. Hooks
    /// are explicitly caller-owned and may have side effects. For a stop, callers retain this
    /// result; resuming a complex edited plan requires a caller-owned continuation context.
    ///
    /// # Errors
    ///
    /// Returns [`TodoExecutionError`] for unsupported merges, unknown labels, missing predecessors,
    /// invalid objects, replay/metadata failures, cancellation or a caller hook failure.
    pub fn execute_todo(
        &self,
        todo: &Todo,
        onto: ObjectId,
        committer: Signature,
        options: Options,
        cancel: &AtomicBool,
        hooks: &mut dyn TodoHooks,
    ) -> Result<TodoRun, TodoExecutionError> {
        if todo
            .lines()
            .iter()
            .any(|line| matches!(line.instruction, Instruction::Merge { .. }))
        {
            return Err(TodoExecutionError::MergeUnsupported);
        }
        if todo
            .lines()
            .iter()
            .any(|line| matches!(line.instruction, Instruction::Reset(_)))
        {
            return Err(TodoExecutionError::ResetUnsupported);
        }
        for line in todo.lines() {
            if let Instruction::UpdateRef(name) = &line.instruction {
                crate::refs::RefName::new(name)?;
            }
        }
        let mut run = TodoRun {
            tip: self.read_commit(onto)?,
            replays: vec![],
            commits: vec![],
            rewritten: BTreeMap::new(),
            updates: BTreeMap::new(),
            stopped: None,
        };
        let mut labels = std::collections::BTreeSet::new();
        let mut memory = crate::merge::ReplayTrees::default();
        let mut group = Vec::new();
        for (position, line) in todo.lines().iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(TodoExecutionError::Cancelled);
            }
            let instruction = &line.instruction;
            match instruction {
                Instruction::Pick(token)
                | Instruction::Revert(token)
                | Instruction::Reword(token)
                | Instruction::Edit(token)
                | Instruction::Squash(token)
                | Instruction::Fixup(token, _) => {
                    let source = resolve(self, token)?;
                    let original = self.read_commit(source)?;
                    let combine = matches!(
                        instruction,
                        Instruction::Squash(_) | Instruction::Fixup(_, _)
                    );
                    if combine && group.is_empty() {
                        return Err(TodoExecutionError::NoPrevious);
                    }
                    let mut policy = options;
                    if combine {
                        policy.originally_empty = EmptyPolicy::Keep;
                        policy.redundant = EmptyPolicy::Keep;
                    }
                    let operation = Operation {
                        source,
                        original: &original,
                        onto: &run.tip,
                        committer: committer.clone(),
                        options: policy,
                        reverse: matches!(instruction, Instruction::Revert(_)),
                    };
                    let replay =
                        operation.compute(self, cancel, &mut memory, hooks.content_driver())?;
                    if matches!(replay.status, Status::Conflicted | Status::Empty(_)) {
                        run.replays.push(replay);
                        run.stopped = Some(TodoStop::Replay(position));
                        return Ok(run);
                    }
                    if let Some(commit) = &replay.commit {
                        let mut fields = commit.to_fields()?;
                        if combine {
                            let previous = run.tip.to_fields()?;
                            fields.parents = previous.parents;
                            fields.author = previous.author;
                            fields.message = match instruction {
                                Instruction::Squash(_) => {
                                    let mut combined = previous.message;
                                    while combined.last() == Some(&b'\n') {
                                        combined.pop();
                                    }
                                    combined.extend_from_slice(b"\n\n");
                                    combined.extend_from_slice(original.message());
                                    hooks
                                        .message(instruction, &combined)
                                        .map_err(TodoExecutionError::Hook)?
                                }
                                Instruction::Fixup(_, MessageMode::Replace) => {
                                    fixup_message(&original).to_vec()
                                }
                                Instruction::Fixup(_, MessageMode::Edit) => hooks
                                    .message(instruction, fixup_message(&original))
                                    .map_err(TodoExecutionError::Hook)?,
                                _ => previous.message,
                            };
                        } else {
                            group.clear();
                            if matches!(instruction, Instruction::Reword(_)) {
                                fields.message = hooks
                                    .message(instruction, &fields.message)
                                    .map_err(TodoExecutionError::Hook)?;
                            }
                        }
                        let commit = Commit::new(fields)?;
                        if combine {
                            let previous = run.tip.id();
                            for target in run.updates.values_mut() {
                                if *target == previous {
                                    *target = commit.id();
                                }
                            }
                        }
                        run.tip = commit.clone();
                        run.commits.push(commit.clone());
                        group.push(source);
                        for &input in &group {
                            run.rewritten.insert(input, run.tip.id());
                        }
                    } else {
                        run.rewritten.insert(source, run.tip.id());
                        if !group.is_empty() {
                            group.push(source);
                        }
                    }
                    run.replays.push(replay);
                    if matches!(instruction, Instruction::Edit(_)) {
                        run.stopped = Some(TodoStop::Edit(position + 1));
                        return Ok(run);
                    }
                }
                Instruction::Drop(_) => {}
                Instruction::Exec(command) => hooks
                    .exec(command, &run.tip)
                    .map_err(TodoExecutionError::Hook)?,
                Instruction::Break => {
                    run.stopped = Some(TodoStop::Break(position + 1));
                    return Ok(run);
                }
                Instruction::Label(name) => {
                    if !labels.insert(name.clone()) {
                        return Err(TodoExecutionError::Label(name.clone()));
                    }
                }
                Instruction::Reset(_) => unreachable!("reset preflight"),
                Instruction::UpdateRef(name) => {
                    crate::refs::RefName::new(name)?;
                    run.updates.insert(name.clone(), run.tip.id());
                }
                Instruction::Merge { .. } => unreachable!("merge preflight"),
            }
        }
        Ok(run)
    }
}

fn resolve(objects: &Objects, token: &CommitToken) -> Result<ObjectId, TodoExecutionError> {
    // Parser already checked ASCII hex; conversion cannot lose bytes.
    let text = std::str::from_utf8(&token.id).expect("validated hex token");
    let prefix = ObjectIdPrefix::from_hex(objects.object_format(), text)?;
    Ok(objects.resolve_prefix(prefix)?)
}

fn fixup_message(commit: &Commit) -> &[u8] {
    if commit.message().starts_with(b"amend! ") {
        commit
            .message()
            .windows(2)
            .position(|bytes| bytes == b"\n\n")
            .map(|position| &commit.message()[position + 2..])
            .unwrap_or_default()
    } else {
        commit.message()
    }
}
