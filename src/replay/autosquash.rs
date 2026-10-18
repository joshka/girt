use std::collections::{BTreeMap, BTreeSet};

use super::TodoRun;
use super::todo::{CommitToken, Instruction, MessageMode, Todo, TodoLine};
use crate::{ObjectId, Objects};

/// Autosquash planning failed without changing objects or references.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AutosquashError {
    /// Input count exceeded the explicit limit.
    #[error("autosquash input exceeds configured bound")]
    Limit,
    /// An input commit could not be read.
    #[error("reading autosquash commit")]
    Read(#[from] crate::ReadCommitError),
}

impl Objects {
    /// Creates a linear todo with Git's `fixup!`, `squash!` and `amend!` subjects reordered.
    ///
    /// Targets must precede the marked input in the supplied sequence. Exact subject or identity
    /// prefix matches take precedence over subject-prefix matches. Unmatched markers remain picks.
    /// Fixups attach after the target's existing fixup group in input order; `amend!` selects fixup
    /// message replacement. The caller executes or edits the returned plan separately.
    ///
    /// # Errors
    ///
    /// Returns [`AutosquashError`] for the explicit count bound or unreadable inputs.
    pub fn autosquash(
        &self,
        commits: &[ObjectId],
        max_commits: usize,
    ) -> Result<Todo, AutosquashError> {
        if commits.len() > max_commits {
            return Err(AutosquashError::Limit);
        }
        let subjects: Vec<_> = commits
            .iter()
            .map(|&id| self.read_commit(id).map(|commit| commit.subject()))
            .collect::<Result<_, _>>()?;
        let mut groups: Vec<Vec<TodoLine>> = Vec::new();
        let mut owners = Vec::new();
        for (index, (&id, subject)) in commits.iter().zip(&subjects).enumerate() {
            let token = CommitToken {
                id: id.to_string().into_bytes(),
                subject: subject.clone(),
            };
            let marker = subject
                .strip_prefix(b"fixup! ")
                .map(|target| (target, MessageMode::Default, false))
                .or_else(|| {
                    subject
                        .strip_prefix(b"squash! ")
                        .map(|target| (target, MessageMode::Default, true))
                })
                .or_else(|| {
                    subject
                        .strip_prefix(b"amend! ")
                        .map(|target| (target, MessageMode::Replace, false))
                });
            let target = marker.and_then(|(target, mode, squash)| {
                find_target(target, &commits[..index], &subjects[..index])
                    .map(|target| (target, mode, squash))
            });
            if let Some((target, mode, squash)) = target {
                let owner: usize = owners[target];
                let instruction = if squash {
                    Instruction::Squash(token)
                } else {
                    Instruction::Fixup(token, mode)
                };
                groups[owner].push(TodoLine {
                    line: index + 1,
                    instruction,
                });
                owners.push(owner);
            } else {
                owners.push(groups.len());
                groups.push(vec![TodoLine {
                    line: index + 1,
                    instruction: Instruction::Pick(token),
                }]);
            }
        }
        Ok(Todo {
            lines: groups.into_iter().flatten().collect(),
        })
    }
}

fn find_target(target: &[u8], commits: &[ObjectId], subjects: &[Vec<u8>]) -> Option<usize> {
    subjects
        .iter()
        .enumerate()
        .find_map(|(index, subject)| {
            let hex = commits[index].to_string();
            let identity = target.len() >= 4
                && target.len() <= hex.len()
                && hex.as_bytes().starts_with(target);
            (subject == target || identity).then_some(index)
        })
        .or_else(|| {
            subjects
                .iter()
                .position(|subject| subject.starts_with(target))
        })
}

impl TodoRun {
    /// Plans ref targets for rewritten inputs, excluding caller-supplied checked-out branch names.
    ///
    /// Supply an observed name-to-old-identity snapshot and every branch checked out in any working
    /// tree. The returned map omits excluded names and unchanged/unmapped tips. Validate the old
    /// identities again in a separate reference transaction; this method changes no references.
    ///
    /// # Errors
    ///
    /// Returns an invalid-name error if any supplied name is outside Git's full reference syntax.
    pub fn plan_updates(
        &self,
        observed: &BTreeMap<Vec<u8>, ObjectId>,
        checked_out: &BTreeSet<Vec<u8>>,
    ) -> Result<BTreeMap<Vec<u8>, ObjectId>, crate::refs::InvalidRefName> {
        let mut updates = BTreeMap::new();
        for (name, old) in observed {
            crate::refs::RefName::new(name)?;
            if checked_out.contains(name) {
                continue;
            }
            if let Some(&new) = self.rewritten.get(old) {
                if new != *old {
                    updates.insert(name.clone(), new);
                }
            }
        }
        Ok(updates)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::exact(b"second", Some(1))]
    #[case::prefix(b"sec", Some(1))]
    #[case::missing(b"missing", None)]
    fn selects_original_subject(#[case] target: &[u8], #[case] expected: Option<usize>) {
        let ids = [
            "1111111111111111111111111111111111111111".parse().unwrap(),
            "2222222222222222222222222222222222222222".parse().unwrap(),
        ];
        let subjects = vec![b"first".to_vec(), b"second".to_vec()];
        assert_eq!(find_target(target, &ids, &subjects), expected);
    }
}
