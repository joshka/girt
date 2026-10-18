use std::sync::atomic::AtomicBool;

use super::{Error, check, encoding};

#[derive(Clone, Default)]
pub(super) struct Headers(Vec<(Vec<u8>, Vec<u8>)>);
pub(super) struct HeaderBudget {
    pub(super) bytes: usize,
    pub(super) fields: usize,
}
impl Headers {
    pub(super) fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.0
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_slice())
    }
    pub(super) fn decode(
        &self,
        name: &[u8],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Option<String>, Error> {
        self.get(name)
            .map(|value| {
                let text = encoding::header(value, cancel)?;
                if text.len() > *budget {
                    return Err(Error::Limit("decoded bytes"));
                }
                *budget -= text.len();
                Ok(text)
            })
            .transpose()
    }
    pub(super) fn overridden(mut self, inner: Self) -> Self {
        for name in [b"from".as_slice(), b"subject", b"date"] {
            if let Some(value) = inner.get(name) {
                self.0.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
                self.0.push((name.to_vec(), value.to_vec()));
            }
        }
        self
    }
}
pub(super) fn read<'a>(
    input: &'a [u8],
    budget: &mut HeaderBudget,
    in_body: bool,
    cancel: &AtomicBool,
) -> Result<(Headers, &'a [u8]), Error> {
    read_lines(
        input,
        input.split_inclusive(|&byte| byte == b'\n'),
        budget,
        in_body,
        cancel,
    )
}

pub(super) fn read_lines<'a>(
    input: &'a [u8],
    lines: impl Iterator<Item = &'a [u8]>,
    budget: &mut HeaderBudget,
    in_body: bool,
    cancel: &AtomicBool,
) -> Result<(Headers, &'a [u8]), Error> {
    let mut result = Headers::default();
    let mut offset = 0;
    for line in lines {
        check(cancel)?;
        let value = line.strip_suffix(b"\n").unwrap_or(line);
        if value.is_empty() {
            offset += line.len();
            break;
        }
        let continuation = value.starts_with(b" ") || value.starts_with(b"\t");
        let field = crate::bytes::split_once(value, b':');
        if !continuation && field.is_none() {
            break;
        }
        if in_body
            && !continuation
            && !field.is_some_and(|(key, _)| {
                [b"from".as_slice(), b"subject", b"date"]
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
            })
        {
            break;
        }
        if line.len() > budget.bytes {
            return Err(Error::Limit("header bytes"));
        }
        budget.bytes -= line.len();
        if value.contains(&0) {
            return Err(Error::Malformed("header NUL"));
        }
        if continuation {
            let (_, previous) = result
                .0
                .last_mut()
                .ok_or(Error::Malformed("header continuation"))?;
            previous.push(b' ');
            previous.extend_from_slice(value.trim_ascii());
        } else if let Some((name, value)) = field {
            if budget.fields == 0 {
                return Err(Error::Limit("headers"));
            }
            budget.fields -= 1;
            let mut value = value.trim_ascii().to_vec();
            if in_body && !name.eq_ignore_ascii_case(b"subject") {
                value.push(b' ');
            }
            result.0.push((name.to_ascii_lowercase(), value));
        }
        offset += line.len();
    }
    Ok((result, &input[offset..]))
}
