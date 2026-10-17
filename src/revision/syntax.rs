//! Revision and range syntax, parsed without reading a repository.

use std::fmt;
use std::str::FromStr;

use crate::ObjectKind;

/// A parsed revision expression naming one object, such as `main~2` or `v1.0^{tree}:README`.
///
/// A revision is a [`Base`] that names a starting object, a sequence of [`Step`]s applied in
/// order, and an optional path looked up in the resulting tree. Parse it with [`str::parse`] and
/// resolve it with [`super::Resolver::resolve`].
///
/// ```
/// use girt::revision::{Base, Revision, Step};
/// let revision: Revision = "main~2^2".parse()?;
/// assert_eq!(revision.base(), &Base::Name("main".into()));
/// assert_eq!(revision.steps(), [Step::Ancestor(2), Step::Parent(2)]);
/// assert_eq!(revision.path(), None);
///
/// let file: Revision = "HEAD:src/lib.rs".parse()?;
/// assert_eq!(file.path(), Some("src/lib.rs"));
/// # Ok::<(), girt::revision::ParseRevisionError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Revision {
    base: Base,
    steps: Vec<Step>,
    path: Option<String>,
}

impl Revision {
    /// `HEAD`, the revision an omitted range side stands for.
    pub fn head() -> Self {
        Self {
            base: Base::Head,
            steps: Vec::new(),
            path: None,
        }
    }

    /// The starting object.
    pub fn base(&self) -> &Base {
        &self.base
    }

    /// Navigation applied to the base, in order.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The path after `:`, looked up in the tree of the stepped revision. `Some("")` names that
    /// tree itself.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

/// The object a [`Revision`] starts from.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Base {
    /// `@`: the current `HEAD`.
    Head,
    /// A reference name or full object ID, expanded at resolution in Git's order.
    Name(String),
    /// `name@{N}` or `@{N}`: the value `index` updates before the reference's latest one.
    ///
    /// Without a name, the reflog of the branch `HEAD` points to is used, or `HEAD`'s own
    /// reflog when it is detached.
    Reflog {
        /// The reference, or `None` for the current branch.
        name: Option<String>,
        /// Entries back from the newest; `0` is the newest entry's value.
        index: usize,
    },
    /// `name@{upstream}` or `@{u}`: the remote-tracking branch the branch merges from.
    Upstream {
        /// The branch, or `None` for the current branch.
        branch: Option<String>,
    },
    /// `name@{push}`: the remote-tracking branch the branch would push to.
    Push {
        /// The branch, or `None` for the current branch.
        branch: Option<String>,
    },
}

/// One navigation step after a [`Base`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Step {
    /// `~N`: follow first parents `N` times. `~0` peels to a commit.
    Ancestor(usize),
    /// `^N`: the `N`th parent, counting from 1. `^0` peels to a commit.
    Parent(usize),
    /// `^{...}`: peel or check the object kind.
    Peel(Peel),
}

/// The target of a `^{...}` step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Peel {
    /// `^{}`: follow annotated tags to the first object that is not a tag.
    Tags,
    /// `^{object}`: require that the object exists, without peeling.
    Object,
    /// `^{commit}`, `^{tree}`, `^{blob}` or `^{tag}`: peel until reaching that kind. Commits
    /// peel to their tree; `^{tag}` requires the object itself to be a tag.
    Kind(ObjectKind),
}

/// A commit-set expression for history commands, such as `main..topic`.
///
/// Several ranges combine into one [`super::Selection`]: the commits reachable from any included
/// revision and from no excluded one.
///
/// ```
/// use girt::revision::{Range, Revision};
/// let range: Range = "main...topic".parse()?;
/// assert_eq!(
///     range,
///     Range::Symmetric {
///         left: "main".parse()?,
///         right: "topic".parse()?
///     }
/// );
/// let since: Range = "v1.0..".parse()?;
/// assert_eq!(
///     since,
///     Range::Between {
///         from: "v1.0".parse()?,
///         to: Revision::head()
///     }
/// );
/// # Ok::<(), girt::revision::ParseRevisionError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Range {
    /// `rev`: commits reachable from `rev`.
    Include(Revision),
    /// `^rev`: exclude commits reachable from `rev`.
    Exclude(Revision),
    /// `from..to`: reachable from `to` but not from `from`.
    Between {
        /// Excluded side.
        from: Revision,
        /// Included side.
        to: Revision,
    },
    /// `left...right`: reachable from either side but not from both.
    Symmetric {
        /// Left side.
        left: Revision,
        /// Right side.
        right: Revision,
    },
}

/// Text that is not a revision or range expression.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid revision `{text}`: {reason}")]
pub struct ParseRevisionError {
    text: String,
    reason: Reason,
}

impl ParseRevisionError {
    fn new(text: &str, reason: Reason) -> Self {
        Self {
            text: text.to_owned(),
            reason,
        }
    }

    /// The rejected text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the text uses recognised syntax girt does not support yet, rather than being
    /// malformed.
    pub fn is_unsupported(&self) -> bool {
        matches!(self.reason, Reason::Unsupported(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Reason {
    Empty,
    Malformed(&'static str),
    Unsupported(&'static str),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty revision"),
            Self::Malformed(what) => f.write_str(what),
            Self::Unsupported(what) => write!(f, "{what} are not supported yet"),
        }
    }
}

impl FromStr for Revision {
    type Err = ParseRevisionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = |reason| ParseRevisionError::new(text, reason);
        let (expression, path) = match path_separator(text) {
            Some(0) => {
                return Err(error(Reason::Unsupported(
                    "index paths and message searches (`:path`, `:/text`)",
                )));
            }
            Some(colon) => (&text[..colon], Some(&text[colon + 1..])),
            None => (text, None),
        };
        if let Some(path) = path
            && (path.starts_with("./") || path.starts_with("../") || path == "." || path == "..")
        {
            return Err(error(Reason::Unsupported(
                "paths relative to the working directory",
            )));
        }
        let end = expression
            .find(['~', '^'])
            .into_iter()
            .chain(expression.find("@{"))
            .min()
            .unwrap_or(expression.len());
        let (name, mut rest) = expression.split_at(end);
        let base = if let Some(selector) = rest.strip_prefix("@{") {
            let close = selector
                .find('}')
                .ok_or_else(|| error(Reason::Malformed("unclosed `@{`")))?;
            rest = &selector[close + 1..];
            let name = match name {
                "" | "@" => None,
                name => Some(name.to_owned()),
            };
            at_selector(&selector[..close], name).map_err(error)?
        } else {
            match name {
                "" => return Err(error(Reason::Empty)),
                "@" => Base::Head,
                name => Base::Name(name.to_owned()),
            }
        };
        let steps = steps(rest).map_err(error)?;
        Ok(Self {
            base,
            steps,
            path: path.map(str::to_owned),
        })
    }
}

impl FromStr for Range {
    type Err = ParseRevisionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(excluded) = text.strip_prefix('^') {
            return Ok(Self::Exclude(excluded.parse()?));
        }
        // Reference names cannot contain `..`, so the first one splits a range unless it is part
        // of a path after `:`.
        let range_end = path_separator(text).unwrap_or(text.len());
        let side = |side: &str| {
            if side.is_empty() {
                Ok(Revision::head())
            } else {
                side.parse()
            }
        };
        if let Some(dots) = text[..range_end].find("..") {
            let (left, right) = (&text[..dots], &text[dots + 2..]);
            if let Some(right) = right.strip_prefix('.') {
                return Ok(Self::Symmetric {
                    left: side(left)?,
                    right: side(right)?,
                });
            }
            return Ok(Self::Between {
                from: side(left)?,
                to: side(right)?,
            });
        }
        if text.ends_with("^@") || text.ends_with("^!") || text.contains("^-") {
            return Err(ParseRevisionError::new(
                text,
                Reason::Unsupported("`^@`, `^!` and `^-` parent shorthands"),
            ));
        }
        Ok(Self::Include(text.parse()?))
    }
}

/// The byte offset of the `:` that starts a path, skipping `:` inside `{...}`.
fn path_separator(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn at_selector(selector: &str, name: Option<String>) -> Result<Base, Reason> {
    if selector.eq_ignore_ascii_case("u") || selector.eq_ignore_ascii_case("upstream") {
        return Ok(Base::Upstream { branch: name });
    }
    if selector.eq_ignore_ascii_case("push") {
        return Ok(Base::Push { branch: name });
    }
    if selector.starts_with('-') {
        return Err(Reason::Unsupported("previous checkouts (`@{-N}`)"));
    }
    if !selector.is_empty() && selector.bytes().all(|b| b.is_ascii_digit()) {
        let index = selector
            .parse()
            .map_err(|_| Reason::Malformed("reflog index out of range"))?;
        return Ok(Base::Reflog { name, index });
    }
    Err(Reason::Unsupported("dated reflog selectors (`@{date}`)"))
}

fn steps(mut rest: &str) -> Result<Vec<Step>, Reason> {
    let mut steps = Vec::new();
    while let Some(operator) = rest.chars().next() {
        rest = &rest[operator.len_utf8()..];
        if operator == '^'
            && let Some(inner) = rest.strip_prefix('{')
        {
            let close = inner.find('}').ok_or(Reason::Malformed("unclosed `^{`"))?;
            let peel = match &inner[..close] {
                "" => Peel::Tags,
                "object" => Peel::Object,
                "commit" => Peel::Kind(ObjectKind::Commit),
                "tree" => Peel::Kind(ObjectKind::Tree),
                "blob" => Peel::Kind(ObjectKind::Blob),
                "tag" => Peel::Kind(ObjectKind::Tag),
                text if text.starts_with('/') => {
                    return Err(Reason::Unsupported("message searches (`^{/text}`)"));
                }
                _ => return Err(Reason::Malformed("unknown `^{...}` object kind")),
            };
            steps.push(Step::Peel(peel));
            rest = &inner[close + 1..];
            continue;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let count = if digits == 0 {
            1
        } else {
            rest[..digits]
                .parse()
                .map_err(|_| Reason::Malformed("step count out of range"))?
        };
        rest = &rest[digits..];
        steps.push(match operator {
            '~' => Step::Ancestor(count),
            '^' => Step::Parent(count),
            '@' | '!' | '-' => {
                return Err(Reason::Unsupported("`^@`, `^!` and `^-` parent shorthands"));
            }
            _ => return Err(Reason::Malformed("expected `~`, `^` or `:` after a name")),
        });
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Revision {
        text.parse().unwrap()
    }

    fn name(text: &str) -> Base {
        Base::Name(text.to_owned())
    }

    #[test]
    fn parses_steps_in_order() {
        let revision = parse("v1.0^{}~3^2^^{tree}");
        assert_eq!(revision.base(), &name("v1.0"));
        assert_eq!(
            revision.steps(),
            [
                Step::Peel(Peel::Tags),
                Step::Ancestor(3),
                Step::Parent(2),
                Step::Parent(1),
                Step::Peel(Peel::Kind(ObjectKind::Tree)),
            ]
        );
        assert_eq!(parse("HEAD~").steps(), [Step::Ancestor(1)]);
        assert_eq!(parse("HEAD^0").steps(), [Step::Parent(0)]);
    }

    #[test]
    fn parses_at_forms() {
        assert_eq!(parse("@").base(), &Base::Head);
        assert_eq!(parse("@~2").steps(), [Step::Ancestor(2)]);
        assert_eq!(
            parse("@{2}").base(),
            &Base::Reflog {
                name: None,
                index: 2
            }
        );
        assert_eq!(
            parse("main@{0}~1").base(),
            &Base::Reflog {
                name: Some("main".into()),
                index: 0
            }
        );
        assert_eq!(parse("@{U}").base(), &Base::Upstream { branch: None });
        assert_eq!(
            parse("topic@{upstream}").base(),
            &Base::Upstream {
                branch: Some("topic".into())
            }
        );
        assert_eq!(parse("@{push}").base(), &Base::Push { branch: None });
    }

    #[test]
    fn parses_paths() {
        assert_eq!(parse("HEAD:").path(), Some(""));
        assert_eq!(parse("HEAD^{tree}:a/b").path(), Some("a/b"));
        assert_eq!(parse("main:a..b").path(), Some("a..b"));
    }

    #[test]
    fn parses_ranges() {
        let range = |text: &str| text.parse::<Range>().unwrap();
        assert_eq!(range("^main"), Range::Exclude(parse("main")));
        assert_eq!(
            range("a..b"),
            Range::Between {
                from: parse("a"),
                to: parse("b")
            }
        );
        assert_eq!(
            range("..b"),
            Range::Between {
                from: Revision::head(),
                to: parse("b")
            }
        );
        assert_eq!(
            range("a..."),
            Range::Symmetric {
                left: parse("a"),
                right: Revision::head()
            }
        );
        assert_eq!(range("main:x..y"), Range::Include(parse("main:x..y")));
    }

    #[test]
    fn rejects_malformed_and_unsupported() {
        for text in [
            "",
            "~1",
            "main^{",
            "main@{1",
            "main^{nope}",
            "main~x",
            "HEAD~é",
        ] {
            let error = text.parse::<Revision>().unwrap_err();
            assert!(!error.is_unsupported(), "{text}: {error}");
        }
        for text in [
            ":path",
            ":/fix",
            "main^{/fix}",
            "@{-1}",
            "main@{yesterday}",
            "HEAD:./x",
        ] {
            let error = text.parse::<Revision>().unwrap_err();
            assert!(error.is_unsupported(), "{text}: {error}");
        }
        for text in ["main^@", "main^!", "main^-"] {
            assert!(
                text.parse::<Range>().unwrap_err().is_unsupported(),
                "{text}"
            );
        }
    }
}
