//! Physical commit/tag header framing, before interpretation of individual records.
use std::collections::BTreeSet;

use super::MessageId;

pub(super) fn body<'a>(data: &'a [u8], ids: &mut BTreeSet<MessageId>) -> Option<&'a [u8]> {
    // A final newline without a blank separator is accepted; an incomplete physical line is not.
    let mut remaining = data;
    while !remaining.is_empty() {
        let Some(end) = remaining.iter().position(|byte| *byte == b'\n') else {
            ids.insert(if remaining.contains(&0) {
                MessageId::NulInHeader
            } else {
                MessageId::UnterminatedHeader
            });
            return None;
        };
        let line = &remaining[..end];
        remaining = &remaining[end + 1..];
        if line.contains(&0) {
            ids.insert(MessageId::NulInHeader);
            return None;
        }
        if line.is_empty() {
            break;
        }
    }
    Some(remaining)
}
