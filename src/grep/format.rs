use std::io::{self, Write};

use super::{FileMatches, LineKind, Searcher};

/// What `git grep` prints for each file.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Listing {
    /// The shown lines, or `Binary file <name> matches` for a binary file.
    #[default]
    Lines,
    /// The number of selected lines, for files with any (`-c`).
    Count,
    /// The names of files with a selected line (`-l`).
    FilesWithMatches,
    /// The names of files without one (`-L`).
    FilesWithoutMatch,
    /// Nothing (`-q`).
    Quiet,
}

/// Writes search results byte for byte as `git grep` prints them, without color.
///
/// Feed every searched file to [`Self::write`] in order; the writer keeps the state Git uses to
/// place `--` group separators and `--break` blank lines across files. Names are passed in as
/// Git shows them, such as `HEAD:src/lib.rs` or `../README`.
///
/// Git with more than one thread can drop a `Binary file … matches` line that would be printed
/// first when context or `--break` is in use; this writer prints what single-threaded Git
/// (`--threads=1`) prints.
///
/// ```
/// use girt::grep::{Expression, GitFormat, Matching, Options, Query, Searcher};
///
/// let query = Query::new(&Expression::pattern("b"), Matching::new())?;
/// let mut searcher = Searcher::new(query, Options::new());
/// let content = b"a\nb\n";
/// let matches = searcher.search(b"notes", content)?;
///
/// let mut out = Vec::new();
/// let mut format = GitFormat::new().with_line_numbers(true);
/// format.write(&mut out, b"notes", content, &matches, &searcher)?;
/// assert_eq!(out, b"notes:2:b\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct GitFormat {
    listing: Listing,
    names: bool,
    line_numbers: bool,
    column: bool,
    only_matching: bool,
    heading: bool,
    break_between: bool,
    null: bool,
    /// Whether a text line has been printed, so the next file's first group needs `--`.
    printed_lines: bool,
    /// Whether a file has printed lines, so `--break` separates the next one; binary messages do
    /// not count.
    printed_file: bool,
}

impl Default for GitFormat {
    fn default() -> Self {
        Self::new()
    }
}

impl GitFormat {
    /// Lines prefixed with file names, as `git grep` prints without options.
    pub fn new() -> Self {
        Self {
            listing: Listing::Lines,
            names: true,
            line_numbers: false,
            column: false,
            only_matching: false,
            heading: false,
            break_between: false,
            null: false,
            printed_lines: false,
            printed_file: false,
        }
    }

    /// Chooses what is printed for each file.
    #[must_use]
    pub fn with_listing(mut self, listing: Listing) -> Self {
        self.listing = listing;
        self
    }

    /// Prefixes lines and counts with the file name (true by default; `-h` turns it off).
    #[must_use]
    pub fn with_names(mut self, names: bool) -> Self {
        self.names = names;
        self
    }

    /// Prefixes lines with their numbers (`-n`).
    #[must_use]
    pub fn with_line_numbers(mut self, line_numbers: bool) -> Self {
        self.line_numbers = line_numbers;
        self
    }

    /// Prefixes selected lines with the 1-based byte column of their first match (`--column`).
    #[must_use]
    pub fn with_column(mut self, column: bool) -> Self {
        self.column = column;
        self
    }

    /// Prints each match of a selected line on its own line instead of the line (`-o`).
    ///
    /// With `-v`, selected lines are printed whole. Context lines are not
    /// printed; combine this with context options only if their `--` separators are wanted.
    #[must_use]
    pub fn with_only_matching(mut self, only_matching: bool) -> Self {
        self.only_matching = only_matching;
        self
    }

    /// Prints the file name once above its lines instead of on each line (`--heading`).
    #[must_use]
    pub fn with_heading(mut self, heading: bool) -> Self {
        self.heading = heading;
        self
    }

    /// Prints an empty line between files (`--break`).
    #[must_use]
    pub fn with_break(mut self, break_between: bool) -> Self {
        self.break_between = break_between;
        self
    }

    /// Ends file names with NUL instead of `:`, `-`, `=` or LF (`-z`).
    #[must_use]
    pub fn with_null(mut self, null: bool) -> Self {
        self.null = null;
        self
    }

    /// Writes one file's result.
    ///
    /// `searcher` must be the searcher that produced `matches`; its query finds the matches `-o`
    /// and `--column` report and its options decide whether `--` separates groups.
    ///
    /// # Errors
    ///
    /// Returns the writer's error.
    pub fn write(
        &mut self,
        out: &mut impl Write,
        name: &[u8],
        content: &[u8],
        matches: &FileMatches,
        searcher: &Searcher,
    ) -> io::Result<()> {
        let end = if self.null { b'\0' } else { b'\n' };
        match self.listing {
            Listing::Quiet => Ok(()),
            Listing::FilesWithMatches if matches.is_match() => {
                out.write_all(name)?;
                out.write_all(&[end])
            }
            Listing::FilesWithoutMatch if !matches.is_match() => {
                out.write_all(name)?;
                out.write_all(&[end])
            }
            Listing::FilesWithMatches | Listing::FilesWithoutMatch => Ok(()),
            Listing::Count if matches.is_match() => {
                if self.names {
                    out.write_all(name)?;
                    out.write_all(if self.null { b"\0" } else { b":" })?;
                }
                writeln!(out, "{}", matches.count)
            }
            Listing::Count => Ok(()),
            Listing::Lines if !matches.is_match() => Ok(()),
            Listing::Lines if matches.binary => {
                out.write_all(b"Binary file ")?;
                out.write_all(name)?;
                out.write_all(b" matches\n")
            }
            Listing::Lines => self.write_lines(out, name, content, matches, searcher),
        }
    }

    fn write_lines(
        &mut self,
        out: &mut impl Write,
        name: &[u8],
        content: &[u8],
        matches: &FileMatches,
        searcher: &Searcher,
    ) -> io::Result<()> {
        if self.break_between && self.printed_file {
            out.write_all(b"\n")?;
        }
        self.printed_file = true;
        let separators = searcher.options().has_context();
        let mut previous: Option<usize> = None;
        for line in &matches.lines {
            if separators {
                let gap = match previous {
                    Some(previous) => line.number != previous + 1,
                    // `--break`'s blank line replaces the separator between files.
                    None => self.printed_lines && !self.break_between,
                };
                if gap {
                    out.write_all(b"--\n")?;
                }
            }
            if previous.is_none() && self.heading {
                out.write_all(name)?;
                out.write_all(b"\n")?;
            }
            previous = Some(line.number);
            self.printed_lines = true;
            let text = content.get(line.range.clone()).unwrap_or_default();
            let mark = match line.kind {
                LineKind::Selected => b':',
                LineKind::Context => b'-',
                LineKind::Function => b'=',
            };
            if line.kind != LineKind::Selected {
                if !self.only_matching {
                    self.prefix(out, name, line.number, None, mark)?;
                    out.write_all(text)?;
                    out.write_all(b"\n")?;
                }
                continue;
            }
            let found = if self.only_matching || self.column {
                searcher.query().matches(text)
            } else {
                Vec::new()
            };
            if self.only_matching && !searcher.options().invert() {
                for span in found {
                    self.prefix(out, name, line.number, Some(span.start + 1), mark)?;
                    out.write_all(text.get(span).unwrap_or_default())?;
                    out.write_all(b"\n")?;
                }
            } else {
                let column = found.first().map_or(1, |span| span.start + 1);
                self.prefix(out, name, line.number, Some(column), mark)?;
                out.write_all(text)?;
                out.write_all(b"\n")?;
            }
        }
        Ok(())
    }

    fn prefix(
        &self,
        out: &mut impl Write,
        name: &[u8],
        number: usize,
        column: Option<usize>,
        mark: u8,
    ) -> io::Result<()> {
        let mark = if self.null { b'\0' } else { mark };
        if self.names && !self.heading {
            out.write_all(name)?;
            out.write_all(&[mark])?;
        }
        if self.line_numbers {
            write!(out, "{number}")?;
            out.write_all(&[mark])?;
        }
        if self.column
            && let Some(column) = column
        {
            write!(out, "{column}")?;
            out.write_all(&[mark])?;
        }
        Ok(())
    }
}
