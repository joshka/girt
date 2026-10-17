//! One color policy for help, diagnostics and human command output.

use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;

#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

impl Color {
    /// Inspect global flags before clap can exit for help or a parse error.
    pub fn early(args: &[OsString]) -> Self {
        let mut color = Self::Auto;
        let mut git = false;
        let mut args = args.iter().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            if arg == "--git" {
                git = true;
            }
            let value = if arg == "--color" {
                args.next().and_then(|value| value.to_str())
            } else {
                arg.to_str().and_then(|arg| arg.strip_prefix("--color="))
            };
            if let Some(value) = value {
                color = match value {
                    "always" => Self::Always,
                    "never" => Self::Never,
                    _ => Self::Auto,
                };
            }
        }
        if git { Self::Never } else { color }
    }

    pub fn enabled(self, terminal: bool, env: impl Fn(&str) -> Option<OsString>) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                if env("NO_COLOR").is_some_and(|value| !value.is_empty()) {
                    false
                } else if env("CLICOLOR_FORCE")
                    .is_some_and(|value| !value.is_empty() && value != "0")
                {
                    true
                } else if env("CLICOLOR").as_deref() == Some(OsStr::new("0")) {
                    false
                } else {
                    terminal && env("TERM").as_deref() != Some(OsStr::new("dumb"))
                }
            }
        }
    }

    pub fn stdout(self) -> bool {
        self.enabled(std::io::stdout().is_terminal(), |name| {
            std::env::var_os(name)
        })
    }

    pub fn stderr(self) -> bool {
        self.enabled(std::io::stderr().is_terminal(), |name| {
            std::env::var_os(name)
        })
    }
}

/// Select style strings once for an output operation.
pub fn style(enabled: bool, ansi: &'static str) -> &'static str {
    if enabled { ansi } else { "" }
}

pub const RESET: &str = "\x1b[0m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence() {
        type Environment<'a> = &'a [(&'a str, &'a str)];
        let cases: &[(Environment<'_>, bool, bool)] = &[
            (&[], false, false),
            (&[], true, true),
            (&[("TERM", "dumb")], true, false),
            (&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")], true, false),
            (&[("NO_COLOR", "")], true, true),
            (
                &[("CLICOLOR_FORCE", "1"), ("CLICOLOR", "0"), ("TERM", "dumb")],
                false,
                true,
            ),
            (&[("CLICOLOR_FORCE", "0")], false, false),
            (&[("CLICOLOR_FORCE", "")], false, false),
            (&[("CLICOLOR", "0")], true, false),
            (&[("CLICOLOR", "1"), ("TERM", "dumb")], true, false),
        ];
        for &(values, terminal, expected) in cases {
            let env = |name: &str| {
                values
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            };
            assert_eq!(Color::Auto.enabled(terminal, env), expected, "{values:?}");
            assert!(Color::Always.enabled(terminal, env));
            assert!(!Color::Never.enabled(terminal, env));
        }
    }
}
