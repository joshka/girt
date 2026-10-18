//! Commit header conventions beyond graph decoding, observed with isolated Git fixtures.
use std::collections::BTreeSet;

use super::{MessageId, headers, identity};
use crate::{Commit, ObjectFormat};

pub(super) fn check(format: ObjectFormat, data: &[u8], ids: &mut BTreeSet<MessageId>) {
    if Commit::parse(format, data).is_err() {
        ids.insert(MessageId::BadCommit);
        return;
    }
    let Some(remaining) = headers::body(data, ids) else {
        return;
    };
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
    if let Some(message) = identity::check(author) {
        ids.insert(message);
    }
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
    if let Some(message) = identity::check(committer) {
        ids.insert(message);
    }
    if ids.is_empty() && remaining.contains(&0) {
        ids.insert(MessageId::NulInCommit);
    }
}
