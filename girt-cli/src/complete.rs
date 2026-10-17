//! Shell completion and man page generation, plus the value completers that read the repository.
//!
//! Dynamic completion (`COMPLETE=zsh girt`) runs these completers on every Tab press, so they only
//! read refs, config and worktree registrations: never objects or the index. Any failure, such as
//! running outside a repository, yields no candidates rather than an error.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::CommandFactory as _;
use clap_complete::engine::CompletionCandidate;
use color_eyre::eyre::{Result, WrapErr as _, eyre};
use girt::Repository;
use girt::remote::Remote;

use crate::Context;
use crate::common::not_cancelled;

/// Upper bound on worktree registrations read while completing.
const MAX_WORKTREES: usize = 10_000;

/// Arguments for `girt completions`.
#[derive(Debug, clap::Args)]
pub struct CompletionsArgs {
    /// Shell to print the completion script for.
    shell: clap_complete::Shell,
    /// Print a self-contained script that completes commands and options only, for packagers.
    #[arg(long = "static")]
    static_script: bool,
}

/// Arguments for `girt man`.
#[derive(Debug, clap::Args)]
pub struct ManArgs {
    /// Write `girt.1` and one page per subcommand (`girt-object-show.1`, ...) into this directory
    /// instead of printing `girt.1` to stdout.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
}

/// Prints the completion script for a shell.
///
/// By default this is the script `COMPLETE=<shell> girt` prints: a small hook that calls back into
/// `girt` on each Tab, so values come from the current repository. `--static` prints a generated
/// script that needs no callback but completes commands and options only.
pub fn completions(args: CompletionsArgs) -> Result<ExitCode> {
    let mut command = crate::Cli::command();
    let mut out = io::stdout().lock();
    if args.static_script {
        clap_complete::generate(args.shell, &mut command, "girt", &mut out);
    } else {
        let shells = clap_complete::env::Shells::builtins();
        let shell = shells
            .completer(&args.shell.to_string())
            .ok_or_else(|| eyre!("no dynamic completion for {}", args.shell))?;
        shell.write_registration("COMPLETE", "girt", "girt", &completer(), &mut out)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The program the shell hook runs, resolved the way `CompleteEnv` resolves it: as invoked, with a
/// relative path made absolute.
fn completer() -> String {
    let invoked = std::env::args_os()
        .next()
        .map_or_else(|| PathBuf::from("girt"), PathBuf::from);
    let path = if invoked.components().count() > 1 && invoked.is_relative() {
        std::env::current_dir().map_or(invoked.clone(), |dir| dir.join(&invoked))
    } else {
        invoked
    };
    path.to_string_lossy().into_owned()
}

/// Prints `girt.1`, or writes every page into `--out`.
pub fn man(args: ManArgs) -> Result<ExitCode> {
    let command = crate::Cli::command();
    match args.out {
        None => {
            let mut out = io::stdout().lock();
            clap_mangen::Man::new(command).render(&mut out)?;
            out.flush()?;
        }
        Some(dir) => {
            fs::create_dir_all(&dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
            for (name, page) in man_pages(command)? {
                let path = dir.join(format!("{name}.1"));
                fs::write(&path, page).wrap_err_with(|| format!("writing {}", path.display()))?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Renders the page for `command` and every subcommand as `(page name, roff)`, parents first.
pub fn man_pages(command: clap::Command) -> Result<Vec<(String, Vec<u8>)>> {
    let mut command = command;
    command.build();
    let mut pages = Vec::new();
    render_pages(&command, command.get_name(), &mut pages)?;
    Ok(pages)
}

fn render_pages(
    command: &clap::Command,
    name: &str,
    pages: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    let mut page = Vec::new();
    let named = command
        .clone()
        .name(name.to_owned())
        .bin_name(name.replace('-', " "));
    clap_mangen::Man::new(named).render(&mut page)?;
    pages.push((name.to_owned(), page));
    for sub in command
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
    {
        render_pages(sub, &format!("{name}-{}", sub.get_name()), pages)?;
    }
    Ok(())
}

fn repository() -> Option<Repository> {
    Context::new(false).repository().ok()
}

/// Revisions: `HEAD` plus every ref by its short name. Inside a range (`A..B`, `A...B`) or after
/// `^`, the part after the operator is completed and the rest kept.
pub fn revisions(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let split = current
        .rfind("..")
        .map(|at| at + 2 + usize::from(current[at + 2..].starts_with('.')))
        .unwrap_or_else(|| usize::from(current.starts_with('^')));
    let (kept, word) = current.split_at(split);
    let mut names = vec![("HEAD".to_owned(), None)];
    if let Some(list) = repository()
        .as_ref()
        .and_then(|repository| repository.references().ok()?.list().ok())
    {
        let mut short: Vec<(String, &str)> = list
            .iter()
            .filter_map(|reference| {
                let name = std::str::from_utf8(reference.name.as_bytes()).ok()?;
                Some(short_name(name))
            })
            .collect();
        short.sort();
        short.dedup();
        names.extend(short.into_iter().map(|(name, kind)| (name, Some(kind))));
    }
    names
        .into_iter()
        .filter(|(name, _)| name.starts_with(word))
        .map(|(name, kind)| {
            CompletionCandidate::new(format!("{kept}{name}")).help(kind.map(Into::into))
        })
        .collect()
}

/// Full reference names, for commands that take a ref rather than a revision.
pub fn references() -> Vec<CompletionCandidate> {
    let mut candidates = vec![CompletionCandidate::new("HEAD")];
    let Some(list) = repository()
        .as_ref()
        .and_then(|repository| repository.references().ok()?.list().ok())
    else {
        return candidates;
    };
    let mut names: Vec<String> = list
        .iter()
        .filter_map(|reference| String::from_utf8(reference.name.as_bytes().to_vec()).ok())
        .collect();
    names.sort();
    candidates.extend(names.into_iter().map(CompletionCandidate::new));
    candidates
}

/// Splits a full ref name into the short name Git would accept and a description of its kind.
fn short_name(name: &str) -> (String, &'static str) {
    for (prefix, kind) in [
        ("refs/heads/", "branch"),
        ("refs/tags/", "tag"),
        ("refs/remotes/", "remote branch"),
    ] {
        if let Some(short) = name.strip_prefix(prefix) {
            return (short.to_owned(), kind);
        }
    }
    (name.to_owned(), "ref")
}

/// Remote names from every config layer, as the remote commands see them.
pub fn remotes() -> Vec<CompletionCandidate> {
    let Ok(repository) = Context::new(false).repository() else {
        return Vec::new();
    };
    let names: BTreeSet<String> = Remote::names(repository.config())
        .into_iter()
        .filter_map(|name| String::from_utf8(name.to_vec()).ok())
        .collect();
    names.into_iter().map(CompletionCandidate::new).collect()
}

/// Linked worktree registration names (the directory names under `worktrees/`).
pub fn worktrees() -> Vec<CompletionCandidate> {
    let Some(repository) = repository() else {
        return Vec::new();
    };
    let Ok(worktrees) = repository.worktrees(MAX_WORKTREES, not_cancelled()) else {
        return Vec::new();
    };
    worktrees
        .into_iter()
        .filter_map(|worktree| {
            let name = worktree.git_dir.file_name()?.to_str()?.to_owned();
            let help = worktree
                .path
                .as_deref()
                .map(Path::display)
                .map(|p| p.to_string());
            Some(CompletionCandidate::new(name).help(help.map(Into::into)))
        })
        .collect()
}

/// Keys set in the repository's local, global and system config, as `git config --list` names them.
pub fn config_keys() -> Vec<CompletionCandidate> {
    let Ok(repository) = Context::new(false).repository() else {
        return Vec::new();
    };
    let keys: BTreeSet<String> = repository
        .config()
        .entries()
        .iter()
        .filter_map(|entry| {
            let mut key = String::from_utf8(entry.section.to_ascii_lowercase()).ok()?;
            if let Some(subsection) = &entry.subsection {
                key.push('.');
                key.push_str(std::str::from_utf8(subsection).ok()?);
            }
            key.push('.');
            key.push_str(&String::from_utf8(entry.name.to_ascii_lowercase()).ok()?);
            Some(key)
        })
        .collect();
    keys.into_iter().map(CompletionCandidate::new).collect()
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;
    use clap_complete::Shell;

    use super::*;

    #[test]
    fn every_subcommand_renders_a_man_page() {
        let pages = man_pages(crate::Cli::command()).unwrap();
        let names: Vec<&str> = pages.iter().map(|(name, _)| name.as_str()).collect();
        for expected in [
            "girt",
            "girt-object",
            "girt-object-show",
            "girt-ref-set",
            "girt-man",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        for (name, page) in &pages {
            let page = String::from_utf8(page.clone()).unwrap();
            assert!(page.starts_with(".ie"), "{name} is not roff");
            assert!(page.contains(".SH NAME"), "{name} has no NAME section");
        }
    }

    #[test]
    fn every_shell_generates_a_static_script() {
        for shell in Shell::value_variants() {
            let mut script = Vec::new();
            clap_complete::generate(*shell, &mut crate::Cli::command(), "girt", &mut script);
            let script = String::from_utf8(script).unwrap();
            assert!(
                script.contains("worktree"),
                "{shell} script lacks subcommands"
            );
        }
    }

    #[test]
    fn every_shell_has_a_dynamic_hook() {
        for shell in Shell::value_variants() {
            let shells = clap_complete::env::Shells::builtins();
            let completer = shells
                .completer(&shell.to_string())
                .unwrap_or_else(|| panic!("no dynamic completer for {shell}"));
            let mut script = Vec::new();
            completer
                .write_registration("COMPLETE", "girt", "girt", "girt", &mut script)
                .unwrap();
            let script = String::from_utf8(script).unwrap();
            assert!(
                script.contains("COMPLETE"),
                "{shell} hook does not call back"
            );
        }
    }

    #[test]
    fn short_names_follow_git_expansion() {
        assert_eq!(short_name("refs/heads/main"), ("main".into(), "branch"));
        assert_eq!(short_name("refs/tags/v1"), ("v1".into(), "tag"));
        assert_eq!(
            short_name("refs/remotes/origin/main"),
            ("origin/main".into(), "remote branch")
        );
        assert_eq!(
            short_name("refs/notes/commits"),
            ("refs/notes/commits".into(), "ref")
        );
    }
}
