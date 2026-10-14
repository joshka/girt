//! Protocol v2 capability advertisement and command request framing.
//!
//! A v2 server opens with `version 2` and a flush-terminated list of `key[=value]` capabilities.
//! Commands such as `ls-refs` and `fetch` are advertised as keys; their optional features are a
//! space-separated value. Each request names one command, repeats the capabilities the client uses,
//! then sends a delimiter packet before command arguments.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted network input (#103)"
)]

use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

use super::{FetchError, FetchLimits};
use crate::ObjectFormat;
use crate::packet::{Frame, Wire, packet, put};

/// A validated v2 capability advertisement, retained in server order.
#[derive(Debug, Clone)]
pub(super) struct Capabilities {
    lines: Vec<Vec<u8>>,
    format: ObjectFormat,
}

impl Capabilities {
    /// Reads capability lines after the `version 2` packet, up to and including the flush.
    ///
    /// Every key must use the documented key alphabet and values must be printable ASCII.
    /// Unknown capabilities are retained but never requested. Repeated `fetch`, `ls-refs` or
    /// `object-format` keys are rejected because their meaning would be ambiguous.
    pub(super) fn read(
        wire: &mut Wire<'_, impl Read>,
        limits: FetchLimits,
    ) -> Result<Self, FetchError> {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        loop {
            let line = match wire.frame()? {
                Frame::Flush => break,
                Frame::Data(line) => line,
                Frame::Delim | Frame::ResponseEnd => {
                    return Err(FetchError::Protocol("v2 capability advertisement"));
                }
            };
            let line = line
                .strip_suffix(b"\n")
                .ok_or(FetchError::Protocol("v2 capability line"))?;
            let (key, value) = split(line);
            if !valid_key(key) || value.is_some_and(|value| !valid_value(value)) {
                return Err(FetchError::Protocol("v2 capability token"));
            }
            // These keys are meaningless without a value; `agent` also excludes spaces.
            match (key, value) {
                (b"object-format", None) => {
                    return Err(FetchError::Protocol("v2 object-format value"));
                }
                (b"agent", None) => return Err(FetchError::Protocol("v2 agent value")),
                (b"agent", Some(value)) if value.contains(&b' ') => {
                    return Err(FetchError::Protocol("v2 agent value"));
                }
                _ => {}
            }
            if matches!(key, b"fetch" | b"ls-refs" | b"object-format")
                && lines.iter().any(|existing| split(existing).0 == key)
            {
                return Err(FetchError::Protocol("duplicate v2 capability"));
            }
            if lines.len() == limits.max_refs {
                return Err(FetchError::Limit("v2 capabilities"));
            }
            lines.push(line.to_vec());
        }
        let format = match lines
            .iter()
            .find_map(|line| line.strip_prefix(b"object-format="))
        {
            None => ObjectFormat::Sha1,
            Some(b"sha1") => ObjectFormat::Sha1,
            Some(b"sha256") => ObjectFormat::Sha256,
            Some(_) => return Err(FetchError::Unsupported("object format")),
        };
        Ok(Self { lines, format })
    }

    /// Object format named by `object-format`, or SHA-1 when the capability is absent.
    pub(super) fn object_format(&self) -> ObjectFormat {
        self.format
    }

    /// Reports whether `key` is advertised, with or without a value.
    pub(super) fn has(&self, key: &[u8]) -> bool {
        self.lines.iter().any(|line| split(line).0 == key)
    }

    /// Reports whether command `command` advertises `feature` in its space-separated value.
    pub(super) fn has_feature(&self, command: &[u8], feature: &[u8]) -> bool {
        self.lines.iter().any(|line| match split(line) {
            (key, Some(value)) if key == command => value
                .split(|byte| *byte == b' ')
                .any(|item| item == feature),
            _ => false,
        })
    }

    /// Capability lines without their trailing LF, in server order, for
    /// [`super::Advertisement::capabilities`].
    ///
    /// V2 defines no `symref` capability, so any advertised `symref` line is omitted: in the
    /// shared advertisement representation it would read as a v0 HEAD hint. HEAD comes only from
    /// the `ls-refs` response. Every other line, known or unknown, is retained.
    pub(super) fn advertised_lines(&self) -> Vec<Vec<u8>> {
        self.lines
            .iter()
            .filter(|line| split(line).0 != b"symref")
            .cloned()
            .collect()
    }

    /// Writes `command=<name>`, the shared request capabilities and the argument delimiter.
    ///
    /// `agent` is sent only when the server advertises it. `object-format` is sent when the server
    /// advertises it, so SHA-256 sessions always name their format.
    pub(super) fn write_command(
        &self,
        writer: &mut impl Write,
        command: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), FetchError> {
        let mut line = b"command=".to_vec();
        line.extend_from_slice(command);
        line.push(b'\n');
        packet(writer, &line, cancel)?;
        if self.has(b"agent") {
            packet(writer, AGENT.as_bytes(), cancel)?;
        }
        if self.has(b"object-format") {
            let format = match self.format {
                ObjectFormat::Sha1 => b"object-format=sha1\n".as_slice(),
                ObjectFormat::Sha256 => b"object-format=sha256\n".as_slice(),
            };
            packet(writer, format, cancel)?;
        }
        put(writer, b"0001", cancel)?;
        Ok(())
    }
}

const AGENT: &str = concat!("agent=girt/", env!("CARGO_PKG_VERSION"), "\n");

fn split(line: &[u8]) -> (&[u8], Option<&[u8]>) {
    match crate::bytes::split_once(line, b'=') {
        Some((key, value)) => (key, Some(value)),
        None => (line, None),
    }
}

fn valid_key(key: &[u8]) -> bool {
    !key.is_empty()
        && key
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

// Deliberately a superset of the generic grammar's value alphabet: any printable ASCII, which also
// covers `agent` strings. Control and non-ASCII bytes are rejected.
fn valid_value(value: &[u8]) -> bool {
    !value.is_empty() && value.iter().all(|byte| (b' '..=b'~').contains(byte))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn read(lines: &[&str]) -> Result<Capabilities, FetchError> {
        let cancel = AtomicBool::new(false);
        let mut input = Vec::new();
        for line in lines {
            packet(&mut input, line.as_bytes(), &cancel).unwrap();
        }
        input.extend_from_slice(b"0000");
        let mut reader = input.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 4096,
            cancel: &cancel,
        };
        let capabilities = Capabilities::read(&mut wire, FetchLimits::default())?;
        wire.end().unwrap();
        Ok(capabilities)
    }

    #[test]
    fn parses_observed_git_advertisement() {
        // Observed from `git upload-pack` 2.56 with GIT_PROTOCOL=version=2.
        let capabilities = read(&[
            "agent=git/2.56.0-Darwin\n",
            "ls-refs=unborn\n",
            "fetch=shallow wait-for-done\n",
            "server-option\n",
            "object-format=sha1\n",
        ])
        .unwrap();
        assert_eq!(capabilities.object_format(), ObjectFormat::Sha1);
        assert!(capabilities.has(b"fetch"));
        assert!(capabilities.has(b"server-option"));
        assert!(capabilities.has_feature(b"fetch", b"shallow"));
        assert!(capabilities.has_feature(b"ls-refs", b"unborn"));
        assert!(!capabilities.has_feature(b"fetch", b"filter"));
        assert!(!capabilities.has(b"fetch=shallow"));
    }

    #[test]
    fn agent_accepts_punctuation_and_symref_is_not_advertised() {
        let capabilities = read(&[
            "agent=git/2.56.0-Darwin(x)~\n",
            "symref=HEAD:refs/heads/main\n",
            "unknown=kept\n",
        ])
        .unwrap();
        assert_eq!(
            capabilities.advertised_lines(),
            [
                b"agent=git/2.56.0-Darwin(x)~".to_vec(),
                b"unknown=kept".to_vec()
            ]
        );
    }

    #[test]
    fn absent_format_is_sha1_and_sha256_is_named() {
        assert_eq!(
            read(&["fetch\n"]).unwrap().object_format(),
            ObjectFormat::Sha1
        );
        assert_eq!(
            read(&["object-format=sha256\n"]).unwrap().object_format(),
            ObjectFormat::Sha256
        );
    }

    #[rstest]
    #[case::missing_lf(&["fetch"], "v2 capability line")]
    #[case::empty(&["\n"], "v2 capability token")]
    #[case::bad_key(&["fe tch\n"], "v2 capability token")]
    #[case::empty_value(&["fetch=\n"], "v2 capability token")]
    #[case::control(&["fetch=a\tb\n"], "v2 capability token")]
    #[case::duplicate(&["fetch\n", "fetch=shallow\n"], "duplicate v2 capability")]
    #[case::bare_format(&["object-format\n"], "v2 object-format value")]
    #[case::bare_agent(&["agent\n"], "v2 agent value")]
    #[case::agent_space(&["agent=a b\n"], "v2 agent value")]
    #[case::duplicate_format(
        &["object-format=sha1\n", "object-format=sha1\n"],
        "duplicate v2 capability"
    )]
    fn rejects_malformed_advertisements(#[case] lines: &[&str], #[case] reason: &str) {
        assert!(matches!(read(lines), Err(FetchError::Protocol(actual)) if actual == reason));
    }

    #[test]
    fn rejects_unknown_format_and_special_packets() {
        assert!(matches!(
            read(&["object-format=sha999\n"]),
            Err(FetchError::Unsupported("object format"))
        ));
        let cancel = AtomicBool::new(false);
        let mut reader = b"0001".as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 4,
            cancel: &cancel,
        };
        assert!(matches!(
            Capabilities::read(&mut wire, FetchLimits::default()),
            Err(FetchError::Protocol("v2 capability advertisement"))
        ));
    }

    #[test]
    fn command_request_echoes_advertised_agent_and_format() {
        let cancel = AtomicBool::new(false);
        let mut request = Vec::new();
        read(&["agent=x\n", "object-format=sha256\n"])
            .unwrap()
            .write_command(&mut request, b"fetch", &cancel)
            .unwrap();
        let mut expected = Vec::new();
        packet(&mut expected, b"command=fetch\n", &cancel).unwrap();
        packet(&mut expected, AGENT.as_bytes(), &cancel).unwrap();
        packet(&mut expected, b"object-format=sha256\n", &cancel).unwrap();
        expected.extend_from_slice(b"0001");
        assert_eq!(request, expected);

        let mut bare = Vec::new();
        read(&["ls-refs\n"])
            .unwrap()
            .write_command(&mut bare, b"ls-refs", &cancel)
            .unwrap();
        assert_eq!(bare, b"0014command=ls-refs\n0001");
    }
}
