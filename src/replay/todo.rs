//! Git interactive-rebase instructions, parsed without executing commands or resolving objects.
//!
//! ```
//! use girt::replay::todo::{Instruction, Todo, TodoLimits};
//! let todo = Todo::parse(
//!     b"pick abcd first\nfixup -C bcde replacement\n",
//!     TodoLimits::default(),
//! )?;
//! assert_eq!(todo.lines().len(), 2);
//! assert!(matches!(
//!     todo.lines()[1].instruction,
//!     Instruction::Fixup(_, _)
//! ));
//! # Ok::<(), girt::replay::todo::TodoError>(())
//! ```
//!
//! Parsing validates syntax and resource bounds. Execute supported linear plans separately through
//! [`crate::Objects::execute_todo`]; topology commands remain structured data for future engines.
use std::collections::BTreeSet;

/// A commit token and optional human-readable subject from a todo line.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct CommitToken {
    /// Hex object identity or prefix; resolve against the operation's object store before use.
    pub id: Vec<u8>,
    /// Opaque subject bytes, ignored for object selection.
    pub subject: Vec<u8>,
}

/// Message selection for fixup and merge instructions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum MessageMode {
    /// Retain the destination message for fixup; create a message for merge.
    #[default]
    Default,
    /// Use the named commit's message (`-C`).
    Replace,
    /// Ask the caller to edit the named commit's message (`-c`).
    Edit,
}

/// Parsed rebase instruction. No command is run by parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Instruction {
    /// Replay a commit.
    Pick(CommitToken),
    /// Invert a commit in Git cherry-pick/revert sequencer files.
    Revert(CommitToken),
    /// Replay with a caller-edited message.
    Reword(CommitToken),
    /// Replay and stop for caller inspection.
    Edit(CommitToken),
    /// Combine this change and message into the preceding commit.
    Squash(CommitToken),
    /// Combine this change into the preceding commit with the chosen message policy.
    Fixup(CommitToken, MessageMode),
    /// Omit a commit.
    Drop(CommitToken),
    /// Run opaque command bytes only through an explicit caller hook.
    Exec(Vec<u8>),
    /// Stop before the next instruction.
    Break,
    /// Name the current tip for later reset or merge.
    Label(Vec<u8>),
    /// Select a previously labeled tip.
    Reset(Vec<u8>),
    /// Recreate a merge of the current tip and labels, optionally taking message metadata.
    Merge {
        /// Source message token when `-C` or `-c` was present.
        commit: Option<Vec<u8>>,
        /// Source message policy.
        message: MessageMode,
        /// One or more label names in parent order.
        labels: Vec<Vec<u8>>,
        /// Optional text after `#`, retained as opaque bytes.
        subject: Vec<u8>,
    },
    /// Git's update-refs instruction; publication remains a separate caller operation.
    UpdateRef(Vec<u8>),
}

/// Parsed instruction with its original one-based line number.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct TodoLine {
    /// Original one-based line number, including preceding blank/comment lines.
    pub line: usize,
    /// Valid instruction syntax; object existence and label topology are unchecked.
    pub instruction: Instruction,
}

/// Resource bounds for todo parsing.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct TodoLimits {
    /// Maximum complete input size (default 8 MiB).
    pub max_bytes: usize,
    /// Maximum non-comment instruction count (default 100,000).
    pub max_instructions: usize,
}
impl Default for TodoLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_instructions: 100_000,
        }
    }
}

/// Ordered instructions from a Git interactive-rebase todo file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Todo {
    pub(super) lines: Vec<TodoLine>,
}

/// Invalid or bounded todo input; parsing has no external side effects.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TodoError {
    /// Input exceeded a configured size/count bound.
    #[error("todo input exceeds configured bound")]
    Limit,
    /// Invalid command or arguments at a one-based line.
    #[error("invalid todo instruction at line {line}: {reason}")]
    Syntax {
        /// Original one-based line number.
        line: usize,
        /// Stable diagnostic describing the malformed syntax.
        reason: &'static str,
    },
}

impl Todo {
    /// Parses long/short Git todo commands, preserving byte subjects and command payloads.
    ///
    /// Leading ASCII whitespace, blank lines and `#` comment lines are ignored. LF and CRLF
    /// endings are accepted. Commit tokens must be 4–64 hex bytes; full identity format and object
    /// existence are validated during execution. Comments are presentation and are not retained.
    /// Parsing validates syntax only: duplicate/unknown labels and squash placement require an
    /// execution context. External commands are never executed implicitly.
    ///
    /// # Errors
    ///
    /// Returns [`TodoError`] for invalid syntax, NUL bytes or exhausted bounds.
    pub fn parse(bytes: &[u8], limits: TodoLimits) -> Result<Self, TodoError> {
        if bytes.len() > limits.max_bytes {
            return Err(TodoError::Limit);
        }
        let mut lines = Vec::new();
        for (index, raw) in bytes.split(|&b| b == b'\n').enumerate() {
            let line = index + 1;
            let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            let raw = raw.trim_ascii_start();
            let invalid = |reason| TodoError::Syntax { line, reason };
            if raw.contains(&0) {
                return Err(invalid("NUL byte"));
            }
            if raw.is_empty() || raw.starts_with(b"#") {
                continue;
            }
            if lines.len() >= limits.max_instructions {
                return Err(TodoError::Limit);
            }
            let (command, args) = token(raw);
            let commit = || parse_commit(args).ok_or_else(|| invalid("expected commit token"));
            let instruction = match command {
                b"pick" | b"p" => Instruction::Pick(commit()?),
                b"revert" => Instruction::Revert(commit()?),
                b"reword" | b"r" => Instruction::Reword(commit()?),
                b"edit" | b"e" => Instruction::Edit(commit()?),
                b"squash" | b"s" => Instruction::Squash(commit()?),
                b"drop" | b"d" => Instruction::Drop(commit()?),
                b"fixup" | b"f" => {
                    let (first, rest) = token(args);
                    let (mode, payload) = match first {
                        b"-C" => (MessageMode::Replace, rest),
                        b"-c" => (MessageMode::Edit, rest),
                        _ => (MessageMode::Default, args),
                    };
                    Instruction::Fixup(
                        parse_commit(payload).ok_or_else(|| invalid("expected fixup commit"))?,
                        mode,
                    )
                }
                b"exec" | b"x" if !args.is_empty() => Instruction::Exec(args.to_vec()),
                b"break" | b"b" if trim(args).is_empty() => Instruction::Break,
                b"label" | b"l" if valid_label(trim(args)) => {
                    Instruction::Label(trim(args).to_vec())
                }
                b"reset" | b"t" if valid_label(trim(args)) => {
                    Instruction::Reset(trim(args).to_vec())
                }
                b"update-ref" | b"u" if valid_label(trim(args)) => {
                    Instruction::UpdateRef(trim(args).to_vec())
                }
                b"merge" | b"m" => {
                    parse_merge(args).ok_or_else(|| invalid("invalid merge arguments"))?
                }
                _ => return Err(invalid("unknown command or invalid arguments")),
            };
            lines.push(TodoLine { line, instruction });
        }
        Ok(Self { lines })
    }

    /// Borrows instructions in execution order, retaining original line locations.
    pub fn lines(&self) -> &[TodoLine] {
        &self.lines
    }

    /// Returns label names defined by this plan, without validating execution order.
    pub fn labels(&self) -> BTreeSet<&[u8]> {
        self.lines
            .iter()
            .filter_map(|line| match &line.instruction {
                Instruction::Label(name) => Some(name.as_slice()),
                _ => None,
            })
            .collect()
    }
}

fn trim(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}
fn token(bytes: &[u8]) -> (&[u8], &[u8]) {
    let bytes = bytes.trim_ascii_start();
    let end = bytes
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(bytes.len());
    (&bytes[..end], bytes[end..].trim_ascii_start())
}
fn valid_id(bytes: &[u8]) -> bool {
    (4..=64).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_hexdigit)
}
fn valid_label(bytes: &[u8]) -> bool {
    !bytes.is_empty() && !bytes.iter().any(u8::is_ascii_whitespace)
}
fn parse_commit(bytes: &[u8]) -> Option<CommitToken> {
    let (id, subject) = token(bytes);
    valid_id(id).then(|| CommitToken {
        id: id.to_vec(),
        subject: subject.to_vec(),
    })
}
fn parse_merge(args: &[u8]) -> Option<Instruction> {
    let (first, rest) = token(args);
    let (message, commit, labels) = match first {
        b"-C" | b"-c" => {
            let (id, labels) = token(rest);
            if !valid_id(id) {
                return None;
            }
            (
                if first == b"-C" {
                    MessageMode::Replace
                } else {
                    MessageMode::Edit
                },
                Some(id.to_vec()),
                labels,
            )
        }
        _ => (MessageMode::Default, None, args),
    };
    let comment = labels
        .iter()
        .enumerate()
        .find_map(|(position, &byte)| {
            (byte == b'#' && (position == 0 || labels[position - 1].is_ascii_whitespace()))
                .then_some(position)
        })
        .unwrap_or(labels.len());
    let subject = labels
        .get(comment + 1..)
        .map(trim)
        .unwrap_or_default()
        .to_vec();
    let labels: Vec<_> = labels[..comment]
        .split(u8::is_ascii_whitespace)
        .filter(|v| !v.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    if labels.is_empty() {
        return None;
    }
    Some(Instruction::Merge {
        commit,
        message,
        labels,
        subject,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::pick(b"p abcd subject\n", Instruction::Pick(CommitToken { id: b"abcd".to_vec(), subject: b"subject".to_vec() }))]
    #[case::fixup(b"f -C abcd fix\n", Instruction::Fixup(CommitToken { id: b"abcd".to_vec(), subject: b"fix".to_vec() }, MessageMode::Replace))]
    #[case::exec(b"x printf '# hi'\n", Instruction::Exec(b"printf '# hi'".to_vec()))]
    #[case::break_(b"b\r\n", Instruction::Break)]
    #[case::label(b"l branch\n", Instruction::Label(b"branch".to_vec()))]
    #[case::reset(b"t branch\n", Instruction::Reset(b"branch".to_vec()))]
    #[case::update_ref(b"u refs/heads/topic\n", Instruction::UpdateRef(b"refs/heads/topic".to_vec()))]
    #[case::reword(b"r abcd subject\n", Instruction::Reword(CommitToken { id: b"abcd".to_vec(), subject: b"subject".to_vec() }))]
    #[case::edit(b"e abcd subject\n", Instruction::Edit(CommitToken { id: b"abcd".to_vec(), subject: b"subject".to_vec() }))]
    #[case::squash(b"s abcd subject\n", Instruction::Squash(CommitToken { id: b"abcd".to_vec(), subject: b"subject".to_vec() }))]
    #[case::drop(b"d abcd subject\n", Instruction::Drop(CommitToken { id: b"abcd".to_vec(), subject: b"subject".to_vec() }))]
    #[case::revert(b"revert abcd subject\n", Instruction::Revert(CommitToken { id: b"abcd".to_vec(), subject: b"subject".to_vec() }))]
    #[case::exec_spaces(b"x echo keep  \n", Instruction::Exec(b"echo keep  ".to_vec()))]
    #[case::byte_subject(b"p abcd \xff\n", Instruction::Pick(CommitToken { id: b"abcd".to_vec(), subject: vec![255] }))]
    fn parses_aliases(#[case] input: &[u8], #[case] expected: Instruction) {
        let todo = Todo::parse(input, TodoLimits::default()).unwrap();
        assert_eq!(todo.lines()[0].instruction, expected);
    }

    #[test]
    fn merge_retains_ordered_labels_and_message_source() {
        let todo = Todo::parse(
            b"# comment\n\nmerge -c abcd left right # subject\n",
            TodoLimits::default(),
        )
        .unwrap();
        assert_eq!(todo.lines()[0].line, 3);
        assert_eq!(
            todo.lines()[0].instruction,
            Instruction::Merge {
                commit: Some(b"abcd".to_vec()),
                message: MessageMode::Edit,
                labels: vec![b"left".to_vec(), b"right".to_vec()],
                subject: b"subject".to_vec()
            }
        );
    }

    #[rstest]
    #[case::unknown(b"unknown abcd\n")]
    #[case::bad_id(b"pick xyz\n")]
    #[case::break_args(b"break argument\n")]
    #[case::empty_merge(b"merge -C abcd\n")]
    #[case::bad_label(b"label two names\n")]
    #[case::nul(b"exec a\0b\n")]
    #[case::nul_comment(b"# a\0b\n")]
    fn refuses_invalid_input(#[case] input: &[u8]) {
        assert!(matches!(
            Todo::parse(input, TodoLimits::default()),
            Err(TodoError::Syntax { line: 1, .. })
        ));
    }

    #[test]
    fn refuses_byte_and_instruction_limits() {
        let mut limits = TodoLimits::default();
        limits.max_bytes = 0;
        assert_eq!(Todo::parse(b"pick abcd\n", limits), Err(TodoError::Limit));
        limits.max_bytes = 1024;
        limits.max_instructions = 0;
        assert_eq!(Todo::parse(b"pick abcd\n", limits), Err(TodoError::Limit));
    }
}
