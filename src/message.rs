//! Commit message text: cleanup before committing, and trailers.
//!
//! Git tidies a commit message before storing it. [`cleanup`] does the same to message bytes:
//! [`Cleanup::Whitespace`] removes trailing whitespace on each line, collapses runs of blank lines
//! and drops leading and trailing blank lines; [`Cleanup::Strip`] also removes comment lines; and
//! [`Cleanup::Scissors`] cuts the message at the scissors line Git writes into a verbose commit
//! template. [`Cleanup::for_commit`] reads `commit.cleanup` and resolves Git's `default` mode,
//! and [`CommentPrefix::from_config`] reads `core.commentChar` and `core.commentString`.
//!
//! ```
//! use girt::message::{Cleanup, CommentPrefix, cleanup};
//!
//! let edited = b"\nFix the parser  \n\n\n# Please enter the commit message.\n";
//! let comment = CommentPrefix::default();
//! assert_eq!(
//!     cleanup(edited, Cleanup::Strip, &comment),
//!     b"Fix the parser\n"
//! );
//!
//! // A message given on the command line is not edited, so Git keeps its `#` lines.
//! let given = b"Count #1 and #2\n# not a comment here\n";
//! assert_eq!(
//!     cleanup(given, Cleanup::Whitespace, &comment),
//!     given.as_slice()
//! );
//! ```
//!
//! Trailers are the `Key: value` lines at the end of a message, such as `Signed-off-by:`.
//! [`trailers`] lists them as `git interpret-trailers --parse` does and [`add_trailers`] adds new
//! ones following the `trailer.*` rules that [`TrailerConfig`] reads.
//!
//! An empty [`cleanup`] result means the message had no content; Git aborts the commit in that
//! case, and so should callers that commit edited messages. The functions here are pure: they read
//! no files and run no editor.

use crate::Config;
use crate::config::InvalidValue;

mod trailers;
pub use trailers::{
    IfExists, IfMissing, Placement, Trailer, TrailerCommandError, TrailerConfig, TrailerKey,
    add_trailers, trailers,
};

/// How [`cleanup`] tidies a message, as Git's `commit.cleanup` modes do.
///
/// Git's `default` mode is not a variant: it means [`Self::Strip`] for a message the user edited
/// and [`Self::Whitespace`] otherwise. [`Self::for_commit`] makes that choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cleanup {
    /// Keep the message byte for byte.
    Verbatim,
    /// Remove trailing whitespace, collapse blank lines and end with one newline.
    Whitespace,
    /// [`Self::Whitespace`], and remove every line that starts with the comment prefix.
    Strip,
    /// [`Self::Whitespace`], and drop the scissors line and everything after it.
    ///
    /// Comment lines above the scissors line are kept.
    Scissors,
}

impl Cleanup {
    /// Reads `commit.cleanup` and picks the mode Git uses for a commit.
    ///
    /// `edited` says whether the user edited the message in an editor. Git's `default` mode, and
    /// an unset variable, mean [`Self::Strip`] for an edited message and [`Self::Whitespace`]
    /// otherwise. `scissors` applies only to an edited message; otherwise it acts as
    /// [`Self::Whitespace`]. Mode names are case-sensitive, as in Git.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::message::Cleanup;
    ///
    /// let unset = Config::parse(b"")?;
    /// assert_eq!(Cleanup::for_commit(&unset, true)?, Cleanup::Strip);
    /// assert_eq!(Cleanup::for_commit(&unset, false)?, Cleanup::Whitespace);
    ///
    /// let scissors = Config::parse(b"[commit]\ncleanup = scissors\n")?;
    /// assert_eq!(Cleanup::for_commit(&scissors, true)?, Cleanup::Scissors);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] when `commit.cleanup` is not one of `default`, `verbatim`,
    /// `whitespace`, `strip` or `scissors`, including an implicit assignment.
    pub fn for_commit(config: &Config, edited: bool) -> Result<Self, InvalidValue> {
        let edited_mode = |mode| if edited { mode } else { Self::Whitespace };
        match config.value("commit", None, "cleanup") {
            None | Some(Some(b"default")) => Ok(edited_mode(Self::Strip)),
            Some(Some(b"verbatim")) => Ok(Self::Verbatim),
            Some(Some(b"whitespace")) => Ok(Self::Whitespace),
            Some(Some(b"strip")) => Ok(Self::Strip),
            Some(Some(b"scissors")) => Ok(edited_mode(Self::Scissors)),
            Some(value) => Err(InvalidValue::new("commit", "cleanup", value)),
        }
    }
}

/// The bytes that start a comment line in a commit message; `#` by default.
///
/// A line is a comment when it starts with these bytes at its first column. The prefix is never
/// empty and never contains a newline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommentPrefix(Vec<u8>);

/// Characters `core.commentChar=auto` chooses from, in order.
const AUTO_CANDIDATES: &[u8] = b"#;@!$%^&|:";

impl CommentPrefix {
    /// Uses `prefix` as the comment prefix, or returns `None` if it is empty or has a newline.
    ///
    /// ```
    /// use girt::message::CommentPrefix;
    ///
    /// assert_eq!(CommentPrefix::new(b"//").unwrap().as_bytes(), b"//");
    /// assert!(CommentPrefix::new(b"").is_none());
    /// assert!(CommentPrefix::new(b"a\nb").is_none());
    /// ```
    pub fn new(prefix: &[u8]) -> Option<Self> {
        (!prefix.is_empty() && !prefix.contains(&b'\n')).then(|| Self(prefix.to_vec()))
    }

    /// Picks the first of `#;@!$%^&|:` that starts no line of `message`, or `None` if every one
    /// does, where Git refuses to choose.
    ///
    /// This is the rule for `core.commentChar=auto`, which Git applies to the message it places in
    /// the editor. Pass that message. Git deprecates `auto`. Before choosing, Git 2.56's commit
    /// command drops comment lines that start with `#` at the end of a message read with `-F`;
    /// callers reproducing `git commit -F` should drop those lines first.
    ///
    /// ```
    /// use girt::message::CommentPrefix;
    ///
    /// let prefix = CommentPrefix::auto(b"Subject\n\n#1 is fixed\n;semicolon\n").unwrap();
    /// assert_eq!(prefix.as_bytes(), b"@");
    /// assert!(CommentPrefix::auto(b"#\n;\n@\n!\n$\n%\n^\n&\n|\n:\n").is_none());
    /// ```
    pub fn auto(message: &[u8]) -> Option<Self> {
        let mut used = [false; 256];
        for line in message.split(|&byte| byte == b'\n') {
            if let Some(&first) = line.first() {
                used[usize::from(first)] = true;
            }
        }
        AUTO_CANDIDATES
            .iter()
            .find(|&&candidate| !used[usize::from(candidate)])
            .map(|&chosen| Self(vec![chosen]))
    }

    /// Reads the comment prefix from `core.commentChar` and `core.commentString`.
    ///
    /// The two variables are one setting: the last assignment of either wins. Unset means `#`.
    /// `auto` returns `Ok(None)`: the caller chooses with [`Self::auto`] once it has the message
    /// it will show in the editor. Git's `stripspace`, which has no such message, uses `#`.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::message::CommentPrefix;
    ///
    /// let config = Config::parse(b"[core]\ncommentChar = \";\"\ncommentString = //\n")?;
    /// let prefix = CommentPrefix::from_config(&config)?.expect("not auto");
    /// assert_eq!(prefix.as_bytes(), b"//");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for an implicit assignment, an empty value or a value containing a
    /// newline in any assignment of either variable, not only the last, as Git refuses them.
    pub fn from_config(config: &Config) -> Result<Option<Self>, InvalidValue> {
        let mut prefix = Some(Self::default());
        // Git checks every assignment as it reads it, so an invalid earlier one fails too.
        for entry in config.entries().iter().filter(|entry| {
            entry.section.eq_ignore_ascii_case(b"core")
                && entry.subsection.is_none()
                && (entry.name.eq_ignore_ascii_case(b"commentchar")
                    || entry.name.eq_ignore_ascii_case(b"commentstring"))
        }) {
            let name = String::from_utf8_lossy(&entry.name).to_ascii_lowercase();
            prefix = match entry.value.as_deref() {
                Some(b"auto") => None,
                Some(value) => Some(
                    Self::new(value)
                        .ok_or_else(|| InvalidValue::new("core", &name, Some(value)))?,
                ),
                None => return Err(InvalidValue::new("core", &name, None)),
            };
        }
        Ok(prefix)
    }

    /// Returns the prefix bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn starts(&self, line: &[u8]) -> bool {
        line.starts_with(&self.0)
    }
}

impl Default for CommentPrefix {
    fn default() -> Self {
        Self(b"#".to_vec())
    }
}

/// The scissors marker that follows the comment prefix and one space on a scissors line.
const SCISSORS: &[u8] = b"------------------------ >8 ------------------------";

/// Tidies a commit message as Git does before committing it.
///
/// Lines are split on `\n`. For every mode except [`Cleanup::Verbatim`], spaces, tabs, carriage
/// returns and newlines at the end of each line are removed; blank lines at the start and end are
/// dropped, runs of blank lines become one, and a non-empty result ends with exactly one newline.
/// Other bytes, including NUL, form-feed and non-ASCII whitespace, are kept.
///
/// [`Cleanup::Strip`] removes lines that start with `comment` before blank lines are collapsed.
/// [`Cleanup::Scissors`] finds the first line that is `comment`, a space and the scissors marker
/// `------------------------ >8 ------------------------`, optionally followed by trailing
/// whitespace, and drops it and every following line. `comment` is unused by the other modes.
///
/// ```
/// use girt::message::{Cleanup, CommentPrefix, cleanup};
///
/// let comment = CommentPrefix::default();
/// let template =
///     b"Subject\n# keep\n# ------------------------ >8 ------------------------\ndiff\n";
/// assert_eq!(
///     cleanup(template, Cleanup::Scissors, &comment),
///     b"Subject\n# keep\n"
/// );
/// assert_eq!(cleanup(b"\n\n", Cleanup::Whitespace, &comment), b"");
/// ```
pub fn cleanup(message: &[u8], mode: Cleanup, comment: &CommentPrefix) -> Vec<u8> {
    if mode == Cleanup::Verbatim {
        return message.to_vec();
    }
    let mut output = Vec::with_capacity(message.len() + 1);
    let mut pending_blank = false;
    for line in message.split(|&byte| byte == b'\n') {
        if mode == Cleanup::Scissors && is_scissors(line, comment) {
            break;
        }
        if mode == Cleanup::Strip && comment.starts(line) {
            continue;
        }
        let content = trim_end(line);
        if content.is_empty() {
            pending_blank = !output.is_empty();
            continue;
        }
        if pending_blank {
            output.push(b'\n');
            pending_blank = false;
        }
        output.extend_from_slice(content);
        output.push(b'\n');
    }
    output
}

fn is_scissors(line: &[u8], comment: &CommentPrefix) -> bool {
    line.strip_prefix(comment.as_bytes())
        .and_then(|rest| rest.strip_prefix(b" "))
        .is_some_and(|rest| trim_end(rest) == SCISSORS)
}

fn trim_end(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        .map_or(0, |last| last + 1);
    &line[..end]
}

#[cfg(test)]
mod tests;
