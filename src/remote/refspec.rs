use std::collections::HashSet;

use crate::ObjectId;
use crate::fetch::Advertisement;
use crate::refs::RefName;

/// Which side supplies source names and which refspec grammar applies.
///
/// The same text means different things in each direction. A fetch refspec maps remote names to
/// local names and may exclude sources with `^`; a push refspec maps local names to remote names
/// and may delete with an empty source (`:refs/heads/old`).
///
/// # Examples
///
/// ```
/// use girt::remote::{Direction, Refspec};
///
/// // Deletion is push-only; exclusion is fetch-only.
/// assert!(Refspec::parse(Direction::Push, b":refs/heads/old").is_ok());
/// assert!(Refspec::parse(Direction::Fetch, b":refs/heads/old").is_err());
/// assert!(Refspec::parse(Direction::Fetch, b"^refs/heads/wip/*").is_ok());
/// assert!(Refspec::parse(Direction::Push, b"^refs/heads/wip/*").is_err());
/// ```
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// For configuration that uses syntax outside this subset, see
/// [`ConfiguredRefspec`](super::ConfiguredRefspec), which describes it without mapping it.
///
/// # Examples
///
/// ```
/// use girt::remote::{Direction, Refspec, RefspecError};
///
/// let spec = Refspec::parse(Direction::Fetch, b"+refs/heads/*:refs/remotes/origin/*")?;
/// assert!(spec.force());
/// assert!(!spec.is_negative());
/// assert_eq!(spec.as_bytes(), b"+refs/heads/*:refs/remotes/origin/*");
///
/// // Shorthand needs name resolution, which this type never performs.
/// assert!(matches!(
///     Refspec::parse(Direction::Fetch, b"main:refs/remotes/origin/main"),
///     Err(RefspecError::Unsupported(_)),
/// ));
/// // Both sides of a wildcard mapping need exactly one `*`.
/// assert!(matches!(
///     Refspec::parse(Direction::Fetch, b"refs/heads/*:refs/remotes/origin/main"),
///     Err(RefspecError::Syntax(_)),
/// ));
/// # Ok::<(), RefspecError>(())
/// ```
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
///
/// # Examples
///
/// ```
/// use girt::remote::{Direction, Refspecs};
///
/// let error =
///     Refspecs::parse(Direction::Fetch, ["refs/heads/main", "refs/heads/a*b*"]).unwrap_err();
/// assert_eq!(error.index, 1);
/// ```
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[error("refspec at index {index}")]
#[non_exhaustive]
pub struct RefspecsError {
    /// Zero-based occurrence index.
    pub index: usize,
    /// Syntax or unsupported-form diagnostic.
    #[source]
    pub source: RefspecError,
}

/// Invalid syntax or a recognized form outside the supported full-name subset.
///
/// [`Syntax`](Self::Syntax) and [`Name`](Self::Name) mean the refspec is malformed; ask for a
/// corrected one. [`Unsupported`](Self::Unsupported) means it is valid Git syntax that needs name
/// resolution, such as shorthand like `main`, which this strict parser never performs. Rewrite it
/// with full names (`refs/heads/main:refs/remotes/origin/main`), or describe it with
/// [`ConfiguredRefspec`](super::ConfiguredRefspec). [`Refspec`](super::Refspec) has examples of
/// each.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
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

/// A reference name and the object ID it points to, as input to [`Refspecs::map`].
///
/// For a fetch, sources are the remote's advertised references; [`Refspecs::map_advertisement`]
/// builds them from an [`Advertisement`]. For a push, they are local references the caller has
/// already resolved.
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

/// Whether a refspec requests a forced mapping (`+`); this does not authorize publication.
///
/// A forced fetch mapping may move a destination to a commit that does not descend from its
/// current value. Whether that is allowed is decided when references are published:
/// [`crate::fetch::FetchRequest::prepare`] requires the caller to name each destination it
/// authorizes for forced updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ForceIntent {
    /// No syntactic force request.
    Normal,
    /// The refspec carries `+`; the caller must separately authorize force.
    Requested,
}

/// One source matched to its destination by [`Refspecs::map`].
///
/// A mapping is a proposal, not a reference update: nothing has been checked against storage.
/// Callers that publish it must still check expected old values, fast-forwarding, object kinds and
/// force authorization; [`crate::fetch::FetchRequest`] and [`crate::push::PushCommand`] do so.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct Mapping {
    /// Selected source and ID; `None` means push deletion.
    pub source: Option<RefSource>,
    /// Full destination; `None` means fetch selection without a tracking update.
    pub destination: Option<RefName>,
    /// Syntactic `+` intent. Callers separately authorize force and validate expected old values,
    /// ancestry, object kinds, namespace restrictions and storage conflicts.
    pub force: ForceIntent,
}

impl Mapping {
    /// Creates a mapping with explicit source, destination and force policy.
    pub fn new(
        source: Option<RefSource>,
        destination: Option<RefName>,
        force: ForceIntent,
    ) -> Self {
        Self {
            source,
            destination,
            force,
        }
    }

    /// Sets [`Self::source`].
    #[must_use]
    pub fn with_source(mut self, source: Option<RefSource>) -> Self {
        self.source = source;
        self
    }

    /// Sets [`Self::destination`].
    #[must_use]
    pub fn with_destination(mut self, destination: Option<RefName>) -> Self {
        self.destination = destination;
        self
    }

    /// Sets [`Self::force`].
    #[must_use]
    pub fn with_force(mut self, force: ForceIntent) -> Self {
        self.force = force;
        self
    }
}

/// An ordered list of refspecs for one direction, such as a remote's `fetch` values.
///
/// Parse with [`Self::parse`], then map reference names with [`Self::map`] or a fetch
/// advertisement with [`Self::map_advertisement`]. Mapping is pure: it reads no repository and
/// contacts no server, so it can be used to preview a transfer or to test a configuration.
///
/// # Examples
///
/// ```
/// use girt::refs::RefName;
/// use girt::remote::{Direction, ForceIntent, RefSource, Refspecs};
/// use girt::{ObjectFormat, ObjectKind};
///
/// let specs = Refspecs::parse(
///     Direction::Fetch,
///     ["+refs/heads/*:refs/remotes/origin/*", "^refs/heads/wip/*"],
/// )?;
/// let id = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"example");
/// let sources = [
///     RefSource {
///         name: RefName::new("refs/heads/main")?,
///         id,
///     },
///     RefSource {
///         name: RefName::new("refs/heads/wip/spike")?,
///         id,
///     },
///     RefSource {
///         name: RefName::new("refs/tags/v1.0")?,
///         id,
///     },
/// ];
/// let mappings = specs.map(&sources)?;
///
/// // `wip/spike` is excluded and the tag matches no refspec.
/// assert_eq!(mappings.len(), 1);
/// assert_eq!(
///     mappings[0].destination,
///     Some(RefName::new("refs/remotes/origin/main")?)
/// );
/// assert_eq!(mappings[0].force, ForceIntent::Requested);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct Refspecs {
    direction: Direction,
    specs: Vec<Refspec>,
}

/// Mapping failed; no partial plan or side effects are produced.
///
/// # Examples
///
/// Two sources mapped to one destination are refused rather than letting one win silently:
///
/// ```
/// use girt::refs::RefName;
/// use girt::remote::{Direction, MappingError, RefSource, Refspecs};
/// use girt::{ObjectFormat, ObjectKind};
///
/// let specs = Refspecs::parse(
///     Direction::Fetch,
///     [
///         "refs/heads/main:refs/remotes/origin/main",
///         "refs/heads/master:refs/remotes/origin/main",
///     ],
/// )?;
/// let id = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"example");
/// let sources = [
///     RefSource {
///         name: RefName::new("refs/heads/main")?,
///         id,
///     },
///     RefSource {
///         name: RefName::new("refs/heads/master")?,
///         id,
///     },
/// ];
/// assert!(matches!(
///     specs.map(&sources),
///     Err(MappingError::Collision(_))
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
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
    /// # Examples
    ///
    /// ```
    /// use girt::refs::RefName;
    /// use girt::remote::{Direction, RefSource, Refspecs};
    /// use girt::{ObjectFormat, ObjectKind};
    ///
    /// let specs = Refspecs::parse(
    ///     Direction::Fetch,
    ///     [
    ///         "refs/heads/main:refs/remotes/origin/main",
    ///         "refs/heads/gone:refs/remotes/origin/gone",
    ///     ],
    /// )?;
    /// let id = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"example");
    /// let sources = [RefSource {
    ///     name: RefName::new("refs/heads/main")?,
    ///     id,
    /// }];
    ///
    /// // `map` fails on the missing source; this reports it and maps the rest.
    /// assert!(specs.map(&sources).is_err());
    /// let (mappings, missing) = specs.map_with_missing(&sources)?;
    /// assert_eq!(mappings.len(), 1);
    /// assert_eq!(missing, [b"refs/heads/gone".to_vec()]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
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
                    force: if spec.force {
                        ForceIntent::Requested
                    } else {
                        ForceIntent::Normal
                    },
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
                        force: if spec.force {
                            ForceIntent::Requested
                        } else {
                            ForceIntent::Normal
                        },
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
            .filter(|reference| reference.kind == crate::fetch::AdvertisementKind::Tip)
            .map(|reference| RefSource {
                name: reference.name.clone(),
                id: reference.id,
            })
            .collect();
        self.map(&sources)
    }

    /// Advertisement mapping with missing exact selectors retained as reportable outcomes.
    ///
    /// # Errors
    ///
    /// Returns [`MappingError::Direction`] for push specs, or duplicate-source, zero-ID, invalid
    /// destination and collision errors from [`Self::map_with_missing`]. Missing exact selectors
    /// are returned in the second tuple element instead of failing the mapping.
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
            .filter(|reference| reference.kind == crate::fetch::AdvertisementKind::Tip)
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
