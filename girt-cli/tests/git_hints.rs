//! Git command and flag names that girt spells differently or lacks produce a `tip:` on the error.

use std::process::Command;

/// Runs `girt` with color off and returns its exit code and stderr; parsing fails before any
/// repository is opened, so no fixture is needed.
fn girt(args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_girt"))
        .arg("--color=never")
        .args(args)
        .env_remove("CLICOLOR_FORCE")
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn renamed_command_points_at_girt_commands() {
    let (code, stderr) = girt(&["rev-parse", "HEAD"]);
    assert_eq!(code, Some(2));
    assert_eq!(
        stderr,
        "error: unrecognized subcommand 'rev-parse'

  tip: for 'git rev-parse --verify', use 'girt object id'
  tip: for 'git rev-parse <ref>', use 'girt ref show'

Usage: girt [OPTIONS] <COMMAND>

For more information, try '--help'.
"
    );
}

#[test]
fn missing_planned_command_links_open_issue() {
    let (_, stderr) = girt(&["commit", "-m", "message"]);
    assert_eq!(
        stderr,
        "error: unrecognized subcommand 'commit'

  tip: girt has no 'git commit' yet; see https://github.com/joshka/girt/issues/482

Usage: girt [OPTIONS] <COMMAND>

For more information, try '--help'.
"
    );
}

#[test]
fn out_of_scope_command_links_closed_issue() {
    let (_, stderr) = girt(&["rebase", "main"]);
    assert!(
        stderr.contains(
            "  tip: girt does not plan 'git rebase'; \
             https://github.com/joshka/girt/issues/488 explains why\n"
        ),
        "{stderr}"
    );
}

#[test]
fn renamed_flag_points_at_girt_flag() {
    let (_, stderr) = girt(&["log", "--max-count=3"]);
    assert_eq!(
        stderr,
        "error: unexpected argument '--max-count' found

  tip: for 'git log --max-count', use 'girt log --limit'

Usage: girt log [OPTIONS] [REVS]... [-- <PATHSPEC>...]

For more information, try '--help'.
"
    );
}

#[test]
fn missing_flag_links_issue() {
    let (_, stderr) = girt(&["log", "--graph"]);
    assert!(
        stderr.contains(
            "  tip: girt has no 'git log --graph' yet; see https://github.com/joshka/girt/issues/484\n"
        ),
        "{stderr}"
    );
}

/// Global options and their values before the command do not hide the subcommand path.
#[test]
fn nested_subcommand_after_global_options() {
    let (_, stderr) = girt(&["-C", ".", "remote", "rm", "origin"]);
    assert!(
        stderr.contains(
            "  tip: girt has no 'git remote rm' yet; see https://github.com/joshka/girt/issues/486\n"
        ),
        "{stderr}"
    );
}

#[test]
fn unknown_name_keeps_clap_suggestion() {
    let (_, stderr) = girt(&["stauts"]);
    assert!(
        stderr.contains("  tip: a similar subcommand exists: 'status'\n"),
        "{stderr}"
    );
}

#[test]
fn hint_follows_color_choice() {
    let output = Command::new(env!("CARGO_BIN_EXE_girt"))
        .args(["--color=always", "cat-file", "-p", "HEAD"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("use '\u{1b}[1m\u{1b}[36mgirt object show\u{1b}[0m'"),
        "{stderr:?}"
    );
}
