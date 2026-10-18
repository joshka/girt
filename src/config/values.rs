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

/// Decode Git's scalar boolean spelling; `None` is an implicit true value.
pub(crate) fn boolean(value: Option<&[u8]>) -> Option<bool> {
    let Some(value) = value else {
        return Some(true);
    };
    match value.to_ascii_lowercase().as_slice() {
        b"true" | b"yes" | b"on" => Some(true),
        b"false" | b"no" | b"off" | b"" => Some(false),
        _ => boolean_integer(value).map(|number| number != 0),
    }
}

/// Parses a boolean's numeric form as Git does: like `strtol`, leading whitespace is skipped,
/// and the value must fit a C `int`, so `2147483648` is not a boolean.
fn boolean_integer(value: &[u8]) -> Option<i64> {
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
        .unwrap_or(value.len());
    integer(value.get(start..)?).filter(|number| i32::try_from(*number).is_ok())
}

/// Parses a boolean as Git does for configuration values and environment variables such as
/// `GIT_NO_LAZY_FETCH`.
///
/// `true`, `yes` and `on` are true and `false`, `no`, `off` and the empty string are false, in any
/// case. Any other value must be an integer in Git's syntax, which is true unless it is zero:
/// optional leading whitespace and sign, a decimal, `0x` hexadecimal or leading-`0` octal number,
/// and an optional `k`, `m` or `g` multiplier, all within the range of a C `int`. Returns `None`
/// for anything else, where Git reports a bad boolean value.
///
/// # Examples
///
/// ```
/// use girt::config::parse_boolean;
///
/// assert_eq!(parse_boolean(b"Yes"), Some(true));
/// assert_eq!(parse_boolean(b""), Some(false));
/// assert_eq!(parse_boolean(b" 1"), Some(true));
/// assert_eq!(parse_boolean(b"0x0"), Some(false));
/// assert_eq!(parse_boolean(b"1 "), None);
/// assert_eq!(parse_boolean(b"2147483648"), None);
/// ```
pub fn parse_boolean(value: &[u8]) -> Option<bool> {
    boolean(Some(value))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    /// Expectations observed from `git config --type=bool` and `GIT_NO_LAZY_FETCH` in Git 2.56,
    /// which agree.
    #[rstest]
    #[case(b"true", Some(true))]
    #[case(b"TRUE", Some(true))]
    #[case(b"On", Some(true))]
    #[case(b"OFF", Some(false))]
    #[case(b"", Some(false))]
    #[case(b" ", None)]
    #[case(b" 1", Some(true))]
    #[case(b" 0", Some(false))]
    #[case(b"1 ", None)]
    #[case(b" true", None)]
    #[case(b"true ", None)]
    #[case(b"-0", Some(false))]
    #[case(b"+1", Some(true))]
    #[case(b"007", Some(true))]
    #[case(b"08", None)]
    #[case(b"0x", None)]
    #[case(b"1.0", None)]
    #[case(b"tru", None)]
    #[case(b"0x7fffffff", Some(true))]
    #[case(b"0x80000000", None)]
    #[case(b"2147483647", Some(true))]
    #[case(b"2147483648", None)]
    #[case(b"-2147483648", Some(true))]
    #[case(b"-2147483649", None)]
    #[case(b"2097151k", Some(true))]
    #[case(b"2097152k", None)]
    #[case(b"1g", Some(true))]
    fn booleans_parse_like_git(#[case] value: &[u8], #[case] expected: Option<bool>) {
        assert_eq!(super::parse_boolean(value), expected);
    }
}
