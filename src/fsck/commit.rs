//! Commit header conventions beyond graph decoding, observed with isolated Git fixtures.
use std::collections::BTreeSet;

use super::{MessageId, check_identity};
use crate::{Commit, IdentityRef, ObjectFormat};

pub(super) fn check(format: ObjectFormat, data: &[u8], ids: &mut BTreeSet<MessageId>) {
    if Commit::parse(format, data).is_err() {
        ids.insert(MessageId::BadCommit);
        return;
    }
    // Git checks the complete physical header before interpreting its required records. A final
    // newline without a blank separator is accepted; an incomplete physical line is not.
    let mut remaining = data;
    while !remaining.is_empty() {
        let Some(end) = remaining.iter().position(|byte| *byte == b'\n') else {
            ids.insert(if remaining.contains(&0) {
                MessageId::NulInHeader
            } else {
                MessageId::UnterminatedHeader
            });
            return;
        };
        let line = &remaining[..end];
        remaining = &remaining[end + 1..];
        if line.contains(&0) {
            ids.insert(MessageId::NulInHeader);
            return;
        }
        if line.is_empty() {
            break;
        }
    }
    let mut lines = data.split(|byte| *byte == b'\n').peekable();
    lines.next(); // The ordinary parser has validated the leading tree record.
    while lines
        .peek()
        .is_some_and(|line| line.starts_with(b"parent "))
    {
        lines.next();
    }
    let Some(author) = lines.next().and_then(|line| line.strip_prefix(b"author ")) else {
        ids.insert(MessageId::MissingAuthor);
        return;
    };
    check_identity(
        IdentityRef::parse(author).map(Some),
        MessageId::MissingAuthor,
        ids,
    );
    if !ids.is_empty() {
        return;
    }
    if lines
        .peek()
        .is_some_and(|line| line.starts_with(b"author "))
    {
        ids.insert(MessageId::MultipleAuthors);
        return;
    }
    let Some(committer) = lines
        .next()
        .and_then(|line| line.strip_prefix(b"committer "))
    else {
        ids.insert(MessageId::MissingCommitter);
        return;
    };
    check_identity(
        IdentityRef::parse(committer).map(Some),
        MessageId::MissingCommitter,
        ids,
    );
    if ids.is_empty() && remaining.contains(&0) {
        ids.insert(MessageId::NulInCommit);
    }
}
