//! Strict identity framing for fsck, separate from permissive ordinary identity readers.
use super::MessageId;

pub(super) fn check(bytes: &[u8]) -> Option<MessageId> {
    let Some(open) = bytes.iter().position(|byte| matches!(byte, b'<' | b'>')) else {
        return Some(MessageId::MissingEmail);
    };
    if bytes[open] == b'>' {
        return Some(MessageId::BadName);
    }
    if open == 0 {
        return Some(MessageId::MissingNameBeforeEmail);
    }
    if bytes[open - 1] != b' ' {
        return Some(MessageId::MissingSpaceBeforeEmail);
    }
    let email = &bytes[open + 1..];
    let Some(close) = email.iter().position(|byte| matches!(byte, b'<' | b'>')) else {
        return Some(MessageId::BadEmail);
    };
    if email[close] != b'>' {
        return Some(MessageId::BadEmail);
    }
    let tail = &email[close + 1..];
    if tail.first() != Some(&b' ') {
        return Some(MessageId::MissingSpaceBeforeDate);
    }
    let date = tail.trim_ascii_start();
    let digits = date.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 || date.get(digits) != Some(&b' ') {
        return Some(MessageId::BadDate);
    }
    if digits > 1 && date[0] == b'0' {
        return Some(MessageId::ZeroPaddedDate);
    }
    let seconds = date[..digits].iter().try_fold(0_u64, |value, byte| {
        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
    });
    if seconds.is_none_or(|value| value > i64::MAX as u64) {
        return Some(MessageId::BadDateOverflow);
    }
    let timezone = &date[digits + 1..];
    if timezone.len() != 5
        || !matches!(timezone[0], b'+' | b'-')
        || !timezone[1..].iter().all(u8::is_ascii_digit)
    {
        return Some(MessageId::BadTimezone);
    }
    None
}
