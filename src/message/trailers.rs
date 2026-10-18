//! Trailers: the `Key: value` lines that end a commit message.

use super::{CommentPrefix, is_scissors};
use crate::Config;

/// One `Key: value` trailer.
///
/// [`trailers`] returns the key as configured (`trailer.<name>.key` replaces a matching
/// abbreviation) and the value with continuation lines joined by single spaces. Values passed to
/// [`add_trailers`] are trimmed of surrounding whitespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trailer {
    /// The trailer's key, such as `Signed-off-by`.
    pub key: Vec<u8>,
    /// The trailer's value, such as `A. Writer <writer@example.com>`.
    pub value: Vec<u8>,
}

impl Trailer {
    /// Splits a `--trailer` argument of `git interpret-trailers` into key and value.
    ///
    /// The key ends at the first configured separator or `=`, which the command line always
    /// accepts. Without a separator the whole argument is the key and the value is empty. Key and
    /// value are trimmed.
    ///
    /// ```
    /// use girt::message::{Trailer, TrailerConfig};
    ///
    /// let config = TrailerConfig::default();
    /// let trailer = Trailer::parse_argument(b"Acked-by = A. Writer", &config).unwrap();
    /// assert_eq!(trailer.key, b"Acked-by");
    /// assert_eq!(trailer.value, b"A. Writer");
    /// assert!(Trailer::parse_argument(b": no key", &config).is_none());
    /// ```
    ///
    /// Returns `None` when the key is empty, as Git refuses an empty trailer token.
    pub fn parse_argument(argument: &[u8], config: &TrailerConfig) -> Option<Self> {
        let split = argument
            .iter()
            .position(|byte| *byte == b'=' || config.separators.contains(byte));
        let (key, value) = match split {
            Some(at) => (&argument[..at], &argument[at + 1..]),
            None => (argument, &b""[..]),
        };
        let key = trim(key);
        (!key.is_empty()).then(|| Self {
            key: key.to_vec(),
            value: trim(value).to_vec(),
        })
    }
}

/// Where [`add_trailers`] puts a new trailer, as `trailer.where` configures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Placement {
    /// After the last line of the trailer block.
    #[default]
    End,
    /// Before the first line of the trailer block.
    Start,
    /// After the last trailer with the same key, or at the end when there is none.
    After,
    /// Before the first trailer with the same key, or at the start when there is none.
    Before,
}

/// What [`add_trailers`] does when the block already has a trailer with the same key, as
/// `trailer.ifExists` configures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IfExists {
    /// Add unless the line where the trailer would go is the same trailer.
    #[default]
    AddIfDifferentNeighbor,
    /// Add unless a trailer with the same key has the same value.
    AddIfDifferent,
    /// Always add.
    Add,
    /// Remove the trailer with the same key nearest the placement, then add.
    Replace,
    /// Leave the block unchanged.
    DoNothing,
}

/// What [`add_trailers`] does when the block has no trailer with the same key, as
/// `trailer.ifMissing` configures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IfMissing {
    /// Add the trailer.
    #[default]
    Add,
    /// Leave the block unchanged.
    DoNothing,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Rules {
    placement: Placement,
    if_exists: IfExists,
    if_missing: IfMissing,
}

/// Settings for trailers with one key, as a `trailer.<name>.*` configuration section holds them.
///
/// A trailer key uses these settings when, without trailing `-`, it is a case-insensitive prefix
/// of the name or of the configured key, so `sob` and `Sig` both select a section named `sob`
/// with key `Signed-off-by`. Unset rules fall back to the [`TrailerConfig`] defaults.
///
/// ```
/// use girt::message::{IfExists, Placement, Trailer, TrailerConfig, TrailerKey, add_trailers};
///
/// let config = TrailerConfig::default().with_key(
///     TrailerKey::new(b"sob")
///         .with_key(b"Signed-off-by")
///         .with_placement(Placement::Start)
///         .with_if_exists(IfExists::AddIfDifferent),
/// );
/// let new = Trailer::parse_argument(b"sob: A. Writer", &config).unwrap();
/// assert_eq!(
///     add_trailers(b"Subject\n\nFixes: #1\n", &[new], &config),
///     b"Subject\n\nSigned-off-by: A. Writer\nFixes: #1\n"
/// );
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrailerKey {
    name: Vec<u8>,
    key: Option<Vec<u8>>,
    placement: Option<Placement>,
    if_exists: Option<IfExists>,
    if_missing: Option<IfMissing>,
}

impl TrailerKey {
    /// Settings named `name`, with no key spelling and no rules of their own.
    pub fn new(name: &[u8]) -> Self {
        Self {
            name: name.to_vec(),
            key: None,
            placement: None,
            if_exists: None,
            if_missing: None,
        }
    }

    /// Writes matching trailers with `key` instead of the abbreviation given, as
    /// `trailer.<name>.key` does.
    pub fn with_key(mut self, key: &[u8]) -> Self {
        self.key = Some(key.to_vec());
        self
    }

    /// Places matching trailers as `trailer.<name>.where` does.
    pub fn with_placement(mut self, placement: Placement) -> Self {
        self.placement = Some(placement);
        self
    }

    /// Handles an existing matching trailer as `trailer.<name>.ifExists` does.
    pub fn with_if_exists(mut self, if_exists: IfExists) -> Self {
        self.if_exists = Some(if_exists);
        self
    }

    /// Handles a missing matching trailer as `trailer.<name>.ifMissing` does.
    pub fn with_if_missing(mut self, if_missing: IfMissing) -> Self {
        self.if_missing = Some(if_missing);
        self
    }

    /// A key abbreviates this section when, without trailing `-`, it is an ASCII
    /// case-insensitive prefix of the section name or its configured key.
    fn matches(&self, key: &[u8]) -> bool {
        let end = key
            .iter()
            .rposition(|byte| *byte != b'-')
            .map_or(0, |last| last + 1);
        let key = &key[..end];
        is_prefix(key, &self.name) || self.key.as_deref().is_some_and(|full| is_prefix(key, full))
    }
}

/// How trailers are found and added: separators, placement rules and per-key settings.
///
/// [`Self::default`] is Git's behaviour without configuration; [`Self::from_config`] reads the
/// `trailer.*` variables.
#[derive(Clone, Debug)]
pub struct TrailerConfig {
    separators: Vec<u8>,
    rules: Rules,
    keys: Vec<TrailerKey>,
    comment: CommentPrefix,
    divider: bool,
}

impl Default for TrailerConfig {
    fn default() -> Self {
        Self {
            separators: b":".to_vec(),
            rules: Rules::default(),
            keys: Vec::new(),
            comment: CommentPrefix::default(),
            divider: true,
        }
    }
}

/// `trailer.<name>.command` or `.cmd` is configured; girt does not run trailer commands.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("trailer.{name}.{variable} runs a command, which girt does not support")]
#[non_exhaustive]
pub struct TrailerCommandError {
    /// The `<name>` of the configured section, lossily decoded.
    pub name: String,
    /// `command` or `cmd`.
    pub variable: String,
}

impl TrailerConfig {
    /// Reads `trailer.separators`, `trailer.where`, `trailer.ifExists`, `trailer.ifMissing` and
    /// each `trailer.<name>.key`, `.where`, `.ifExists` and `.ifMissing`.
    ///
    /// Values are case-insensitive. An unknown value is ignored, as Git ignores it with a warning.
    /// When several sections match a key, the first section in configuration order applies.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::message::{Trailer, TrailerConfig, add_trailers};
    ///
    /// let config = Config::parse(b"[trailer \"sob\"]\n\tkey = Signed-off-by\n")?;
    /// let config = TrailerConfig::from_config(&config)?;
    /// let new = Trailer::parse_argument(b"sob: A. Writer", &config).unwrap();
    /// assert_eq!(
    ///     add_trailers(b"Subject\n", &[new], &config),
    ///     b"Subject\n\nSigned-off-by: A. Writer\n"
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`TrailerCommandError`] when any `trailer.<name>.command` or `.cmd` is set: Git
    /// runs those commands to compute values, and girt refuses rather than omit them.
    pub fn from_config(config: &Config) -> Result<Self, TrailerCommandError> {
        let mut result = Self::default();
        if let Ok(Some(comment)) = CommentPrefix::from_config(config) {
            result.comment = comment;
        }
        for entry in config.entries() {
            if !entry.section.eq_ignore_ascii_case(b"trailer") {
                continue;
            }
            let name = String::from_utf8_lossy(&entry.name).to_ascii_lowercase();
            let value = entry.value.as_deref();
            let Some(subsection) = &entry.subsection else {
                match (name.as_str(), value) {
                    ("separators", Some(value)) if !value.is_empty() => {
                        result.separators = value.to_vec();
                    }
                    ("where", Some(value)) => {
                        if let Some(placement) = placement(value) {
                            result.rules.placement = placement;
                        }
                    }
                    ("ifexists", Some(value)) => {
                        if let Some(if_exists) = if_exists(value) {
                            result.rules.if_exists = if_exists;
                        }
                    }
                    ("ifmissing", Some(value)) => {
                        if let Some(if_missing) = if_missing(value) {
                            result.rules.if_missing = if_missing;
                        }
                    }
                    _ => {}
                }
                continue;
            };
            if name == "command" || name == "cmd" {
                return Err(TrailerCommandError {
                    name: String::from_utf8_lossy(subsection).into_owned(),
                    variable: name,
                });
            }
            let index = match result.keys.iter().position(|key| key.name == *subsection) {
                Some(index) => index,
                None => {
                    result.keys.push(TrailerKey::new(subsection));
                    result.keys.len() - 1
                }
            };
            let rules = &mut result.keys[index];
            let Some(value) = value else { continue };
            match name.as_str() {
                "key" => rules.key = Some(value.to_vec()),
                "where" => rules.placement = placement(value).or(rules.placement),
                "ifexists" => rules.if_exists = if_exists(value).or(rules.if_exists),
                "ifmissing" => rules.if_missing = if_missing(value).or(rules.if_missing),
                _ => {}
            }
        }
        Ok(result)
    }

    /// Places new trailers as `trailer.where` does, unless their key's settings say otherwise.
    pub fn with_placement(mut self, placement: Placement) -> Self {
        self.rules.placement = placement;
        self
    }

    /// Handles an existing trailer with the same key as `trailer.ifExists` does, unless the key's
    /// settings say otherwise.
    pub fn with_if_exists(mut self, if_exists: IfExists) -> Self {
        self.rules.if_exists = if_exists;
        self
    }

    /// Handles a missing key as `trailer.ifMissing` does, unless the key's settings say otherwise.
    pub fn with_if_missing(mut self, if_missing: IfMissing) -> Self {
        self.rules.if_missing = if_missing;
        self
    }

    /// Uses `separators` to find trailers, as `trailer.separators` does; the first one is written
    /// in new trailer lines. Returns `None` when `separators` is empty.
    pub fn with_separators(mut self, separators: &[u8]) -> Option<Self> {
        if separators.is_empty() {
            return None;
        }
        self.separators = separators.to_vec();
        Some(self)
    }

    /// Adds settings for one key after any already present; the first matching settings apply.
    /// A section with the same name replaces the earlier one.
    pub fn with_key(mut self, key: TrailerKey) -> Self {
        match self
            .keys
            .iter_mut()
            .find(|existing| existing.name == key.name)
        {
            Some(existing) => *existing = key,
            None => self.keys.push(key),
        }
        self
    }

    /// Sets whether a line starting with `---` ends the message, as in a patch email.
    ///
    /// On by default, as for `git interpret-trailers`; `--no-divider` turns it off.
    pub fn with_divider(mut self, divider: bool) -> Self {
        self.divider = divider;
        self
    }

    /// Sets the prefix of comment lines, which trailer detection skips. `#` by default, or as
    /// `core.commentChar` configures when read with [`Self::from_config`].
    pub fn with_comment_prefix(mut self, comment: CommentPrefix) -> Self {
        self.comment = comment;
        self
    }

    fn section(&self, key: &[u8]) -> Option<&TrailerKey> {
        self.keys.iter().find(|rules| rules.matches(key))
    }

    /// The configured spelling of `key` and the rules that apply to it.
    fn resolve(&self, key: &[u8]) -> (Vec<u8>, Rules) {
        let Some(section) = self.section(key) else {
            return (key.to_vec(), self.rules);
        };
        let rules = Rules {
            placement: section.placement.unwrap_or(self.rules.placement),
            if_exists: section.if_exists.unwrap_or(self.rules.if_exists),
            if_missing: section.if_missing.unwrap_or(self.rules.if_missing),
        };
        (section.key.clone().unwrap_or_else(|| key.to_vec()), rules)
    }

    /// Writes `key`, a separator and `value` as Git writes a trailer line, without the newline.
    fn format(&self, key: &[u8], value: &[u8], output: &mut Vec<u8>) {
        output.extend_from_slice(key);
        if !key
            .trim_ascii_end()
            .last()
            .is_some_and(|last| self.separators.contains(last))
        {
            output.push(self.separators[0]);
            output.push(b' ');
        }
        output.extend_from_slice(value);
    }
}

fn placement(value: &[u8]) -> Option<Placement> {
    Some(match value.to_ascii_lowercase().as_slice() {
        b"end" => Placement::End,
        b"start" => Placement::Start,
        b"after" => Placement::After,
        b"before" => Placement::Before,
        _ => return None,
    })
}

fn if_exists(value: &[u8]) -> Option<IfExists> {
    Some(match value.to_ascii_lowercase().as_slice() {
        b"addifdifferentneighbor" => IfExists::AddIfDifferentNeighbor,
        b"addifdifferent" => IfExists::AddIfDifferent,
        b"add" => IfExists::Add,
        b"replace" => IfExists::Replace,
        b"donothing" => IfExists::DoNothing,
        _ => return None,
    })
}

fn if_missing(value: &[u8]) -> Option<IfMissing> {
    Some(match value.to_ascii_lowercase().as_slice() {
        b"add" => IfMissing::Add,
        b"donothing" => IfMissing::DoNothing,
        _ => return None,
    })
}

/// A line of the trailer block, as it will be written back.
#[derive(Clone, Debug)]
enum Line<'m> {
    Trailer {
        key: Vec<u8>,
        value: Vec<u8>,
        continuation: Vec<&'m [u8]>,
    },
    Other(&'m [u8]),
}

/// The trailer block of a message and the bytes around it.
struct Block<'m> {
    /// Bytes before the block, or the whole content when there is no block.
    before: &'m [u8],
    /// Bytes after the block: trailing comments, blank lines, a scissors line or a divider.
    after: &'m [u8],
    lines: Option<Vec<Line<'m>>>,
}

/// Lines with their byte offsets; the last line may lack a newline.
fn lines(message: &[u8]) -> Vec<(usize, &[u8])> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < message.len() {
        let end = message[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(message.len(), |at| start + at + 1);
        result.push((start, &message[start..end]));
        start = end;
    }
    result
}

fn content(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

fn is_divider(line: &[u8]) -> bool {
    content(line)
        .strip_prefix(b"---")
        .is_some_and(|rest| rest.first().is_none_or(u8::is_ascii_whitespace))
}

fn is_prefix(prefix: &[u8], of: &[u8]) -> bool {
    of.len() >= prefix.len() && of[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Keys match when the shorter is an ASCII case-insensitive prefix of the longer, as Git compares
/// a new trailer's key with existing ones.
fn same_key(left: &[u8], right: &[u8]) -> bool {
    let length = left.len().min(right.len());
    left[..length].eq_ignore_ascii_case(&right[..length])
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |last| last + 1);
    &bytes[start..end]
}

impl TrailerConfig {
    /// Splits a line into a trailer key and value: the key is the text before the first
    /// separator, trimmed, and made only of ASCII letters, digits and `-`. A URL such as
    /// `https://example.com` is not a trailer.
    fn split<'l>(&self, line: &'l [u8]) -> Option<(&'l [u8], &'l [u8])> {
        let line = content(line);
        let at = line
            .iter()
            .position(|byte| self.separators.contains(byte))?;
        let key = trim(&line[..at]);
        let valid = !key.is_empty()
            && !line[at..].starts_with(b"://")
            && key.len() == line[..at].trim_ascii_end().len()
            && key
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-');
        valid.then(|| (key, trim(&line[at + 1..])))
    }

    /// Whether Git counts a line toward the 25% rule: a `Signed-off-by: ` line, a cherry-pick note
    /// or a trailer whose key, exactly as written before the separator, abbreviates a configured
    /// section.
    fn recognized(&self, line: &[u8], is_trailer: bool) -> bool {
        let configured = || {
            line.iter()
                .position(|byte| self.separators.contains(byte))
                .is_some_and(|at| self.section(&line[..at]).is_some())
        };
        line.starts_with(b"Signed-off-by: ")
            || line.starts_with(b"(cherry picked from commit ")
            || (is_trailer && configured())
    }

    fn block<'m>(&self, message: &'m [u8]) -> Block<'m> {
        let lines = lines(message);
        let comment = |line: &[u8]| self.comment.starts(line);
        let end = lines
            .iter()
            .position(|(_, line)| {
                is_scissors(content(line), &self.comment) || (self.divider && is_divider(line))
            })
            .unwrap_or(lines.len());
        let mut content_end = end;
        while content_end > 0 {
            let line = lines[content_end - 1].1;
            if !is_blank(line) && !comment(line) {
                break;
            }
            content_end -= 1;
        }
        let offset = |index: usize| lines.get(index).map_or(message.len(), |(at, _)| *at);
        let no_block = Block {
            before: &message[..offset(content_end)],
            after: &message[offset(content_end)..],
            lines: None,
        };
        let subject = lines
            .iter()
            .position(|(_, line)| !is_blank(line) && !comment(line));
        let mut start = content_end;
        while start > 0 && !is_blank(lines[start - 1].1) {
            start -= 1;
        }
        // The first paragraph is the subject, never trailers.
        if subject.is_none_or(|subject| start <= subject) {
            return no_block;
        }
        let mut trailers = 0usize;
        let mut others = 0usize;
        let mut recognized = false;
        // Whether the previous counted line was a trailer, which continuation lines extend.
        let mut after_trailer = false;
        let mut block = Vec::new();
        for &(_, line) in &lines[start..content_end] {
            if comment(line) {
                after_trailer = false;
                continue;
            }
            let continues = after_trailer
                && line
                    .first()
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'));
            if continues {
                match block.last_mut() {
                    // Git moves the first continuation of an empty value onto the trailer's line.
                    Some(Line::Trailer {
                        value,
                        continuation,
                        ..
                    }) if value.is_empty() && continuation.is_empty() => {
                        *value = trim(line).to_vec();
                    }
                    Some(Line::Trailer { continuation, .. }) => continuation.push(line),
                    _ => block.push(Line::Other(line)),
                }
                continue;
            }
            let split = self.split(line);
            recognized |= self.recognized(line, split.is_some());
            if let Some((key, value)) = split {
                trailers += 1;
                after_trailer = true;
                block.push(Line::Trailer {
                    key: self.resolve(key).0,
                    value: value.to_vec(),
                    continuation: Vec::new(),
                });
            } else {
                // Git's own lines count as trailers even when no separator matches.
                if line.starts_with(b"(cherry picked from commit ")
                    || line.starts_with(b"Signed-off-by: ")
                {
                    trailers += 1;
                    after_trailer = true;
                } else {
                    others += 1;
                    after_trailer = false;
                }
                block.push(Line::Other(line));
            }
        }
        let is_block = others == 0 || (recognized && trailers * 3 >= others);
        if !is_block {
            return no_block;
        }
        Block {
            before: &message[..offset(start)],
            after: &message[offset(content_end)..],
            lines: Some(block),
        }
    }
}

/// Returns the trailers at the end of `message`, as `git interpret-trailers --parse` lists them.
///
/// The trailer block is the last paragraph before any scissors line, `---` divider (unless turned
/// off with [`TrailerConfig::with_divider`]), trailing comment lines and blank lines. It is never
/// the first paragraph. It counts as trailers when every line is a trailer, or when at least a
/// quarter of its lines are trailers and one is a `Signed-off-by: ` line, a
/// `(cherry picked from commit …` note or uses a configured key. Comment lines are skipped.
/// Continuation lines start with a space or tab and belong to the trailer directly above, with no
/// comment line between them; they are joined to its value with single spaces.
///
/// ```
/// use girt::message::{Trailer, TrailerConfig, trailers};
///
/// let message =
///     b"Subject\n\nBody.\n\nSigned-off-by: A. Writer\nReviewed-by: B. Reader\n  and C.\n";
/// let found = trailers(message, &TrailerConfig::default());
/// assert_eq!(
///     found,
///     [
///         Trailer {
///             key: b"Signed-off-by".to_vec(),
///             value: b"A. Writer".to_vec()
///         },
///         Trailer {
///             key: b"Reviewed-by".to_vec(),
///             value: b"B. Reader and C.".to_vec()
///         },
///     ]
/// );
/// ```
pub fn trailers(message: &[u8], config: &TrailerConfig) -> Vec<Trailer> {
    let Some(lines) = config.block(message).lines else {
        return Vec::new();
    };
    lines
        .into_iter()
        .filter_map(|line| match line {
            Line::Trailer {
                key,
                mut value,
                continuation,
            } => {
                for line in continuation {
                    value.push(b' ');
                    value.extend_from_slice(trim(line));
                }
                Some(Trailer { key, value })
            }
            Line::Other(_) => None,
        })
        .collect()
}

/// Adds trailers to `message` as `git interpret-trailers --trailer …` does.
///
/// Each new trailer is applied in order to the block the previous ones produced. Its key is
/// replaced by the configured `trailer.<name>.key` when it abbreviates a section, and that
/// section's placement and existence rules apply. An existing trailer has the same key when the
/// shorter key is a case-insensitive prefix of the longer.
///
/// Like Git, this rewrites the whole trailer block: separators become the first configured
/// separator followed by a space, configured keys replace their abbreviations and comment lines
/// inside the block are dropped. When the message has no trailer block, a blank line and the new
/// block go after its last content line, before trailing comments and blank lines; Git adds that
/// blank line even when there are no trailers to add.
///
/// ```
/// use girt::message::{Trailer, TrailerConfig, add_trailers};
///
/// let config = TrailerConfig::default();
/// let sign = Trailer {
///     key: b"Signed-off-by".to_vec(),
///     value: b"A. Writer".to_vec(),
/// };
/// let message = add_trailers(b"Subject\n\n# comment\n", &[sign.clone()], &config);
/// assert_eq!(
///     message,
///     b"Subject\n\nSigned-off-by: A. Writer\n\n# comment\n"
/// );
///
/// // By default a trailer is not repeated next to itself.
/// assert_eq!(add_trailers(&message, &[sign], &config), message);
/// ```
pub fn add_trailers(message: &[u8], new: &[Trailer], config: &TrailerConfig) -> Vec<u8> {
    let block = config.block(message);
    let had_block = block.lines.is_some();
    let mut lines = block.lines.unwrap_or_default();
    for trailer in new {
        let (key, rules) = config.resolve(trim(&trailer.key));
        let value = trim(&trailer.value).to_vec();
        add(&mut lines, key, value, rules);
    }

    let mut output = Vec::with_capacity(message.len() + 64);
    output.extend_from_slice(block.before);
    if !had_block {
        if !output.is_empty() && !output.ends_with(b"\n") {
            output.push(b'\n');
        }
        output.push(b'\n');
    }
    for line in &lines {
        match line {
            Line::Trailer {
                key,
                value,
                continuation,
            } => {
                config.format(key, value, &mut output);
                output.push(b'\n');
                for line in continuation {
                    output.extend_from_slice(line);
                    if !line.ends_with(b"\n") {
                        output.push(b'\n');
                    }
                }
            }
            Line::Other(line) => {
                output.extend_from_slice(line);
                if !line.ends_with(b"\n") {
                    output.push(b'\n');
                }
            }
        }
    }
    output.extend_from_slice(block.after);
    output
}

fn add(lines: &mut Vec<Line<'_>>, key: Vec<u8>, value: Vec<u8>, rules: Rules) {
    let matching: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Trailer { key: existing, .. } if same_key(existing, &key) => Some(index),
            _ => None,
        })
        .collect();
    let towards_end = matches!(rules.placement, Placement::End | Placement::After);
    let anchor = if towards_end {
        matching.last()
    } else {
        matching.first()
    }
    .copied();
    let position = |lines: &[Line<'_>], anchor: Option<usize>| match (rules.placement, anchor) {
        (Placement::After, Some(anchor)) => anchor + 1,
        (Placement::Before, Some(anchor)) => anchor,
        (Placement::End | Placement::After, _) => lines.len(),
        (Placement::Start | Placement::Before, _) => 0,
    };
    let new = Line::Trailer {
        key,
        value,
        continuation: Vec::new(),
    };
    let same = |line: &Line<'_>| match (line, &new) {
        (
            Line::Trailer {
                key,
                value,
                continuation,
            },
            Line::Trailer {
                key: new_key,
                value: new_value,
                ..
            },
            // An existing trailer with continuation lines never equals a new one-line trailer.
        ) => same_key(key, new_key) && value == new_value && continuation.is_empty(),
        _ => false,
    };
    let Some(anchor) = anchor else {
        if rules.if_missing == IfMissing::Add {
            let at = position(lines, None);
            lines.insert(at, new);
        }
        return;
    };
    match rules.if_exists {
        IfExists::DoNothing => {}
        IfExists::Add => {
            let at = position(lines, Some(anchor));
            lines.insert(at, new);
        }
        IfExists::AddIfDifferent => {
            if !matching.iter().any(|&index| same(&lines[index])) {
                let at = position(lines, Some(anchor));
                lines.insert(at, new);
            }
        }
        IfExists::AddIfDifferentNeighbor => {
            let at = position(lines, Some(anchor));
            let neighbor = if towards_end {
                at.checked_sub(1)
            } else {
                Some(at)
            };
            if !neighbor
                .and_then(|index| lines.get(index))
                .is_some_and(same)
            {
                lines.insert(at, new);
            }
        }
        IfExists::Replace => {
            lines.remove(anchor);
            let at = match rules.placement {
                Placement::After | Placement::Before => anchor,
                Placement::End | Placement::Start => position(lines, None),
            };
            lines.insert(at, new);
        }
    }
}
