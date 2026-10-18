//! Hints for Git users: when clap rejects a Git command or flag, say what girt offers instead.
//!
//! The table maps Git spellings to girt invocations, or to the issue that tracks a missing command.
//! It never runs anything: girt's semantics often differ from Git's (`girt checkout` does not move
//! HEAD), so a Git name only ever produces a `tip:` line on clap's error.

use std::ffi::OsString;
use std::fmt::Write as _;

use clap::builder::StyledStr;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Command, CommandFactory};

const ISSUES: &str = "https://github.com/joshka/girt/issues";

/// What girt offers for one Git command, subcommand or flag.
enum Hint {
    /// girt has a command with the same name, so clap accepts it and no hint is needed.
    Same,
    /// girt spells it differently: `(Git form, girt invocation)` pairs, both without the program.
    Use(&'static [(&'static str, &'static str)]),
    /// girt does not have it yet; the open issue tracks the work.
    Planned(u32),
    /// girt deliberately does not have it; the closed issue records why.
    OutOfScope(u32),
}

/// Objects, staging and commits.
const STAGING: Hint = Hint::Planned(482);
/// Branches, tags and other reference porcelain.
const REFS: Hint = Hint::Planned(483);
/// History, diff, blame and attribute queries.
const QUERIES: Hint = Hint::Planned(484);
/// Object store integrity, auxiliary indexes and packs.
const STORE: Hint = Hint::Planned(485);
/// Transfer, remotes and credentials.
const TRANSFER: Hint = Hint::Planned(486);
/// Worktrees, submodules, hooks and configuration scopes.
const SETUP: Hint = Hint::Planned(487);
/// Interactive, branch-switching, history-rewriting and merge porcelain that jj owns.
const PORCELAIN: Hint = Hint::OutOfScope(488);
/// Email patches, other version control systems, servers and Git's own tooling.
const FOREIGN: Hint = Hint::OutOfScope(489);

/// Git words under one girt command path (`""` for top-level commands) that share a hint.
struct Entry {
    path: &'static str,
    git: &'static [&'static str],
    hint: Hint,
}

const fn commands(git: &'static [&'static str], hint: Hint) -> Entry {
    Entry {
        path: "",
        git,
        hint,
    }
}

const fn under(path: &'static str, git: &'static [&'static str], hint: Hint) -> Entry {
    Entry { path, git, hint }
}

const fn use_(pairs: &'static [(&'static str, &'static str)]) -> Hint {
    Hint::Use(pairs)
}

#[rustfmt::skip]
const ENTRIES: &[Entry] = &[
    // Top-level Git commands: every name in `git_commands.txt` appears exactly once.
    commands(&[
        "blame", "checkout", "clone", "config", "diff", "fetch", "help", "history", "init", "log",
        "maintenance", "multi-pack-index", "push", "remote", "repo", "reset", "status", "worktree",
    ], Hint::Same),
    commands(&["annotate"], use_(&[("annotate", "blame")])),
    commands(&["cat-file"], use_(&[
        ("cat-file -p", "object show"),
        ("cat-file -t / -s", "object info"),
        ("cat-file -e", "object exists"),
        ("cat-file --batch", "object batch"),
        ("cat-file --batch-all-objects", "object list"),
    ])),
    commands(&["check-ignore"], use_(&[("check-ignore -v", "ignore check")])),
    commands(&["count-objects"], use_(&[("count-objects -v", "object count")])),
    commands(&["diff-files", "diff-index"], use_(&[("diff-index / diff-files", "status")])),
    commands(&["describe"], use_(&[("describe", "history describe")])),
    commands(&["diff-tree"], use_(&[("diff-tree", "diff")])),
    commands(&["fetch-pack"], use_(&[("fetch-pack", "fetch")])),
    commands(&["for-each-ref"], use_(&[("for-each-ref", "ref list")])),
    commands(&["gc"], use_(&[
        ("gc", "maintenance repack"),
        ("gc --prune", "maintenance run --prune-loose --yes"),
    ])),
    commands(&["hash-object"], use_(&[("hash-object", "object hash")])),
    commands(&["index-pack"], use_(&[("index-pack", "pack reindex")])),
    commands(&["ls-files"], use_(&[("ls-files", "index list")])),
    commands(&["ls-remote"], use_(&[("ls-remote", "remote heads")])),
    commands(&["ls-tree"], use_(&[("ls-tree", "tree list")])),
    commands(&["merge-base"], use_(&[
        ("merge-base", "history merge-base"),
        ("merge-base --is-ancestor", "history contains"),
    ])),
    commands(&["prune"], use_(&[
        ("prune -n", "maintenance run --prune-loose"),
        ("prune", "maintenance run --prune-loose --yes"),
    ])),
    commands(&["reflog"], use_(&[("reflog show", "ref log")])),
    commands(&["refs"], use_(&[("refs list", "ref list")])),
    commands(&["repack"], use_(&[("repack -a", "maintenance repack")])),
    commands(&["rev-list"], use_(&[("rev-list --count", "history count"), ("rev-list", "log")])),
    commands(&["rev-parse"], use_(&[("rev-parse --verify", "object id"), ("rev-parse <ref>", "ref show")])),
    commands(&["send-pack"], use_(&[("send-pack", "push")])),
    commands(&["show"], use_(&[("show <object>", "object show"), ("show <commit>", "diff --patch <commit>^ <commit>")])),
    commands(&["show-ref"], use_(&[("show-ref", "ref list")])),
    commands(&["update-ref"], use_(&[("update-ref", "ref set"), ("update-ref -d", "ref delete")])),
    commands(&["verify-pack"], use_(&[("verify-pack", "pack verify")])),
    commands(&["version"], use_(&[("version", "--version")])),
    commands(&[
        "add", "checkout-index", "clean", "commit", "commit-tree", "interpret-trailers", "mktag",
        "mktree", "mv", "read-tree", "restore", "rm", "stripspace", "write-tree",
    ], STAGING),
    commands(&["update-index"], use_(&[("update-index --refresh", "index refresh")])),
    commands(&[
        "branch", "check-ref-format", "name-rev", "notes", "pack-refs", "replace",
        "symbolic-ref", "tag",
    ], REFS),
    commands(&[
        "check-attr", "check-mailmap", "cherry", "diff-pairs", "format-rev", "grep",
        "last-modified", "patch-id", "range-diff", "shortlog", "show-branch", "verify-commit",
        "verify-tag", "whatchanged",
    ], QUERIES),
    commands(&[
        "commit-graph", "fsck", "pack-objects", "pack-redundant",
        "prune-packed", "show-index", "unpack-file", "unpack-objects",
    ], STORE),
    commands(&[
        "archive", "backfill", "bundle", "credential", "credential-cache", "credential-store",
        "get-tar-commit-id", "url-parse",
    ], TRANSFER),
    commands(&["for-each-repo", "hook", "sparse-checkout", "submodule", "var"], SETUP),
    commands(&[
        "bisect", "cherry-pick", "citool", "difftool", "filter-branch", "fmt-merge-msg", "gitk",
        "gui", "merge", "merge-file", "merge-index", "merge-one-file", "merge-tree", "mergetool",
        "pull", "rebase", "replay", "rerere", "revert", "stash", "switch",
    ], PORCELAIN),
    commands(&[
        "am", "apply", "archimport", "bugreport", "column", "cvsexportcommit", "cvsimport",
        "cvsserver", "daemon", "diagnose", "fast-export", "fast-import", "format-patch", "gitweb",
        "http-backend", "imap-send", "instaweb", "mailinfo", "mailsplit", "p4", "quiltimport",
        "request-pull", "scalar", "send-email", "sh-i18n", "sh-setup", "svn", "update-server-info",
    ], FOREIGN),

    // Flags and subcommands of girt commands that share a name with a Git command.
    under("blame", &[
        "-L", "-w", "-M", "-s", "--porcelain", "--line-porcelain", "--reverse", "--date",
    ], QUERIES),
    under("checkout", &["-b", "-B", "--orphan", "--detach", "-p", "--patch"], PORCELAIN),
    under("clone", &[
        "--depth", "--shallow-since", "-b", "--branch", "--single-branch", "--mirror", "--filter",
        "-o", "--origin", "--reference", "--sparse", "--separate-git-dir",
    ], TRANSFER),
    under("clone", &["--recurse-submodules", "--recursive"], SETUP),
    under("config", &["--get"], use_(&[("config --get", "config get")])),
    under("config", &["-l", "--list"], use_(&[("config --list", "config list")])),
    under("config", &["--unset", "--unset-all"], use_(&[("config --unset", "config unset")])),
    under("config", &[
        "--global", "--system", "--local", "-f", "--file", "--get-all", "--get-regexp", "--add",
        "--replace-all", "--show-origin", "--show-scope", "--type", "-e", "--edit",
        "rename-section", "remove-section", "edit",
    ], SETUP),
    under("diff", &["--cached", "--staged"], use_(&[("diff --cached", "status")])),
    under("diff", &["-p", "-u"], use_(&[("diff -p", "diff --patch")])),
    under("diff", &["-M", "--find-renames"], use_(&[("diff -M", "diff --renames")])),
    under("diff", &["--find-copies"], use_(&[("diff --find-copies", "diff --copies")])),
    under("diff", &["--name-status"], use_(&[("diff --name-status", "diff --names")])),
    under("diff", &[
        "--name-only", "--stat", "--numstat", "--shortstat", "--word-diff", "-U", "--unified", "--no-index", "-w",
        "--ignore-all-space", "--exit-code", "--raw",
    ], QUERIES),
    under("fetch", &[
        "--all", "-p", "--prune", "--prune-tags", "-t", "--tags", "--depth", "--unshallow",
        "--dry-run", "-f", "--force", "--multiple",
    ], TRANSFER),
    under("init", &["--object-format"], use_(&[("init --object-format=sha256", "init --sha256")])),
    under("init", &["--ref-format"], use_(&[("init --ref-format=reftable", "init --reftable")])),
    under("init", &["--template", "--shared", "--separate-git-dir"], SETUP),
    under("log", &["--max-count"], use_(&[("log --max-count", "log --limit")])),
    under("log", &[
        "--graph", "-p", "--patch", "--stat", "--format", "--pretty", "--author", "--grep",
        "--since", "--after", "--until", "--before", "--all", "--reverse", "--follow",
        "--first-parent", "--name-only", "--name-status", "--decorate", "--merges", "--no-merges",
        "--date",
    ], QUERIES),
    under("maintenance", &["start", "stop", "register", "unregister"], STORE),
    under("push", &["-f", "--force"], use_(&[("push --force", "push +<refspec>")])),
    under("push", &["--force-with-lease"], use_(&[("push --force-with-lease", "push")])),
    under("push", &[
        "--all", "--tags", "--follow-tags", "--mirror", "-d", "--delete", "-u", "--set-upstream",
        "-n", "--dry-run", "--atomic", "-o", "--push-option", "--prune", "--no-verify",
    ], TRANSFER),
    under("remote", &[
        "remove", "rm", "rename", "set-url", "get-url", "set-head", "set-branches", "prune",
        "update",
    ], TRANSFER),
    under("status", &["-s", "--short"], use_(&[("status --short", "status")])),
    under("status", &["--porcelain"], use_(&[("status --porcelain", "status --git")])),
    under("status", &["--untracked-files"], use_(&[("status --untracked-files", "status --untracked")])),
    under("status", &["--ignored", "-z"], QUERIES),
    under("worktree", &["add", "remove", "move"], SETUP),
];

/// Replaces clap's suggestions with a girt hint when the rejected command or flag is a Git name.
///
/// Leaves the error untouched when the table has no entry, so clap's own "did you mean" remains.
pub fn annotate(error: &mut clap::Error, args: &[OsString]) {
    let context = match error.kind() {
        ErrorKind::InvalidSubcommand => ContextKind::InvalidSubcommand,
        ErrorKind::UnknownArgument => ContextKind::InvalidArg,
        _ => return,
    };
    let Some(ContextValue::String(word)) = error.get(context) else {
        return;
    };
    // Clap reports `--max-count=3` as `--max-count`; strip a value anyway in case it ever doesn't.
    let word = word.split('=').next().unwrap_or_default().to_owned();
    let mut root = crate::Cli::command();
    root.build();
    let path = command_path(&root, args);
    let Some((path, hint)) = lookup(&path, &word) else {
        return;
    };
    let tips = tips(path, &word, hint);
    if tips.is_empty() {
        return;
    }
    error.remove(ContextKind::SuggestedSubcommand);
    error.remove(ContextKind::SuggestedArg);
    error.insert(ContextKind::Suggested, ContextValue::StyledStrs(tips));
}

/// Finds the hint for `word` rejected under the girt subcommand `path`.
///
/// A flag also matches an entry for an enclosing command, so `config set --global` finds the
/// `config` entry; a subcommand name only matches at its own level.
fn lookup(path: &[String], word: &str) -> Option<(&'static str, &'static Hint)> {
    let levels = if word.starts_with('-') {
        path.len() + 1
    } else {
        1
    };
    (0..levels).find_map(|drop| {
        let prefix = path[..path.len() - drop].join(" ");
        ENTRIES
            .iter()
            .find(|entry| entry.path == prefix && entry.git.contains(&word))
            .map(|entry| (entry.path, &entry.hint))
    })
}

fn tips(path: &str, word: &str, hint: &Hint) -> Vec<StyledStr> {
    let valid = crate::STYLES.get_valid();
    let git = if path.is_empty() {
        format!("git {word}")
    } else {
        format!("git {path} {word}")
    };
    let mut tip = StyledStr::new();
    match hint {
        Hint::Same => return Vec::new(),
        Hint::Use(pairs) => {
            return pairs
                .iter()
                .map(|(git, girt)| {
                    let mut tip = StyledStr::new();
                    let _ = write!(tip, "for 'git {git}', use '{valid}girt {girt}{valid:#}'");
                    tip
                })
                .collect();
        }
        Hint::Planned(issue) => {
            let _ = write!(tip, "girt has no '{git}' yet; see {ISSUES}/{issue}");
        }
        Hint::OutOfScope(issue) => {
            let _ = write!(
                tip,
                "girt does not plan '{git}'; {ISSUES}/{issue} explains why"
            );
        }
    }
    vec![tip]
}

/// Returns the names of the girt subcommands `args` selects before the first unknown word.
///
/// Skips option values (`-C <PATH>`, `--color <WHEN>`) so they are not mistaken for commands.
fn command_path(root: &Command, args: &[OsString]) -> Vec<String> {
    let mut path = Vec::new();
    let mut command = root;
    let mut args = args.iter().skip(1).map(|arg| arg.to_string_lossy());
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let long = long.split('=').next().unwrap_or_default();
            // An option this command does not know is the one clap rejected: stop here.
            let Some(option) = command
                .get_arguments()
                .find(|option| option.get_long() == Some(long))
            else {
                break;
            };
            if option.get_action().takes_values() && !arg.contains('=') {
                args.next();
            }
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|shorts| !shorts.is_empty()) {
            // In a cluster such as `-qC dir`, the first value-taking short consumes the rest of the
            // cluster, or the next argument when it ends the cluster.
            let mut known = true;
            for (index, short) in shorts.char_indices() {
                let Some(option) = command
                    .get_arguments()
                    .find(|option| option.get_short() == Some(short))
                else {
                    known = false;
                    break;
                };
                if option.get_action().takes_values() {
                    if index + short.len_utf8() == shorts.len() {
                        args.next();
                    }
                    break;
                }
            }
            if !known {
                break;
            }
        } else if let Some(subcommand) = command.find_subcommand(arg.as_ref()) {
            path.push(subcommand.get_name().to_owned());
            command = subcommand;
        } else if command.has_subcommands() {
            break;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIT_COMMANDS: &str = include_str!("git_commands.txt");

    fn git_commands() -> Vec<&'static str> {
        GIT_COMMANDS
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect()
    }

    fn built() -> Command {
        let mut root = crate::Cli::command();
        root.build();
        root
    }

    /// Resolves a girt path (`"remote heads"`) to its command, if every name exists.
    fn find<'a>(root: &'a Command, path: &str) -> Option<&'a Command> {
        path.split_whitespace()
            .try_fold(root, |command, name| command.find_subcommand(name))
    }

    /// Whether `command` already accepts `word` as a subcommand or flag.
    fn accepts(command: &Command, word: &str) -> bool {
        if let Some(long) = word.strip_prefix("--") {
            command
                .get_arguments()
                .any(|arg| arg.get_long() == Some(long))
        } else if let Some(short) = word.strip_prefix('-') {
            let short = short.chars().next();
            command.get_arguments().any(|arg| arg.get_short() == short)
        } else {
            command.find_subcommand(word).is_some()
        }
    }

    /// Checks a girt invocation such as `log --limit` or `push +<refspec>`: each name must be a
    /// subcommand and each flag an option of the command so far; placeholders are skipped.
    fn invocation_exists(root: &Command, invocation: &str) -> bool {
        let mut command = root;
        for word in invocation.split_whitespace() {
            if word.contains('<') {
                continue;
            }
            if word.starts_with('-') {
                if !accepts(command, word) {
                    return false;
                }
            } else {
                match command.find_subcommand(word) {
                    Some(subcommand) => command = subcommand,
                    None => return false,
                }
            }
        }
        true
    }

    #[test]
    fn every_git_command_has_one_entry() {
        let listed = git_commands();
        let mut entries: Vec<_> = ENTRIES
            .iter()
            .filter(|entry| entry.path.is_empty())
            .flat_map(|entry| entry.git.iter().copied())
            .collect();
        entries.sort_unstable();
        let mut expected = listed.clone();
        expected.sort_unstable();
        assert_eq!(entries, expected);
    }

    #[test]
    fn same_name_entries_are_girt_commands() {
        let root = built();
        let missing: Vec<_> = ENTRIES
            .iter()
            .filter(|entry| matches!(entry.hint, Hint::Same))
            .flat_map(|entry| entry.git.iter())
            .filter(|name| root.find_subcommand(name).is_none())
            .collect();
        assert!(missing.is_empty(), "not girt commands: {missing:?}");
    }

    #[test]
    fn use_targets_exist() {
        let root = built();
        let missing: Vec<_> = ENTRIES
            .iter()
            .filter_map(|entry| match entry.hint {
                Hint::Use(pairs) => Some(pairs),
                _ => None,
            })
            .flatten()
            .filter(|(_, girt)| !invocation_exists(&root, girt))
            .collect();
        assert!(
            missing.is_empty(),
            "girt invocations that do not parse: {missing:?}"
        );
    }

    /// An entry girt already accepts would never be shown, and one under a missing path never
    /// matches.
    #[test]
    fn entries_are_reachable() {
        let root = built();
        let unreachable: Vec<_> = ENTRIES
            .iter()
            .filter(|entry| !matches!(entry.hint, Hint::Same))
            .flat_map(|entry| entry.git.iter().map(move |word| (entry.path, *word)))
            .filter(|(path, word)| find(&root, path).is_none_or(|command| accepts(command, word)))
            .collect();
        assert!(
            unreachable.is_empty(),
            "unreachable entries: {unreachable:?}"
        );
    }

    #[test]
    fn words_are_unique_per_path() {
        let mut keys: Vec<_> = ENTRIES
            .iter()
            .flat_map(|entry| entry.git.iter().map(move |word| (entry.path, *word)))
            .collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }

    fn path(args: &[&str]) -> Vec<String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        command_path(&built(), &args)
    }

    #[test]
    fn command_path_skips_option_values() {
        assert_eq!(path(&["girt", "-C", "log", "log", "--stat"]), ["log"]);
        assert_eq!(
            path(&["girt", "--color", "never", "remote", "rm"]),
            ["remote"]
        );
        assert_eq!(
            path(&["girt", "-qC", "dir", "config", "set", "--global"]),
            ["config", "set"]
        );
        assert_eq!(path(&["girt", "rev-parse", "HEAD"]), Vec::<String>::new());
        assert_eq!(path(&["girt", "--color=never", "log", "--stat"]), ["log"]);
    }

    /// The walk stops at the option clap rejected, so a later subcommand does not claim it.
    #[test]
    fn command_path_stops_at_unknown_option() {
        assert_eq!(
            path(&["girt", "--get", "config", "list"]),
            Vec::<String>::new()
        );
        assert_eq!(path(&["girt", "log", "-x", "status"]), ["log"]);
    }

    #[test]
    fn flags_fall_back_to_enclosing_commands() {
        let path = ["config".to_owned(), "set".to_owned()];
        assert_eq!(
            lookup(&path, "--global").map(|(path, _)| path),
            Some("config")
        );
        let path = ["remote".to_owned(), "list".to_owned()];
        assert!(lookup(&path, "rm").is_none());
    }
}
