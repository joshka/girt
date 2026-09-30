//! Pure configured refspec syntax, independent of mapping and transport policy.

use super::Direction;
use crate::refs::RefName;

/// The operation described by a configured refspec.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ConfiguredRefspecKind {
    /// Fetch a source without naming a destination.
    Selection,
    /// Exclude a source or source pattern in either direction.
    Exclusion,
    /// Map a source or source pattern to a destination.
    Mapping,
    /// Push branches having matching names at both ends.
    AllMatching,
    /// Delete a push destination.
    Deletion,
}

/// A byte-preserving description of configured fetch or push syntax.
///
/// Unlike [`super::Refspec`], this type accepts shorthand names, positive object IDs and matching
/// push syntax. An explicit, non-pattern push source is an opaque revision expression. Parsing does
/// not resolve names, expand patterns, authorize updates, or establish that an operation can be
/// executed. Use the stricter mapping API when those are its requirements.
///
/// Empty fetch values select `HEAD`; they do not reset a collection. This parser handles one value
/// at a time and performs no collection sorting or deduplication.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConfiguredRefspec {
    kind: ConfiguredRefspecKind,
    source: Option<Vec<u8>>,
    destination: Option<Vec<u8>>,
    force: bool,
}

/// Invalid configured refspec syntax.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum ConfiguredRefspecError {
    /// A source or destination violates reference-name syntax.
    #[error("invalid configured refspec name")]
    Name,
    /// An exclusion has no source, has a destination, or names a full object ID.
    #[error("invalid configured refspec exclusion")]
    Exclusion,
    /// A pattern has multiple wildcards or lacks a corresponding destination pattern.
    #[error("unbalanced configured refspec pattern")]
    Pattern,
    /// A push value is empty.
    #[error("empty configured push refspec")]
    EmptyPush,
    /// A push source explicitly names an empty destination.
    #[error("empty configured push destination")]
    EmptyDestination,
}

impl ConfiguredRefspec {
    /// Parses one configured value without I/O or UTF-8 conversion.
    ///
    /// An implicit configuration value must be passed as an empty slice. Fetch selections discard
    /// a redundant force marker or empty destination; push deletions discard a force marker.
    /// Negative refspecs are accepted for both directions, but cannot be forced or name a full
    /// 40- or 64-digit hexadecimal object ID. Standalone `tag <name>` command-line shorthand is not
    /// accepted. An `@` source normalizes to `HEAD`. Explicit push destinations allow opaque
    /// non-pattern revision sources; their syntax and resolvability are not checked here. A mapping
    /// pattern requires exactly one `*` on each side; an exclusion permits one source wildcard.
    ///
    /// # Errors
    ///
    /// Returns [`ConfiguredRefspecError`] for malformed names, exclusions or patterns, and for an
    /// empty push value. A successfully parsed descriptor may still require unsupported policy
    /// or name resolution in the caller.
    ///
    /// ```
    /// use girt::remote::{ConfiguredRefspec, ConfiguredRefspecKind, Direction};
    /// let spec = ConfiguredRefspec::parse(Direction::Fetch, b"+main:")?;
    /// assert_eq!(spec.kind(), ConfiguredRefspecKind::Selection);
    /// assert_eq!(spec.to_bytes(), b"main");
    /// # Ok::<(), girt::remote::ConfiguredRefspecError>(())
    /// ```
    pub fn parse(direction: Direction, bytes: &[u8]) -> Result<Self, ConfiguredRefspecError> {
        use ConfiguredRefspecKind as Kind;
        if let Some(source) = bytes.strip_prefix(b"^") {
            if source.is_empty()
                || source.contains(&b':')
                || (matches!(source.len(), 40 | 64) && source.iter().all(u8::is_ascii_hexdigit))
            {
                return Err(ConfiguredRefspecError::Exclusion);
            }
            let source = normalize_source(source);
            validate_name(source)?;
            return Ok(Self {
                kind: Kind::Exclusion,
                source: Some(source.to_vec()),
                destination: None,
                force: false,
            });
        }
        let (force, body) = bytes
            .strip_prefix(b"+")
            .map_or((false, bytes), |b| (true, b));
        let (source, destination) = match body.iter().rposition(|b| *b == b':') {
            Some(colon) => (&body[..colon], Some(&body[colon + 1..])),
            None => (body, None),
        };
        if direction == Direction::Push && !source.is_empty() && destination == Some(b"".as_slice())
        {
            return Err(ConfiguredRefspecError::EmptyDestination);
        }
        let explicit_destination = destination.is_some();
        let source = normalize_source(source);
        let destination = destination.filter(|name| !name.is_empty());
        if direction == Direction::Push && bytes.is_empty() {
            return Err(ConfiguredRefspecError::EmptyPush);
        }
        let (kind, source, destination, force) = match (direction, source, destination) {
            (Direction::Fetch, source, destination) => {
                let source = if source.is_empty() { b"HEAD" } else { source };
                let kind = if destination.is_some() {
                    Kind::Mapping
                } else {
                    Kind::Selection
                };
                (
                    kind,
                    Some(source),
                    destination,
                    force && destination.is_some(),
                )
            }
            (Direction::Push, [], None) => (Kind::AllMatching, None, None, force),
            (Direction::Push, [], Some(destination)) => {
                (Kind::Deletion, None, Some(destination), false)
            }
            (Direction::Push, source, destination) => (
                Kind::Mapping,
                Some(source),
                Some(destination.unwrap_or(source)),
                force,
            ),
        };
        // An explicit non-pattern push source is an opaque revision expression. Its eventual
        // resolution belongs to the caller; implicit destinations and patterns remain names.
        let source_pattern = match source {
            Some(source)
                if direction == Direction::Push
                    && explicit_destination
                    && !source.contains(&b'*') =>
            {
                false
            }
            Some(source) => validate_name(source)?,
            None => false,
        };
        let destination_pattern = destination.map(validate_name).transpose()?.unwrap_or(false);
        if source_pattern != destination_pattern {
            return Err(ConfiguredRefspecError::Pattern);
        }
        Ok(Self {
            kind,
            source: source.map(<[u8]>::to_vec),
            destination: destination.map(<[u8]>::to_vec),
            force,
        })
    }

    /// Returns the operation described by this value.
    pub fn kind(&self) -> ConfiguredRefspecKind {
        self.kind
    }

    /// Borrows the source bytes, if this operation has a source.
    pub fn source(&self) -> Option<&[u8]> {
        self.source.as_deref()
    }

    /// Borrows the destination bytes, if this operation has a destination.
    pub fn destination(&self) -> Option<&[u8]> {
        self.destination.as_deref()
    }

    /// Returns whether the mapping or matching push permits forced updates.
    pub fn force(&self) -> bool {
        self.force
    }

    /// Serializes the normalized descriptor without UTF-8 conversion.
    ///
    /// Fetch defaults become `HEAD`; selections omit redundant `+` and `:`; push mappings always
    /// include a destination; deletions omit redundant force. Parsing these bytes in the original
    /// direction reproduces an equal descriptor.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        if self.force {
            bytes.push(b'+');
        }
        if self.kind == ConfiguredRefspecKind::Exclusion {
            bytes.push(b'^');
        }
        if let Some(source) = &self.source {
            bytes.extend_from_slice(source);
        }
        if matches!(
            self.kind,
            ConfiguredRefspecKind::Mapping
                | ConfiguredRefspecKind::Deletion
                | ConfiguredRefspecKind::AllMatching
        ) {
            bytes.push(b':');
        }
        if let Some(destination) = &self.destination {
            bytes.extend_from_slice(destination);
        }
        bytes
    }
}

fn normalize_source(bytes: &[u8]) -> &[u8] {
    if bytes == b"@" { b"HEAD" } else { bytes }
}

/// Validate shorthand names with the same component rules as full names.
fn validate_name(bytes: &[u8]) -> Result<bool, ConfiguredRefspecError> {
    if bytes == b"@" {
        return Err(ConfiguredRefspecError::Name);
    }
    let wildcards = bytes.iter().filter(|b| **b == b'*').count();
    if wildcards > 1 {
        return Err(ConfiguredRefspecError::Pattern);
    }
    let mut full = b"refs/".to_vec();
    full.extend(bytes.iter().map(|b| if *b == b'*' { b'x' } else { *b }));
    RefName::new(full).map_err(|_| ConfiguredRefspecError::Name)?;
    Ok(wildcards == 1)
}

#[cfg(test)]
mod tests {
    use ConfiguredRefspecKind::{AllMatching, Deletion, Exclusion, Mapping, Selection};
    use Direction::{Fetch, Push};
    use rstest::rstest;

    use super::*;

    // Original fixtures observed through the public configured-remote API. These do not execute
    // mappings, and deliberately include syntax outside the executable Refspec subset.
    #[rstest]
    #[case::fetch_empty(Fetch, b"", b"HEAD", Selection, Some(b"HEAD".as_slice()), None, false)]
    #[case::fetch_colon(Fetch, b":", b"HEAD", Selection, Some(b"HEAD".as_slice()), None, false)]
    #[case::fetch_plus(Fetch, b"+", b"HEAD", Selection, Some(b"HEAD".as_slice()), None, false)]
    #[case::fetch_short(Fetch, b"main", b"main", Selection, Some(b"main".as_slice()), None, false)]
    #[case::fetch_empty_destination(Fetch, b"+main:", b"main", Selection, Some(b"main".as_slice()), None, false)]
    #[case::fetch_default_mapping(Fetch, b"+:origin/main", b"+HEAD:origin/main", Mapping, Some(b"HEAD".as_slice()), Some(b"origin/main".as_slice()), true)]
    #[case::fetch_at(Fetch, b"@", b"HEAD", Selection, Some(b"HEAD".as_slice()), None, false)]
    #[case::push_at(Push, b"@", b"HEAD:HEAD", Mapping, Some(b"HEAD".as_slice()), Some(b"HEAD".as_slice()), false)]
    #[case::push_short(Push, b"main", b"main:main", Mapping, Some(b"main".as_slice()), Some(b"main".as_slice()), false)]
    #[case::push_matching(Push, b":", b":", AllMatching, None, None, false)]
    #[case::push_matching_force(Push, b"+", b"+:", AllMatching, None, None, true)]
    #[case::push_delete(Push, b"+:main", b":main", Deletion, None, Some(b"main".as_slice()), false)]
    #[case::push_revision(Push, b"+HEAD~1:main", b"+HEAD~1:main", Mapping, Some(b"HEAD~1".as_slice()), Some(b"main".as_slice()), true)]
    #[case::push_colon_revision(Push, b"HEAD:file:main", b"HEAD:file:main", Mapping, Some(b"HEAD:file".as_slice()), Some(b"main".as_slice()), false)]
    #[case::fetch_negative(Fetch, b"^main", b"^main", Exclusion, Some(b"main".as_slice()), None, false)]
    #[case::push_negative(Push, b"^main", b"^main", Exclusion, Some(b"main".as_slice()), None, false)]
    #[case::negative_plus(Fetch, b"^+refs/a", b"^+refs/a", Exclusion, Some(b"+refs/a".as_slice()), None, false)]
    #[case::negative_at(Push, b"^@", b"^HEAD", Exclusion, Some(b"HEAD".as_slice()), None, false)]
    fn describes_and_normalizes(
        #[case] direction: Direction,
        #[case] input: &[u8],
        #[case] canonical: &[u8],
        #[case] kind: ConfiguredRefspecKind,
        #[case] source: Option<&[u8]>,
        #[case] destination: Option<&[u8]>,
        #[case] force: bool,
    ) {
        let spec = ConfiguredRefspec::parse(direction, input).unwrap();
        assert_eq!(spec.kind(), kind);
        assert_eq!(spec.source(), source);
        assert_eq!(spec.destination(), destination);
        assert_eq!(spec.force(), force);
        assert_eq!(spec.to_bytes(), canonical);
        assert_eq!(
            ConfiguredRefspec::parse(direction, canonical).unwrap(),
            spec
        );
    }

    #[rstest]
    #[case::mapping(b"+refs/a:refs/b")]
    #[case::partial_pattern(b"a*:b*")]
    #[case::whole_pattern(b"*:refs/*")]
    #[case::negative_pattern(b"^refs/*")]
    #[case::negative_abbreviated_hash(b"^1111111")]
    #[case::negative_nonhex_40(b"^z111111111111111111111111111111111111111")]
    #[case::sha1_mapping(b"+1111111111111111111111111111111111111111:refs/b")]
    #[case::sha256_mapping(
        b"1111111111111111111111111111111111111111111111111111111111111111:refs/b"
    )]
    #[case::non_utf8(b"+refs/\xff:refs/\xfe")]
    #[case::negative_non_utf8(b"^refs/\xff")]
    fn roundtrips_both_directions(
        #[values(Fetch, Push)] direction: Direction,
        #[case] input: &[u8],
    ) {
        let spec = ConfiguredRefspec::parse(direction, input).unwrap();
        assert_eq!(spec.to_bytes(), input);
        assert_eq!(
            ConfiguredRefspec::parse(direction, &spec.to_bytes()).unwrap(),
            spec
        );
    }

    #[rstest]
    #[case::empty_negative(b"^")]
    #[case::negative_destination(b"^main:other")]
    #[case::negative_empty_destination(b"^main:")]
    #[case::negative_sha1(b"^1111111111111111111111111111111111111111")]
    #[case::negative_sha256(b"^1111111111111111111111111111111111111111111111111111111111111111")]
    #[case::forced_negative(b"+^refs/a")]
    #[case::multiple_patterns(b"a**:b**")]
    #[case::unbalanced_source(b"a*:b")]
    #[case::unbalanced_destination(b"a:b*")]
    #[case::multiple_negative_patterns(b"^a**")]
    #[case::invalid_pattern_name(b"a~*:b*")]
    #[case::destination_at(b"a:@")]
    #[case::destination_control(b"a:b\0")]
    #[case::destination_dotdot(b"a:b..c")]
    #[case::destination_lock(b"a:b.lock")]
    #[case::destination_slashes(b"a:b//c")]
    #[case::tag_shorthand(b"tag v1")]
    fn rejects_malformed_both_directions(
        #[values(Fetch, Push)] direction: Direction,
        #[case] input: &[u8],
    ) {
        assert!(ConfiguredRefspec::parse(direction, input).is_err());
    }

    #[rstest]
    #[case::selection_pattern(Fetch, b"refs/*")]
    #[case::selection_pattern_empty_destination(Fetch, b"refs/*:")]
    #[case::fetch_revision(Fetch, b"HEAD~1:main")]
    #[case::fetch_multiple_colons(Fetch, b"a:b:c")]
    #[case::push_empty(Push, b"")]
    #[case::push_empty_destination(Push, b"main:")]
    #[case::push_revision_without_destination(Push, b"HEAD~1")]
    fn rejects_direction_specific_syntax(#[case] direction: Direction, #[case] input: &[u8]) {
        assert!(ConfiguredRefspec::parse(direction, input).is_err());
    }

    #[rstest]
    #[case::sha1(b"1111111111111111111111111111111111111111")]
    #[case::sha256(b"1111111111111111111111111111111111111111111111111111111111111111")]
    fn fetch_selects_positive_object_ids(#[case] source: &[u8]) {
        let spec = ConfiguredRefspec::parse(Fetch, source).unwrap();
        assert_eq!(spec.kind(), Selection);
        assert_eq!(spec.source(), Some(source));
        assert_eq!(spec.to_bytes(), source);
    }

    #[test]
    fn push_implicit_pattern_maps_to_itself() {
        let spec = ConfiguredRefspec::parse(Push, b"+refs/*").unwrap();
        assert_eq!(spec.to_bytes(), b"+refs/*:refs/*");
        assert_eq!(
            ConfiguredRefspec::parse(Push, &spec.to_bytes()).unwrap(),
            spec
        );
    }

    #[test]
    fn parsing_occurrences_does_not_reset_sort_or_deduplicate() {
        let inputs: &[&[u8]] = &[b"z", b"", b"a", b"z"];
        let specs = inputs
            .iter()
            .map(|s| ConfiguredRefspec::parse(Fetch, s).unwrap().to_bytes())
            .collect::<Vec<_>>();
        assert_eq!(
            specs,
            [
                b"z".to_vec(),
                b"HEAD".to_vec(),
                b"a".to_vec(),
                b"z".to_vec()
            ]
        );
    }
}
