use std::collections::HashSet;

use crate::ObjectId;
use crate::fetch::Advertisement;
use crate::refs::RefName;

/// Which side supplies source names and which refspec grammar applies.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Direction {
    /// Remote sources map to local destinations; negative exclusions are permitted.
    Fetch,
    /// Local sources map to remote destinations; deletion is permitted.
    Push,
}

/// One validated byte-preserving specification in the supported full-name subset.
///
/// Sources are full `refs/…` names or exact `HEAD`; destinations must be under `refs/`.
/// One `*` may occur anywhere in each side of a wildcard mapping and captures arbitrary bytes,
/// including slashes or the empty string. Negative fetch specs have only a source, optionally
/// with one `*`. Push deletion uses `:refs/…`. A leading `+` records force intent only.
///
/// Fetch `src` and `src:` select without a destination. Push `refs/…` maps to the same name;
/// push `src:` is rejected. HEAD requires an explicit push destination. Shorthand, object IDs,
/// revision expressions, matching push (`:`), empty/default fetch and `tag <name>` are unsupported.
/// Parsing does not resolve names, authorize force, or validate an update against storage.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Refspec {
    raw: Vec<u8>,
    source: Vec<u8>,
    destination: Option<Vec<u8>>,
    force: bool,
    negative: bool,
    wildcard: bool,
}

/// A rejected occurrence in an ordered refspec list.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("refspec at index {index}: {source}")]
pub struct RefspecsError {
    /// Zero-based occurrence index.
    pub index: usize,
    /// Syntax or unsupported-form diagnostic.
    #[source]
    pub source: RefspecError,
}

/// Invalid syntax or a recognized form outside the supported full-name subset.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum RefspecError {
    /// Invalid separators, markers, negative form or wildcard count/pairing.
    #[error("invalid refspec syntax: {0}")]
    Syntax(&'static str),
    /// Unsupported default/matching/name-resolution form, with no fallback interpretation.
    #[error("unsupported refspec form: {0}")]
    Unsupported(&'static str),
    /// A full name or wildcard pattern violates reference-name rules.
    #[error("invalid full reference name or pattern")]
    Name,
}

impl Refspec {
    /// Parses one specification under the requested direction's grammar.
    ///
    /// # Errors
    ///
    /// Returns a structured syntax, unsupported-form, or name diagnostic. Inputs are never trimmed
    /// or normalized. See [`Refspec`] for the accepted subset.
    pub fn parse(direction: Direction, bytes: &[u8]) -> Result<Self, RefspecError> {
        let force = bytes.starts_with(b"+");
        let body = if force { &bytes[1..] } else { bytes };
        let negative = body.starts_with(b"^");
        let body = if negative { &body[1..] } else { body };
        if negative && (direction != Direction::Fetch || force || body.contains(&b':')) {
            return Err(RefspecError::Syntax(
                "negative specs are fetch-only, without + or destination",
            ));
        }
        if body.is_empty() || body == b":" || body.starts_with(b"tag ") {
            return Err(RefspecError::Unsupported(
                "empty/default, matching, or tag shorthand",
            ));
        }
        let mut parts = body.split(|byte| *byte == b':');
        let source = parts.next().unwrap();
        let explicit_destination = parts.next();
        if parts.next().is_some() {
            return Err(RefspecError::Syntax("more than one colon"));
        }
        let destination = match (direction, explicit_destination, negative) {
            (_, _, true) => None,
            (Direction::Fetch, None | Some(b""), false) => None,
            (Direction::Push, None, false) => Some(source),
            (Direction::Push, Some(b""), false) => {
                return Err(RefspecError::Unsupported("empty push destination"));
            }
            (_, value, false) => value,
        };
        if source.is_empty() && direction == Direction::Fetch {
            return Err(RefspecError::Unsupported("empty fetch source"));
        }
        let source_stars = source.iter().filter(|b| **b == b'*').count();
        let destination_stars = destination.map_or(0, |s| s.iter().filter(|b| **b == b'*').count());
        if source_stars > 1
            || destination_stars > 1
            || (!negative && source_stars != destination_stars)
        {
            return Err(RefspecError::Syntax(
                "wildcards require exactly one * on both sides",
            ));
        }
        if !source.is_empty() {
            validate_pattern(source, true)?;
        }
        if let Some(destination) = destination {
            validate_pattern(destination, false)?;
        }
        Ok(Self {
            raw: bytes.to_vec(),
            source: source.to_vec(),
            destination: destination.map(<[u8]>::to_vec),
            force,
            negative,
            wildcard: source_stars == 1,
        })
    }

    /// Original bytes, including separators and markers, without normalization.
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Whether `+` was written; this does not grant update permission.
    pub fn force(&self) -> bool {
        self.force
    }

    /// Whether this specification excludes fetch sources.
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    fn capture<'a>(&self, name: &'a [u8]) -> Option<&'a [u8]> {
        if self.wildcard {
            let star = self.source.iter().position(|b| *b == b'*').unwrap();
            let prefix = &self.source[..star];
            let suffix = &self.source[star + 1..];
            name.strip_prefix(prefix)?.strip_suffix(suffix)
        } else {
            (self.source == name).then_some(b"")
        }
    }

    fn destination(&self, capture: &[u8]) -> Result<Option<RefName>, MappingError> {
        let Some(pattern) = &self.destination else {
            return Ok(None);
        };
        let bytes = if let Some(star) = pattern.iter().position(|b| *b == b'*') {
            [&pattern[..star], capture, &pattern[star + 1..]].concat()
        } else {
            pattern.clone()
        };
        let name = RefName::new(bytes).map_err(|_| MappingError::InvalidDestination)?;
        Ok(Some(name))
    }

    /// Source name corresponding to a destination owned by this positive fetch mapping.
    /// Source-only and negative specifications do not own destinations.
    pub(crate) fn source_for_destination(&self, destination: &RefName) -> Option<Vec<u8>> {
        if self.negative {
            return None;
        }
        let pattern = self.destination.as_ref()?;
        let capture = if let Some(star) = pattern.iter().position(|byte| *byte == b'*') {
            destination
                .as_bytes()
                .strip_prefix(&pattern[..star])?
                .strip_suffix(&pattern[star + 1..])?
        } else if pattern == destination.as_bytes() {
            b""
        } else {
            return None;
        };
        if let Some(star) = self.source.iter().position(|byte| *byte == b'*') {
            Some([&self.source[..star], capture, &self.source[star + 1..]].concat())
        } else {
            Some(self.source.clone())
        }
    }
}

fn validate_pattern(bytes: &[u8], source: bool) -> Result<(), RefspecError> {
    if source && bytes == b"HEAD" {
        return Ok(());
    }
    if !bytes.starts_with(b"refs/") {
        return Err(RefspecError::Unsupported(
            "requires full refs/ name; only exact source HEAD is allowed",
        ));
    }
    // Replacing the one wildcard permits validation with the same full-name rules as stored refs.
    let probe: Vec<_> = bytes
        .iter()
        .map(|b| if *b == b'*' { b'x' } else { *b })
        .collect();
    RefName::new(probe).map_err(|_| RefspecError::Name)?;
    Ok(())
}

/// A resolved source supplied explicitly by the caller, without symbolic-reference traversal.
///
/// Local symbolic refs must be resolved by the caller. Advertisement conversion keeps the
/// advertised name and ID, including HEAD, and discards peeled hints. Symbolic capabilities do not
/// rename refs.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RefSource {
    /// Full source name or HEAD.
    pub name: RefName,
    /// Nonzero SHA-1 tip; mapping rejects zero IDs, without reading the object.
    pub id: ObjectId,
}

/// One pure selection, mapping, or push deletion; not an authorized reference update.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Mapping {
    /// Selected source and ID; `None` means push deletion.
    pub source: Option<RefSource>,
    /// Full destination; `None` means fetch selection without a tracking update.
    pub destination: Option<RefName>,
    /// Syntactic `+` intent. Callers separately authorize force and validate expected old values,
    /// ancestry, object kinds, namespace restrictions and storage conflicts.
    pub force: bool,
}

/// An ordered set of specifications sharing one direction, including duplicate occurrences.
#[derive(Debug, Clone)]
pub struct Refspecs {
    direction: Direction,
    specs: Vec<Refspec>,
}

/// Mapping failed; no partial plan or side effects are produced.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum MappingError {
    /// An explicit positive source was absent, even if an exclusion names it too.
    #[error("explicit refspec source not found: {0:?}")]
    UnmatchedSource(Vec<u8>),
    /// Repeated input names are ambiguous, even if IDs agree.
    #[error("duplicate source name: {0:?}")]
    DuplicateSource(RefName),
    /// A supplied source is the wire-format zero sentinel, not an object tip.
    #[error("source has a zero object ID: {0:?}")]
    ZeroId(RefName),
    /// Distinct mappings target the same destination, including different force intent or source
    /// names with equal IDs. Exact duplicates alone are collapsed.
    #[error("conflicting mappings for destination: {0:?}")]
    Collision(RefName),
    /// A wildcard capture produces an invalid full reference name.
    #[error("wildcard substitution produced an invalid destination")]
    InvalidDestination,
    /// Advertisement mapping is only defined for fetch specifications.
    #[error("advertisement selection requires fetch refspecs")]
    Direction,
}

impl Refspecs {
    /// Parses all occurrences in order. Empty input produces an empty plan, without defaults.
    ///
    /// # Errors
    ///
    /// Returns the zero-based occurrence index and parser diagnostic for the first rejected spec.
    pub fn parse<I, B>(direction: Direction, values: I) -> Result<Self, RefspecsError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let specs = values
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| {
                Refspec::parse(direction, bytes.as_ref())
                    .map_err(|source| RefspecsError { index, source })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { direction, specs })
    }

    /// Direction fixed during construction.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Parsed specifications in original order, retaining duplicates and negative entries.
    pub fn specs(&self) -> &[Refspec] {
        &self.specs
    }

    /// Source names whose positive mappings own a tracking destination for pruning.
    /// Negative selectors protect their matching destinations from deletion.
    pub(crate) fn prune_sources(&self, destination: &RefName) -> Vec<Vec<u8>> {
        self.specs
            .iter()
            .filter_map(|spec| spec.source_for_destination(destination))
            .filter(|source| {
                !self
                    .specs
                    .iter()
                    .any(|spec| spec.negative && spec.capture(source).is_some())
            })
            .collect()
    }

    /// Maps explicit source tips without consulting objects, references or the environment.
    ///
    /// Results follow positive-spec order, then input order. All negative specs apply globally,
    /// regardless of position. Explicit missing sources fail; unmatched wildcards/exclusions are
    /// harmless. Exact duplicate mappings collapse, preserving first occurrence. Destination-free
    /// selections can coexist with mappings of the same source. Push deletions need no source or
    /// destination advertisement. No destination existence, ancestry or filesystem checks occur.
    ///
    /// Work is proportional to specs × sources plus exclusion checks per match. Storage is
    /// proportional to inputs and unique mappings. Callers bound those inputs and output size;
    /// this API does not impose a resource limit or a cancellation policy.
    ///
    /// # Errors
    ///
    /// Rejects duplicate input names, zero IDs, missing explicit sources, invalid substituted
    /// destinations and conflicting destination mappings. Even unused input entries are checked.
    pub fn map(&self, sources: &[RefSource]) -> Result<Vec<Mapping>, MappingError> {
        self.map_with_missing(sources).map(|(mappings, missing)| {
            if let Some(source) = missing.into_iter().next() {
                Err(MappingError::UnmatchedSource(source))
            } else {
                Ok(mappings)
            }
        })?
    }

    /// Maps valid sources while reporting absent exact positive selectors separately.
    ///
    /// This permits fetch callers to install and publish other advertised selections while
    /// showing each missing explicit source. Wildcards with no match are harmless.
    ///
    /// # Errors
    ///
    /// Rejects duplicate input names, zero IDs, invalid destinations and collisions.
    pub fn map_with_missing(
        &self,
        sources: &[RefSource],
    ) -> Result<(Vec<Mapping>, Vec<Vec<u8>>), MappingError> {
        let mut names = HashSet::new();
        for source in sources {
            if !names.insert(&source.name) {
                return Err(MappingError::DuplicateSource(source.name.clone()));
            }
            if source.id.is_null() {
                return Err(MappingError::ZeroId(source.name.clone()));
            }
        }
        let mut plan = Plan::default();
        let mut missing = Vec::new();
        for spec in self.specs.iter().filter(|spec| !spec.negative) {
            if spec.source.is_empty() {
                plan.add(Mapping {
                    source: None,
                    destination: spec.destination(b"")?,
                    force: spec.force,
                })?;
                continue;
            }
            let mut matched = false;
            for source in sources {
                let Some(capture) = spec.capture(source.name.as_bytes()) else {
                    continue;
                };
                matched = true;
                let excluded = self.specs.iter().any(|negative| {
                    negative.negative && negative.capture(source.name.as_bytes()).is_some()
                });
                if !excluded {
                    plan.add(Mapping {
                        source: Some(source.clone()),
                        destination: spec.destination(capture)?,
                        force: spec.force,
                    })?;
                }
            }
            if !matched && !spec.wildcard {
                missing.push(spec.source.clone());
            }
        }
        Ok((plan.mappings, missing))
    }

    /// Maps fetch advertisement tips, omitting peeled hints before duplicate-name checks.
    ///
    /// Advertised symbolic aliases (such as HEAD) retain their own name and advertised ID.
    /// Capability tokens are not followed. This never creates local symbolic references or selects
    /// a peeled tag target in place of its tag object.
    ///
    /// # Errors
    ///
    /// Returns [`MappingError::Direction`] for push specs, or any error from [`Self::map`].
    pub fn map_advertisement(
        &self,
        advertisement: &Advertisement,
    ) -> Result<Vec<Mapping>, MappingError> {
        if self.direction != Direction::Fetch {
            return Err(MappingError::Direction);
        }
        let sources: Vec<_> = advertisement
            .refs
            .iter()
            .filter(|reference| !reference.peeled)
            .map(|reference| RefSource {
                name: reference.name.clone(),
                id: reference.id,
            })
            .collect();
        self.map(&sources)
    }

    /// Advertisement mapping with missing exact selectors retained as reportable outcomes.
    pub fn map_advertisement_with_missing(
        &self,
        advertisement: &Advertisement,
    ) -> Result<(Vec<Mapping>, Vec<Vec<u8>>), MappingError> {
        if self.direction != Direction::Fetch {
            return Err(MappingError::Direction);
        }
        let sources: Vec<_> = advertisement
            .refs
            .iter()
            .filter(|reference| !reference.peeled)
            .map(|reference| RefSource {
                name: reference.name.clone(),
                id: reference.id,
            })
            .collect();
        self.map_with_missing(&sources)
    }
}

#[derive(Default)]
struct Plan {
    mappings: Vec<Mapping>,
    seen: HashSet<Mapping>,
    destinations: HashSet<RefName>,
}

impl Plan {
    fn add(&mut self, mapping: Mapping) -> Result<(), MappingError> {
        if self.seen.contains(&mapping) {
            return Ok(());
        }
        if let Some(destination) = &mapping.destination {
            if self.destinations.contains(destination) {
                return Err(MappingError::Collision(destination.clone()));
            }
            self.destinations.insert(destination.clone());
        }
        self.seen.insert(mapping.clone());
        self.mappings.push(mapping);
        Ok(())
    }
}

#[cfg(test)]
#[path = "refspec_tests.rs"]
mod tests;
