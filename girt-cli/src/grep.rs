//! `girt grep`: search file contents, printing what `git grep` prints.
use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::ops::ControlFlow;
use std::path::Path;
use std::process::ExitCode;

use color_eyre::eyre::{Result, WrapErr as _, bail, eyre};
use girt::grep::{
    Binary, Expression, GitFormat, Listing, Matching, Options, Query, Scope, Searcher, Source,
    Syntax,
};
use girt::pathspec::{self, Pathspec};
use girt::{ObjectKind, Repository};

use crate::{Context, common};

/// Search tracked files, the index or trees for lines matching patterns (`git grep`).
///
/// Takes `git grep`'s options and prints its output. Patterns are POSIX basic (`-G`, default),
/// extended (`-E`) or fixed (`-F`) and combine with `-e`, `-f`, `--and`, `--or`, `--not`,
/// parentheses and `--all-match`. Selection: `-v`, `-i`, `-w`, `-a`, `-I`, `-m`, `--max-depth`,
/// `--cached`, `--untracked`, `--no-exclude-standard`, `--no-index`, revisions and pathspecs.
/// Output: `-n`, `--column`, `-h`, `-H`, `--full-name`, `-z`, `-o`, `-l`, `-L`, `-c`, `-q`,
/// `--heading`, `--break`, `-A`, `-B`, `-C`, `-NUM`, `-p` and `-W`. Back-references, `-P`,
/// `--textconv`, `--recurse-submodules`, `-O` and color are refused.
#[derive(Debug, clap::Args)]
#[command(disable_help_flag = true)]
pub struct Args {
    /// `git grep` options, patterns, revisions and pathspecs.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    args: Vec<OsString>,
    #[arg(skip)]
    raw: Option<Vec<OsString>>,
}

impl Args {
    /// Replaces the parsed arguments with those
    /// [`common::split_raw_args`](crate::common::split_raw_args) took before clap ran.
    pub fn set_raw(&mut self, raw: Vec<OsString>) {
        self.raw = Some(raw);
    }
}

/// Everything the command line chose.
#[derive(Default)]
struct Parsed {
    atoms: Vec<Token>,
    patterns_given: bool,
    matching: Matching,
    options: Options,
    format: GitFormat,
    listing: Listing,
    cached: bool,
    no_index: bool,
    untracked: bool,
    exclude_standard: Option<bool>,
    max_depth: Option<usize>,
    full_name: bool,
    all_match: bool,
    names: Option<bool>,
    only_matching: bool,
    null: bool,
    context: (Option<usize>, Option<usize>),
    positional: Vec<OsString>,
    paths: Vec<OsString>,
}

enum Token {
    Pattern(Vec<u8>),
    And,
    Or,
    Not,
    Open,
    Close,
}

pub fn run(args: Args, context: &Context) -> Result<ExitCode> {
    let raw = args.raw.unwrap_or(args.args);
    let mut parsed = parse(raw)?;
    if !parsed.patterns_given {
        if parsed.positional.is_empty() {
            bail!("no pattern given");
        }
        let pattern = parsed.positional.remove(0);
        parsed
            .atoms
            .push(Token::Pattern(pattern.as_encoded_bytes().to_vec()));
    }
    if parsed.only_matching && parsed.options_have_context() {
        bail!("-o with context (-A, -B, -C or -W) is unsupported");
    }
    let expression = expression(&mut parsed.atoms.drain(..).peekable())?;
    let Some(expression) = expression else {
        bail!("no pattern given");
    };
    let matching = parsed
        .matching
        .with_bytes(!utf8_locale(&context.environment));
    let query = Query::new(&expression, matching)?.with_all_match(parsed.all_match);
    let format = parsed
        .format
        .clone()
        .with_listing(parsed.listing)
        .with_only_matching(parsed.only_matching)
        .with_names(parsed.names.unwrap_or(true));
    let mut out = common::stdout();
    let matched = if parsed.no_index {
        search_directory(&parsed, query, format, context, &mut out)?
    } else {
        search_repository(&parsed, query, format, context, &mut out)?
    };
    out.flush()?;
    Ok(if matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

impl Parsed {
    fn options_have_context(&self) -> bool {
        self.context.0.unwrap_or(0) > 0
            || self.context.1.unwrap_or(0) > 0
            || self.options == self.options.with_function_body(true)
    }
}

fn parse(raw: Vec<OsString>) -> Result<Parsed> {
    let mut parsed = Parsed::default();
    let mut syntax = Syntax::Basic;
    let mut ignore_case = false;
    let mut word = false;
    let mut options = Options::new();
    let mut format = GitFormat::new();
    let mut args = raw.into_iter();
    let number = |value: Option<OsString>, option: &str| -> Result<usize> {
        let value = value.ok_or_else(|| eyre!("option `{option}` requires a value"))?;
        value
            .to_str()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| eyre!("option `{option}` expects a number"))
    };
    while let Some(arg) = args.next() {
        let Some(text) = arg.to_str().map(str::to_owned) else {
            parsed.positional.push(arg);
            continue;
        };
        if text == "--" {
            // A leading `--` only ends options: the pattern and revisions may follow it, and a
            // second `--` starts the paths.
            if !parsed.patterns_given && parsed.positional.is_empty() && parsed.atoms.is_empty() {
                for arg in args.by_ref() {
                    if arg == "--" {
                        parsed.paths.extend(args.by_ref());
                        break;
                    }
                    parsed.positional.push(arg);
                }
            } else {
                parsed.paths.extend(args.by_ref());
            }
            break;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(OsString::from(value))),
                None => (long, None),
            };
            let mut value = |option: &str| {
                inline
                    .clone()
                    .or_else(|| args.next())
                    .ok_or_else(|| eyre!("option `--{option}` requires a value"))
            };
            match name {
                "cached" => parsed.cached = true,
                "no-cached" => parsed.cached = false,
                "no-index" => parsed.no_index = true,
                "index" => parsed.no_index = false,
                "untracked" => parsed.untracked = true,
                "no-untracked" => parsed.untracked = false,
                "exclude-standard" => parsed.exclude_standard = Some(true),
                "no-exclude-standard" => parsed.exclude_standard = Some(false),
                "invert-match" => options = options.with_invert(true),
                "no-invert-match" => options = options.with_invert(false),
                "ignore-case" => ignore_case = true,
                "no-ignore-case" => ignore_case = false,
                "word-regexp" => word = true,
                "no-word-regexp" => word = false,
                "text" => options = options.with_binary(Binary::Text),
                "no-text" => options = options.with_binary(Binary::Report),
                "no-textconv" | "recursive" => {}
                "no-recursive" => parsed.max_depth = Some(0),
                "max-depth" => {
                    let depth: isize = value("max-depth")?
                        .to_str()
                        .and_then(|value| value.parse().ok())
                        .ok_or_else(|| eyre!("option `--max-depth` expects a number"))?;
                    parsed.max_depth = usize::try_from(depth).ok();
                }
                "extended-regexp" => syntax = Syntax::Extended,
                "basic-regexp" => syntax = Syntax::Basic,
                "fixed-strings" => syntax = Syntax::Fixed,
                "no-extended-regexp" | "no-basic-regexp" | "no-fixed-strings" => {
                    syntax = Syntax::Basic;
                }
                "line-number" => format = format.with_line_numbers(true),
                "no-line-number" => format = format.with_line_numbers(false),
                "column" => format = format.with_column(true),
                "no-column" => format = format.with_column(false),
                "full-name" => parsed.full_name = true,
                "no-full-name" => parsed.full_name = false,
                "null" => parsed.null = true,
                "no-null" => parsed.null = false,
                "only-matching" => parsed.only_matching = true,
                "no-only-matching" => parsed.only_matching = false,
                "files-with-matches" | "name-only" => parsed.listing = Listing::FilesWithMatches,
                "files-without-match" => parsed.listing = Listing::FilesWithoutMatch,
                "count" => parsed.listing = Listing::Count,
                "quiet" => parsed.listing = Listing::Quiet,
                "break" => format = format.with_break(true),
                "no-break" => format = format.with_break(false),
                "heading" => format = format.with_heading(true),
                "no-heading" => format = format.with_heading(false),
                "show-function" => options = options.with_function_name(true),
                "no-show-function" => options = options.with_function_name(false),
                "function-context" => options = options.with_function_body(true),
                "no-function-context" => options = options.with_function_body(false),
                "after-context" => {
                    parsed.context.1 =
                        Some(number(Some(value("after-context")?), "--after-context")?)
                }
                "before-context" => {
                    parsed.context.0 =
                        Some(number(Some(value("before-context")?), "--before-context")?)
                }
                "context" => {
                    let lines = number(Some(value("context")?), "--context")?;
                    parsed.context = (Some(lines), Some(lines));
                }
                "max-count" => {
                    let count: isize = value("max-count")?
                        .to_str()
                        .and_then(|value| value.parse().ok())
                        .ok_or_else(|| eyre!("option `--max-count` expects a number"))?;
                    options = options.with_max_count(usize::try_from(count).ok());
                }
                "no-max-count" => options = options.with_max_count(None),
                "threads" => {
                    value("threads")?;
                }
                "and" => parsed.atoms.push(Token::And),
                "or" => parsed.atoms.push(Token::Or),
                "not" => parsed.atoms.push(Token::Not),
                "all-match" => parsed.all_match = true,
                "no-all-match" => parsed.all_match = false,
                "color" if inline.as_deref().is_none_or(|value| value == "never") => {
                    if inline.is_none() {
                        bail!("color output is unsupported");
                    }
                }
                "no-color" => {}
                "perl-regexp" => bail!("Perl-compatible patterns (-P) are unsupported"),
                "textconv" => bail!("--textconv is unsupported: girt does not run commands"),
                "recurse-submodules" => bail!("--recurse-submodules is unsupported"),
                "open-files-in-pager" => bail!("--open-files-in-pager is unsupported"),
                _ => bail!("unknown option `--{name}`"),
            }
            continue;
        }
        if text == "(" {
            parsed.atoms.push(Token::Open);
            continue;
        }
        if text == ")" {
            parsed.atoms.push(Token::Close);
            continue;
        }
        let Some(shorts) = text.strip_prefix('-').filter(|shorts| !shorts.is_empty()) else {
            parsed.positional.push(arg);
            continue;
        };
        if shorts.bytes().all(|byte| byte.is_ascii_digit()) {
            let lines = number(Some(OsString::from(shorts)), "-NUM")?;
            parsed.context = (Some(lines), Some(lines));
            continue;
        }

        for (at, flag) in shorts.char_indices() {
            let rest = shorts.get(at + flag.len_utf8()..).unwrap_or_default();
            let mut value = || -> Result<OsString> {
                if rest.is_empty() {
                    args.next()
                        .ok_or_else(|| eyre!("switch `{flag}` requires a value"))
                } else {
                    Ok(OsString::from(rest))
                }
            };
            match flag {
                'v' => options = options.with_invert(true),
                'i' | 'y' => ignore_case = true,
                'w' => word = true,
                'a' => options = options.with_binary(Binary::Text),
                'I' => options = options.with_binary(Binary::Skip),
                'r' => {}
                'E' => syntax = Syntax::Extended,
                'G' => syntax = Syntax::Basic,
                'F' => syntax = Syntax::Fixed,
                'P' => bail!("Perl-compatible patterns (-P) are unsupported"),
                'n' => format = format.with_line_numbers(true),
                'h' => parsed.names = Some(false),
                'H' => parsed.names = Some(true),
                'z' => parsed.null = true,
                'o' => parsed.only_matching = true,
                'l' => parsed.listing = Listing::FilesWithMatches,
                'L' => parsed.listing = Listing::FilesWithoutMatch,
                'c' => parsed.listing = Listing::Count,
                'q' => parsed.listing = Listing::Quiet,
                'p' => options = options.with_function_name(true),
                'W' => options = options.with_function_body(true),
                'O' => bail!("-O (open files in a pager) is unsupported"),
                'A' | 'B' | 'C' | 'm' | 'e' | 'f' => {
                    let value = value()?;
                    match flag {
                        'A' => parsed.context.1 = Some(number(Some(value), "-A")?),
                        'B' => parsed.context.0 = Some(number(Some(value), "-B")?),
                        'C' => {
                            let lines = number(Some(value), "-C")?;
                            parsed.context = (Some(lines), Some(lines));
                        }
                        'm' => {
                            let count: isize = value
                                .to_str()
                                .and_then(|value| value.parse().ok())
                                .ok_or_else(|| eyre!("switch `m` expects a number"))?;
                            options = options.with_max_count(usize::try_from(count).ok());
                        }
                        'e' => {
                            parsed.patterns_given = true;
                            parsed
                                .atoms
                                .push(Token::Pattern(value.as_encoded_bytes().to_vec()));
                        }
                        _ => {
                            parsed.patterns_given = true;
                            read_pattern_file(&value, &mut parsed.atoms)?;
                        }
                    }
                    break;
                }
                _ => bail!("unknown switch `{flag}`"),
            }
        }
    }
    let (before, after) = parsed.context;
    parsed.options = options
        .with_before(before.unwrap_or(0))
        .with_after(after.unwrap_or(0));
    parsed.matching = Matching::new()
        .with_syntax(syntax)
        .with_ignore_case(ignore_case)
        .with_word(word);
    parsed.format = format.with_null(parsed.null);
    Ok(parsed)
}

/// Whether `LC_ALL`, `LC_CTYPE` or `LANG` (the first set) names a UTF-8 locale, in which Git's
/// regular expressions match characters; otherwise, as in the C locale, they match bytes.
fn utf8_locale(environment: &girt::workflow::Environment) -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| environment.get(name).filter(|value| !value.is_empty()))
        .is_some_and(|value| {
            let value = value.to_string_lossy().to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
}

/// Adds one pattern per nonempty line of `file` (`-` for standard input), as `-f` does.
fn read_pattern_file(file: &OsString, atoms: &mut Vec<Token>) -> Result<()> {
    let mut content = Vec::new();
    if file == "-" {
        std::io::stdin().read_to_end(&mut content)?;
    } else {
        content = std::fs::read(file)
            .wrap_err_with(|| format!("cannot open '{}'", Path::new(file).display()))?;
    }
    // Git skips empty lines.
    for line in content.split(|&byte| byte == b'\n') {
        if !line.is_empty() {
            atoms.push(Token::Pattern(line.to_vec()));
        }
    }
    Ok(())
}

type Tokens<I> = std::iter::Peekable<I>;

/// Parses Git's grammar: `--or` (or adjacency) binds loosest, then `--and`, then `--not`.
fn expression<I: Iterator<Item = Token>>(tokens: &mut Tokens<I>) -> Result<Option<Expression>> {
    if tokens.peek().is_none() {
        return Ok(None);
    }
    let expression = or(tokens)?;
    if tokens.next().is_some() {
        bail!("unmatched parenthesis");
    }
    Ok(Some(expression))
}

fn or<I: Iterator<Item = Token>>(tokens: &mut Tokens<I>) -> Result<Expression> {
    let left = and(tokens)?;
    match tokens.peek() {
        None | Some(Token::Close) => Ok(left),
        Some(Token::Or) => {
            tokens.next();
            Ok(left.or(or(tokens)?))
        }
        Some(_) => Ok(left.or(or(tokens)?)),
    }
}

fn and<I: Iterator<Item = Token>>(tokens: &mut Tokens<I>) -> Result<Expression> {
    let left = not(tokens)?;
    if matches!(tokens.peek(), Some(Token::And)) {
        tokens.next();
        return Ok(left.and(and(tokens)?));
    }
    Ok(left)
}

fn not<I: Iterator<Item = Token>>(tokens: &mut Tokens<I>) -> Result<Expression> {
    match tokens.next() {
        Some(Token::Not) => Ok(!not(tokens)?),
        Some(Token::Pattern(text)) => Ok(Expression::Pattern(text)),
        Some(Token::Open) => {
            let inner = or(tokens)?;
            match tokens.next() {
                Some(Token::Close) => Ok(inner),
                _ => bail!("unmatched parenthesis"),
            }
        }
        Some(Token::Close) => bail!("unmatched parenthesis"),
        Some(Token::And | Token::Or) | None => bail!("--and, --or or --not needs a pattern"),
    }
}

/// The current directory relative to the working tree root, with a trailing `/`, or empty.
fn prefix(repository: &Repository, context: &Context) -> Vec<u8> {
    if let Some(worktree) = repository.worktree()
        && let Ok(worktree) = worktree.canonicalize()
        && let Ok(current) = context.directory().canonicalize()
        && let Ok(prefix) = current.strip_prefix(&worktree)
        && !prefix.as_os_str().is_empty()
    {
        let mut prefix = prefix.as_os_str().as_encoded_bytes().to_vec();
        if std::path::MAIN_SEPARATOR != '/' {
            for byte in &mut prefix {
                if *byte == std::path::MAIN_SEPARATOR as u8 {
                    *byte = b'/';
                }
            }
        }
        prefix.push(b'/');
        return prefix;
    }
    Vec::new()
}

/// `path` as Git shows it from the directory `prefix`: relative, climbing with `../`.
fn relative(path: &[u8], prefix: &[u8]) -> Vec<u8> {
    let mut common = 0;
    for (index, (a, b)) in path.iter().zip(prefix).enumerate() {
        if a != b {
            break;
        }
        if *a == b'/' {
            common = index + 1;
        }
    }
    let ups = prefix
        .get(common..)
        .unwrap_or_default()
        .iter()
        .filter(|&&byte| byte == b'/')
        .count();
    let mut out = b"../".repeat(ups);
    out.extend_from_slice(path.get(common..).unwrap_or_default());
    out
}

fn search_repository(
    parsed: &Parsed,
    query: Query,
    format: GitFormat,
    context: &Context,
    out: &mut impl std::io::Write,
) -> Result<bool> {
    let repository = context.repository()?;
    let objects = repository.objects()?;
    let prefix = prefix(&repository, context);
    let mut revisions = Vec::new();
    let mut paths = Vec::new();
    for (index, arg) in parsed.positional.iter().enumerate() {
        let name = arg.to_str().ok_or_else(|| eyre!("argument is not UTF-8"))?;
        let id = common::resolve(&repository, name).ok().and_then(|id| {
            match objects
                .header(id, girt::ReadLimits::default())
                .ok()??
                .kind()
            {
                ObjectKind::Blob => Some((id, true)),
                _ => common::resolve_tree(&repository, &objects, name)
                    .ok()
                    .map(|tree| (tree, false)),
            }
        });
        match id {
            Some((id, blob)) => revisions.push((name.to_owned(), id, blob)),
            None => {
                paths.extend(parsed.positional.iter().skip(index).cloned());
                break;
            }
        }
    }
    paths.extend(parsed.paths.iter().cloned());
    if parsed.cached && !revisions.is_empty() {
        bail!("--cached cannot be used with revisions");
    }
    if parsed.untracked && (parsed.cached || !revisions.is_empty()) {
        bail!("--untracked cannot be used with --cached or revisions");
    }
    let mut pathspec_context = pathspec::Context::from_environment(&context.environment)?
        .with_prefix(prefix.strip_suffix(b"/").unwrap_or(&prefix));
    if paths.is_empty() && !prefix.is_empty() {
        paths.push(OsString::from("."));
    }
    pathspec_context = pathspec_context.with_excludes_from_root(false);
    let specs = paths.iter().map(|spec| spec.as_encoded_bytes());
    let pathspec = Pathspec::parse(specs, &pathspec_context)?;
    let scope = Scope::new(pathspec)
        .with_max_depth(parsed.max_depth)
        .with_environment(context.environment.clone());
    let attributes = repository.diff_attributes(&context.environment)?;
    let mut searcher = Searcher::new(query, parsed.options).with_attributes(attributes);
    let mut printer = Printer::new(parsed, format, quote_path_setting(&repository)?);
    let display = |path: &[u8]| {
        if parsed.full_name {
            path.to_vec()
        } else {
            relative(path, &prefix)
        }
    };
    if revisions.is_empty() {
        let source = if parsed.cached {
            Source::Index
        } else if parsed.untracked {
            Source::WorktreeAndUntracked {
                ignored: parsed.exclude_standard == Some(false),
            }
        } else {
            Source::Worktree
        };
        repository.grep(source, &scope, &mut searcher, |found| {
            printer.visit(out, b"", &display(found.path), &found)
        })?;
    } else {
        for (name, id, blob) in &revisions {
            let mut stop = false;
            repository.grep(Source::Object(*id), &scope, &mut searcher, |found| {
                let flow = if *blob {
                    printer.visit(out, name.as_bytes(), b"", &found)
                } else {
                    let revision = format!("{name}:");
                    printer.visit(out, revision.as_bytes(), &display(found.path), &found)
                };
                stop = flow.is_break();
                flow
            })?;
            if stop {
                break;
            }
        }
    }
    printer.finish()
}

fn search_directory(
    parsed: &Parsed,
    query: Query,
    format: GitFormat,
    context: &Context,
    out: &mut impl std::io::Write,
) -> Result<bool> {
    if parsed.cached || parsed.untracked || parsed.exclude_standard == Some(true) {
        bail!("--no-index cannot be used with --cached, --untracked or --exclude-standard");
    }
    let mut paths = parsed.positional.clone();
    paths.extend(parsed.paths.iter().cloned());
    let specs = paths.iter().map(|spec| spec.as_encoded_bytes());
    let pathspec = Pathspec::parse(specs, &pathspec::Context::new())?;
    let scope = Scope::new(pathspec).with_max_depth(parsed.max_depth);
    let mut searcher = Searcher::new(query, parsed.options);
    let quote = match context.user_config() {
        Ok(config) => config.boolean("core", None, "quotepath")?.unwrap_or(true),
        Err(_) => true,
    };
    let mut printer = Printer::new(parsed, format, quote);
    girt::grep::grep_directory(Path::new("."), &scope, &mut searcher, |found| {
        printer.visit(out, b"", found.path, &found)
    })?;
    printer.finish()
}

/// Prints each searched file and tracks the exit status: whether a file was listed, for `-L`,
/// or matched otherwise.
struct Printer {
    format: GitFormat,
    listing: Listing,
    null: bool,
    quote: bool,
    matched: bool,
    failure: Option<std::io::Error>,
}

impl Printer {
    fn new(parsed: &Parsed, format: GitFormat, quote: bool) -> Self {
        Self {
            format,
            listing: parsed.listing,
            null: parsed.null,
            quote,
            matched: false,
            failure: None,
        }
    }

    /// Prints one file as `<revision><path>`, quoting the path as Git does unless `-z` is given.
    fn visit(
        &mut self,
        out: &mut impl std::io::Write,
        revision: &[u8],
        path: &[u8],
        found: &girt::grep::Found<'_>,
    ) -> ControlFlow<()> {
        let mut name = revision.to_vec();
        if self.null {
            name.extend_from_slice(path);
        } else {
            name.extend_from_slice(&quote(path, self.quote));
        }
        let listed = match self.listing {
            Listing::FilesWithoutMatch => !found.matches.is_match(),
            _ => found.matches.is_match(),
        };
        self.matched |= listed;
        if let Err(error) =
            self.format
                .write(out, &name, found.content, found.matches, found.searcher)
        {
            self.failure = Some(error);
            return ControlFlow::Break(());
        }
        if self.listing == Listing::Quiet && self.matched {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    fn finish(self) -> Result<bool> {
        match self.failure {
            Some(error) => Err(error.into()),
            None => Ok(self.matched),
        }
    }
}

/// `core.quotePath` for this repository; Git's default is true.
fn quote_path_setting(repository: &Repository) -> Result<bool> {
    Ok(repository
        .config()
        .boolean("core", None, "quotepath")?
        .unwrap_or(true))
}

/// `path` as Git prints it: unchanged when it holds only printable ASCII other than `"` and
/// `\`, otherwise quoted with C escapes. With `core.quotePath` false, bytes above ASCII stay raw.
fn quote(path: &[u8], escape_non_ascii: bool) -> Vec<u8> {
    if escape_non_ascii {
        return girt::display::quote_path(path).to_string().into_bytes();
    }
    let special = |byte: u8| byte < 0x20 || byte == 0x7f || byte == b'"' || byte == b'\\';
    if !path.iter().any(|&byte| special(byte)) {
        return path.to_vec();
    }
    let mut out = vec![b'"'];
    for &byte in path {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\x07' => out.extend_from_slice(b"\\a"),
            b'\x08' => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\x0b' => out.extend_from_slice(b"\\v"),
            b'\x0c' => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            byte if special(byte) => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
            byte => out.push(byte),
        }
    }
    out.push(b'"');
    out
}
