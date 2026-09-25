// Git integer syntax has C-style bases and binary k/m/g multipliers.
pub(crate) fn integer(value: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(value).ok()?;
    let (text, multiplier) = match text.as_bytes().last()? {
        b'k' | b'K' => (&text[..text.len() - 1], 1024i64),
        b'm' | b'M' => (&text[..text.len() - 1], 1024i64.pow(2)),
        b'g' | b'G' => (&text[..text.len() - 1], 1024i64.pow(3)),
        _ => (text, 1),
    };
    let (digits, sign) = if let Some(rest) = text.strip_prefix('-') {
        (rest, -1)
    } else {
        (text.strip_prefix('+').unwrap_or(text), 1)
    };
    let (digits, radix) = if let Some(rest) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (rest, 16)
    } else if digits.starts_with('0') {
        (digits, 8)
    } else {
        (digits, 10)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    i64::from_str_radix(digits, radix)
        .ok()?
        .checked_mul(sign)?
        .checked_mul(multiplier)
}
