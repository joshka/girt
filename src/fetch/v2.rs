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

use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;

use super::{AdvertisedRef, Advertisement, FetchError, FetchLimits};
use crate::packet::{Frame, Wire, packet, put};
use crate::refs::RefName;
use crate::{ObjectFormat, ObjectId};

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
    /// Runs one `ls-refs` command and returns its references as an advertisement.
    ///
    /// See [`Self::write_ls_refs`] and [`Self::read_ls_refs`] for the two halves.
    pub(super) fn ls_refs(
        &self,
        wire: &mut Wire<'_, impl Read>,
        writer: &mut impl Write,
        prefixes: &[&[u8]],
        limits: FetchLimits,
    ) -> Result<Advertisement, FetchError> {
        self.write_ls_refs(writer, prefixes, limits, wire.cancel)?;
        self.read_ls_refs(wire, limits)
    }

    /// Writes an `ls-refs` request for `peel`, `symrefs`, advertised `unborn`, and one
    /// `ref-prefix` per prefix.
    pub(super) fn write_ls_refs(
        &self,
        writer: &mut impl Write,
        prefixes: &[&[u8]],
        limits: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<(), FetchError> {
        if !self.has(b"ls-refs") {
            return Err(FetchError::Unsupported("v2 ls-refs"));
        }
        validate_prefixes(prefixes, limits)?;
        self.write_command(writer, b"ls-refs", cancel)?;
        packet(writer, b"peel\n", cancel)?;
        packet(writer, b"symrefs\n", cancel)?;
        if self.has_feature(b"ls-refs", b"unborn") {
            packet(writer, b"unborn\n", cancel)?;
        }
        for prefix in prefixes {
            let mut line = b"ref-prefix ".to_vec();
            line.extend_from_slice(prefix);
            line.push(b'\n');
            packet(writer, &line, cancel)?;
        }
        put(writer, b"0000", cancel)?;
        writer.flush()?;
        Ok(())
    }

    /// Reads an `ls-refs` response. The returned capabilities are this advertisement's lines plus
    /// a `symref=HEAD:<branch>` hint when HEAD names a branch, matching v0 HEAD interpretation.
    pub(super) fn read_ls_refs(
        &self,
        wire: &mut Wire<'_, impl Read>,
        limits: FetchLimits,
    ) -> Result<Advertisement, FetchError> {
        let (refs, head) = read_refs(wire, limits, self.format)?;
        let mut capabilities = self.advertised_lines();
        if let Some(branch) = head {
            let mut hint = b"symref=HEAD:".to_vec();
            hint.extend_from_slice(branch.as_bytes());
            capabilities.push(hint);
        }
        Ok(Advertisement { refs, capabilities })
    }
}

/// Rejects `ref-prefix` arguments that cannot be sent as one well-formed request line.
pub(super) fn validate_prefixes(prefixes: &[&[u8]], limits: FetchLimits) -> Result<(), FetchError> {
    if prefixes.len() > limits.max_refs {
        return Err(FetchError::Limit("ref prefixes"));
    }
    for prefix in prefixes {
        if prefix.is_empty() || prefix.len() > 4096 || prefix.iter().any(|b| matches!(b, 0 | b'\n'))
        {
            return Err(FetchError::Protocol("ref prefix"));
        }
    }
    Ok(())
}

fn read_refs(
    wire: &mut Wire<'_, impl Read>,
    limits: FetchLimits,
    format: ObjectFormat,
) -> Result<(Vec<AdvertisedRef>, Option<RefName>), FetchError> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let mut head_hint = None;
    while let Some(line) = wire.packet()? {
        let line = line
            .strip_suffix(b"\n")
            .ok_or(FetchError::Protocol("v2 reference line"))?;
        let mut parts = line.split(|byte| *byte == b' ');
        let raw_id = parts.next().ok_or(FetchError::Protocol("v2 reference"))?;
        let raw_name = parts.next().ok_or(FetchError::Protocol("v2 reference"))?;
        let name = RefName::new(raw_name).map_err(|_| FetchError::Protocol("v2 ref name"))?;
        if !seen.insert(name.clone()) {
            return Err(FetchError::Protocol("duplicate v2 ref"));
        }
        let unborn = raw_id == b"unborn";
        let id = if unborn {
            None
        } else {
            let value = std::str::from_utf8(raw_id)
                .ok()
                .and_then(|raw| ObjectId::from_hex(format, raw).ok())
                .filter(|id| !id.is_null())
                .ok_or(FetchError::Protocol("v2 object ID"))?;
            Some(value)
        };
        let mut peeled = None;
        let mut target = None;
        for attribute in parts {
            if let Some(raw) = attribute.strip_prefix(b"symref-target:") {
                let value =
                    RefName::new(raw).map_err(|_| FetchError::Protocol("v2 symref target"))?;
                if target.replace(value).is_some() {
                    return Err(FetchError::Protocol("duplicate v2 symref target"));
                }
            } else if let Some(raw) = attribute.strip_prefix(b"peeled:") {
                let value = std::str::from_utf8(raw)
                    .ok()
                    .and_then(|raw| ObjectId::from_hex(format, raw).ok())
                    .filter(|id| !id.is_null())
                    .ok_or(FetchError::Protocol("v2 peeled ID"))?;
                if peeled.replace(value).is_some() {
                    return Err(FetchError::Protocol("duplicate v2 peeled ID"));
                }
            } else {
                return Err(FetchError::Protocol("v2 ref attribute"));
            }
        }
        if name.as_bytes() == b"HEAD" {
            head_hint = target;
        } else if unborn {
            return Err(FetchError::Unsupported("v2 non-HEAD unborn ref"));
        }
        if unborn && head_hint.is_none() {
            return Err(FetchError::Protocol("unborn HEAD without target"));
        }
        if let Some(id) = id {
            if refs.len() == limits.max_refs {
                return Err(FetchError::Limit("advertised refs"));
            }
            refs.push(AdvertisedRef {
                name: name.clone(),
                id,
                kind: crate::fetch::AdvertisementKind::Tip,
            });
            if let Some(id) = peeled {
                if !name.as_bytes().starts_with(b"refs/tags/") {
                    return Err(FetchError::Protocol("v2 non-tag peel"));
                }
                if refs.len() == limits.max_refs {
                    return Err(FetchError::Limit("advertised refs"));
                }
                refs.push(AdvertisedRef {
                    name,
                    id,
                    kind: crate::fetch::AdvertisementKind::PeeledHint,
                });
            }
        } else if peeled.is_some() {
            return Err(FetchError::Protocol("unborn peeled ref"));
        }
    }
    Ok((refs, head_hint))
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
        assert_eq!(capabilities.format, ObjectFormat::Sha1);
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
        assert_eq!(read(&["fetch\n"]).unwrap().format, ObjectFormat::Sha1);
        assert_eq!(
            read(&["object-format=sha256\n"]).unwrap().format,
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

    #[rstest]
    #[case::empty(&[b"".as_slice()])]
    #[case::newline(&[b"refs/a\n".as_slice()])]
    #[case::nul(&[b"refs/\0".as_slice()])]
    fn rejects_unsendable_prefixes(#[case] prefixes: &[&[u8]]) {
        assert!(matches!(
            validate_prefixes(prefixes, FetchLimits::default()),
            Err(FetchError::Protocol("ref prefix"))
        ));
    }

    #[test]
    fn bounds_prefix_count() {
        let limits = FetchLimits {
            max_refs: 1,
            ..FetchLimits::default()
        };
        assert!(validate_prefixes(&[b"a"], limits).is_ok());
        assert!(matches!(
            validate_prefixes(&[b"a", b"b"], limits),
            Err(FetchError::Limit("ref prefixes"))
        ));
    }

    #[test]
    fn prefix_length_boundary() {
        let longest = vec![b'r'; 4096];
        let limits = FetchLimits::default();
        assert!(validate_prefixes(&[&longest], limits).is_ok());
        let longer = vec![b'r'; 4097];
        assert!(matches!(
            validate_prefixes(&[&longer], limits),
            Err(FetchError::Protocol("ref prefix"))
        ));
    }

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn ls_refs(lines: &[String]) -> Result<(Vec<AdvertisedRef>, Option<RefName>), FetchError> {
        let cancel = AtomicBool::new(false);
        let mut input = Vec::new();
        for line in lines {
            match line.as_str() {
                "0001" | "0002" => input.extend_from_slice(line.as_bytes()),
                line => packet(&mut input, line.as_bytes(), &cancel).unwrap(),
            }
        }
        input.extend_from_slice(b"0000");
        let mut reader = input.as_slice();
        let mut wire = Wire {
            reader: &mut reader,
            remaining: 4096,
            cancel: &cancel,
        };
        read_refs(&mut wire, FetchLimits::default(), ObjectFormat::Sha1)
    }

    #[rstest]
    #[case::duplicate_target(
        format!("{A} HEAD symref-target:refs/heads/a symref-target:refs/heads/b\n"),
        "duplicate v2 symref target"
    )]
    #[case::duplicate_peel(format!("{A} refs/tags/t peeled:{B} peeled:{B}\n"), "duplicate v2 peeled ID")]
    #[case::null_id(format!("{} refs/heads/a\n", "0".repeat(40)), "v2 object ID")]
    #[case::wrong_format(format!("{A}{A} refs/heads/a\n"), "v2 object ID")]
    #[case::bad_peel(format!("{A} refs/tags/t peeled:{}\n", "0".repeat(40)), "v2 peeled ID")]
    #[case::invalid_name(format!("{A} refs/heads/a..b\n"), "v2 ref name")]
    #[case::unknown_attribute(format!("{A} refs/heads/a future:x\n"), "v2 ref attribute")]
    #[case::missing_lf(format!("{A} refs/heads/a"), "v2 reference line")]
    #[case::non_tag_peel(format!("{A} refs/heads/a peeled:{B}\n"), "v2 non-tag peel")]
    #[case::unborn_without_target("unborn HEAD\n".to_owned(), "unborn HEAD without target")]
    #[case::unborn_peeled(
        format!("unborn HEAD symref-target:refs/heads/main peeled:{B}\n"),
        "unborn peeled ref"
    )]
    #[case::delimiter("0001".to_owned(), "pkt-line length")]
    #[case::response_end("0002".to_owned(), "pkt-line length")]
    fn ls_refs_rejects_malformed_responses(#[case] line: String, #[case] reason: &str) {
        assert!(matches!(
            ls_refs(&[line]),
            Err(FetchError::Protocol(actual)) if actual == reason
        ));
    }

    #[test]
    fn ls_refs_rejects_duplicates_and_non_head_unborn() {
        let line = format!("{A} refs/heads/a\n");
        assert!(matches!(
            ls_refs(&[line.clone(), line]),
            Err(FetchError::Protocol("duplicate v2 ref"))
        ));
        assert!(matches!(
            ls_refs(&["unborn refs/heads/a\n".to_owned()]),
            Err(FetchError::Unsupported("v2 non-HEAD unborn ref"))
        ));
    }

    #[test]
    fn ls_refs_accepts_refs_outside_requested_prefixes() {
        // Servers may ignore `ref-prefix`; the inventory is kept as returned.
        let (refs, _) = ls_refs(&[
            format!("{A} refs/heads/main\n"),
            format!("{B} refs/notes/unrequested\n"),
        ])
        .unwrap();
        assert_eq!(refs.len(), 2);
    }
}
