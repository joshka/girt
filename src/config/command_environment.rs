use super::parse::{SectionName, SectionOccurrence};
use super::{Config, ConfigError, Entry};

// A deterministic allowance for each synthetic section, entry and membership index. This is a
// logical work budget, not a promise about allocator overhead.
const ASSIGNMENT_BYTES: usize = 128;

impl Config {
    /// Decodes counted environment pairs followed by legacy `GIT_CONFIG_PARAMETERS` assignments.
    ///
    /// Unlike [`Self::from_environment`], this accepts both Git command-environment encodings.
    /// Legacy assignments use single-quoted `key=value` or separately quoted `key` and `value`.
    /// Values are literal bytes; implicit and explicitly empty values remain distinct. Each
    /// assignment forms its own synthetic section, in input order. [`Self::resolve`] attaches
    /// runtime provenance when this result is supplied as [`super::ConfigInputs::environment`];
    /// explicit command entries follow it. No shell, file or process environment is accessed.
    ///
    /// `max_pairs` bounds the combined number of assignments. `max_bytes` counts all supplied
    /// COUNT, KEY, VALUE and PARAMETERS bytes plus 128 logical bytes per assignment. Checks precede
    /// decoder-owned retention and decoding copies. The supplier owns allocation of its returned
    /// buffers and should bound that allocation independently. The byte budget is not a measurement
    /// of heap use; decoded output is no larger than the supplied bytes, apart from bounded
    /// metadata.
    ///
    /// # Errors
    ///
    /// Returns counted-pair errors before examining legacy parameters, or legacy quoting/key/NUL
    /// errors. Resource exhaustion reports `environment pair limit` or `environment byte limit`.
    /// Error `line` is the one-based assignment ordinal, continuing after the counted pairs;
    /// count and whole-input byte errors use line 1. Raw input values are never included in errors.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// let config = Config::from_command_environment(
    ///     |key| (key == "GIT_CONFIG_PARAMETERS").then(|| b"'core.flag' 'core.value'=''".to_vec()),
    ///     2,
    ///     1024,
    /// )?;
    /// assert_eq!(config.value("core", None, "flag"), Some(None));
    /// assert_eq!(
    ///     config.value("core", None, "value"),
    ///     Some(Some(b"".as_slice()))
    /// );
    /// # Ok::<(), girt::ConfigError>(())
    /// ```
    pub fn from_command_environment(
        mut get: impl FnMut(&str) -> Option<Vec<u8>>,
        max_pairs: usize,
        max_bytes: usize,
    ) -> Result<Self, ConfigError> {
        let mut remaining = max_bytes;
        let mut exhausted = false;
        let counted = Self::from_environment(
            |key| {
                let value = get(key)?;
                let metadata = if key.starts_with("GIT_CONFIG_VALUE_") {
                    ASSIGNMENT_BYTES
                } else {
                    0
                };
                let charge = value.len().checked_add(metadata);
                let available = charge.and_then(|charge| remaining.checked_sub(charge));
                match available {
                    Some(bytes) => {
                        remaining = bytes;
                        Some(value)
                    }
                    None => {
                        exhausted = true;
                        None
                    }
                }
            },
            max_pairs,
        );
        if exhausted {
            return Err(error(1, "environment byte limit"));
        }
        let mut config = counted?;
        let parameters = get("GIT_CONFIG_PARAMETERS").unwrap_or_default();
        remaining = remaining
            .checked_sub(parameters.len())
            .ok_or_else(|| error(1, "environment byte limit"))?;
        let mut rest = parameters.as_slice();
        while !rest.is_empty() {
            let index = config.entries.len();
            let line = index.saturating_add(1);
            if index >= max_pairs {
                return Err(error(line, "environment pair limit"));
            }
            remaining = remaining
                .checked_sub(ASSIGNMENT_BYTES)
                .ok_or_else(|| error(line, "environment byte limit"))?;
            let mut key = quoted(&mut rest, line)?;
            let value = if rest.first() == Some(&b'=') {
                rest = &rest[1..];
                Some(if rest.first() == Some(&b'\'') {
                    quoted(&mut rest, line)?
                } else {
                    Vec::new()
                })
            } else if let Some(equal) = key.iter().position(|&byte| byte == b'=') {
                let value = key.split_off(equal + 1);
                key.pop();
                Some(value)
            } else {
                None
            };
            if !rest.is_empty() && !rest[0].is_ascii_whitespace() {
                return Err(error(line, "invalid command environment quoting"));
            }
            append_assignment(&mut config, &key, value, line)?;
            rest = rest.trim_ascii_start();
        }
        Ok(config)
    }
}

fn error(line: usize, reason: &'static str) -> ConfigError {
    ConfigError { line, reason }
}

/// Decode Git's single-quote encoding, including escaped quote and bang between quoted spans.
fn quoted(input: &mut &[u8], line: usize) -> Result<Vec<u8>, ConfigError> {
    let invalid = || error(line, "invalid command environment quoting");
    if input.first() != Some(&b'\'') {
        return Err(invalid());
    }
    let mut rest = &input[1..];
    let mut value = Vec::new();
    loop {
        let end = rest
            .iter()
            .position(|&byte| byte == b'\'')
            .ok_or_else(invalid)?;
        value.extend_from_slice(&rest[..end]);
        rest = &rest[end + 1..];
        if rest.len() >= 3
            && rest[0] == b'\\'
            && matches!(rest[1], b'\'' | b'!')
            && rest[2] == b'\''
        {
            value.push(rest[1]);
            rest = &rest[3..];
        } else {
            *input = rest;
            return Ok(value);
        }
    }
}

fn append_assignment(
    config: &mut Config,
    key: &[u8],
    value: Option<Vec<u8>>,
    line: usize,
) -> Result<(), ConfigError> {
    let invalid = || error(line, "invalid command environment key or value");
    let first = key
        .iter()
        .position(|&byte| byte == b'.')
        .ok_or_else(invalid)?;
    let last = key.iter().rposition(|&byte| byte == b'.').unwrap();
    let section = &key[..first];
    let name = &key[last + 1..];
    if section.is_empty()
        || !section
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        || !name.first().is_some_and(u8::is_ascii_alphabetic)
        || !name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        || key.contains(&0)
        || value.as_ref().is_some_and(|value| value.contains(&0))
    {
        return Err(invalid());
    }
    let subsection = (first != last).then(|| key[first + 1..last].to_vec());
    let index = config.entries.len();
    config.occurrences.push(SectionOccurrence {
        name: SectionName {
            section: section.to_vec(),
            subsection: subsection.clone(),
        },
        start: index,
        entries: vec![index],
    });
    config.entries.push(Entry {
        section: section.to_vec(),
        subsection,
        name: name.to_vec(),
        value,
        line,
        origin: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests;
