//! File loading and optional object-store verification.
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use super::{CommitGraph, GraphError, GraphLimits, bad};
use crate::{ObjectFormat, ObjectId, Objects, ReadLimits, Repository};

/// Failure to load an auxiliary index or validate its object claims.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphLoadError {
    /// A graph file could not be read.
    #[error("reading commit-graph")]
    Io(#[from] io::Error),
    /// The graph bytes violate their format or the selected bounds.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// `core.commitGraph` is not a Git boolean.
    #[error(transparent)]
    Config(#[from] crate::config::InvalidValue),
}

/// Failure to establish that indexed records agree with the object database.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphVerifyError {
    /// A claimed commit could not be read and parsed.
    #[error("verifying indexed commit {id}")]
    Read {
        /// Indexed commit being checked.
        id: ObjectId,
        /// Object-store read or parse failure.
        #[source]
        source: crate::ReadCommitError,
    },
    /// A claimed tree, parent sequence or timestamp differs from the stored object.
    #[error("commit-graph metadata differs from commit {0}")]
    Mismatch(ObjectId),
}

impl CommitGraph {
    /// Loads a standalone graph or split chain from an object directory with default bounds.
    ///
    /// Prefers `info/commit-graph` when present. Returns `None` when neither it nor the chain
    /// manifest exists. Does not inspect repository configuration or verify object claims.
    ///
    /// # Errors
    ///
    /// Reports I/O, malformed files, inconsistent chain names/bases, or resource-limit failures.
    /// A concurrent chain replacement can fail this load; retain an earlier index or use objects.
    pub fn open(object_dir: &Path, format: ObjectFormat) -> Result<Option<Self>, GraphLoadError> {
        Self::open_with(object_dir, format, GraphLimits::default())
    }

    /// Loads graph files with aggregate byte, commit and parent limits.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::open`]. Files are bounded while reading, including
    /// files that grow after opening. The manifest is bounded separately to 256 hash lines.
    pub fn open_with(
        object_dir: &Path,
        format: ObjectFormat,
        limits: GraphLimits,
    ) -> Result<Option<Self>, GraphLoadError> {
        let info = object_dir.join("info");
        if let Some(bytes) = read_optional(&info.join("commit-graph"), limits.max_bytes)? {
            return Ok(Some(Self::parse_owned(bytes, format, limits)?));
        }
        let directory = info.join("commit-graphs");
        let manifest_limit = 256 * (format.digest_len() * 2 + 1);
        let Some(manifest) = read_optional(&directory.join("commit-graph-chain"), manifest_limit)?
        else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&manifest).map_err(|_| bad("chain encoding"))?;
        let mut layers = Vec::new();
        let mut remaining = limits.max_bytes;
        for line in text.lines() {
            if layers.len() == 256
                || line.len() != format.digest_len() * 2
                || !line.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(bad("chain identity").into());
            }
            let expected: ObjectId = line.parse().map_err(|_| bad("chain identity"))?;
            let bytes = read_optional(&directory.join(format!("graph-{line}.graph")), remaining)?
                .ok_or_else(|| bad("missing chain layer"))?;
            remaining -= bytes.len();
            let trailer = bytes
                .len()
                .checked_sub(format.digest_len())
                .and_then(|at| bytes.get(at..))
                .ok_or(bad("chain layer trailer"))?;
            if trailer != expected.as_bytes() {
                return Err(bad("chain filename identity").into());
            }
            layers.push(bytes);
        }
        if layers.is_empty() {
            return Err(bad("empty chain").into());
        }
        Ok(Some(Self::parse_chain_owned(layers, format, limits)?))
    }

    /// Checks each indexed tree, ordered parent list and committer date against verified objects.
    ///
    /// Does not require complete repository coverage. Reads each indexed commit once using
    /// [`ReadLimits::default`]; does not check generation equations.
    /// Writes nothing and retains the graph after failure.
    ///
    /// # Errors
    ///
    /// Returns an object-read error for missing, corrupt, malformed or wrong-kind commits, or a
    /// mismatch for different metadata or an unavailable/unrepresentable committer timestamp.
    pub fn verify(&self, objects: &Objects) -> Result<(), GraphVerifyError> {
        self.verify_with(objects, ReadLimits::default())
    }

    /// Verifies indexed object claims with explicit per-object read limits.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::verify`], including per-object resource failures.
    pub fn verify_with(
        &self,
        objects: &Objects,
        limits: ReadLimits,
    ) -> Result<(), GraphVerifyError> {
        for record in self.commits() {
            let commit = objects
                .read_commit_with(record.id, limits)
                .map_err(|source| GraphVerifyError::Read {
                    id: record.id,
                    source,
                })?;
            let date = commit
                .committer()
                .ok()
                .flatten()
                .and_then(|person| person.date().ok())
                .and_then(|date| u64::try_from(date.seconds).ok());
            if commit.tree() != record.tree
                || commit.parents() != record.parents
                || date != Some(record.commit_time)
            {
                return Err(GraphVerifyError::Mismatch(record.id));
            }
        }
        Ok(())
    }
}

impl Repository {
    /// Loads optional commit-graph metadata under this handle's `core.commitGraph` setting.
    ///
    /// An absent setting enables loading. `false` returns `None` without opening graph files.
    /// Structural validation does not establish agreement with objects; use [`CommitGraph::verify`]
    /// when required by the caller's trust policy. Errors permit callers to fall back to objects.
    ///
    /// # Errors
    ///
    /// Reports invalid configuration, file I/O or malformed/incomplete auxiliary indexes.
    pub fn commit_graph(&self) -> Result<Option<CommitGraph>, GraphLoadError> {
        if self.config().boolean("core", None, "commitGraph")? == Some(false) {
            return Ok(None);
        }
        CommitGraph::open(self.object_dir(), self.object_format())
    }
}

pub(super) fn read_optional(
    path: &Path,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, GraphLoadError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(GraphError::Limit("bytes").into());
    }
    Ok(Some(bytes))
}
