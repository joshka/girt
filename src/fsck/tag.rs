//! Annotated tag name and tagger conventions beyond graph decoding.
use std::collections::BTreeSet;

use super::{MessageId, headers, identity};
use crate::{ObjectFormat, Tag};

pub(super) fn check(format: ObjectFormat, data: &[u8], ids: &mut BTreeSet<MessageId>) {
    let Ok(tag) = Tag::parse(format, data) else {
        ids.insert(MessageId::BadTagObject);
        return;
    };
    if headers::body(data, ids).is_none() {
        return;
    }
    if crate::refs::RefName::tag(tag.name()).is_err() {
        ids.insert(MessageId::BadTagName);
    }
    let Some(tagger) = data
        .split(|byte| *byte == b'\n')
        .nth(3)
        .and_then(|line| line.strip_prefix(b"tagger "))
    else {
        ids.insert(MessageId::MissingTaggerEntry);
        return;
    };
    if let Some(message) = identity::check(tagger) {
        ids.insert(message);
    }
}
