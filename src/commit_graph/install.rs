//! Coordinated, additive publication of auxiliary graph files.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{Chunks, CommitGraph, GraphError, GraphLimits, bad, id, word};

/// On-disk graph publication strategy.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum GraphWriteMode {
    /// Replace the standalone file; retain any previous split-chain files.
    Full,
    /// Append a split layer, migrating an existing standalone file into the chain.
    Incremental,
}

/// Result of publishing a standalone graph or one appended chain layer.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GraphInstallReport {
    /// Checksum naming the newly encoded file.
    pub checksum: crate::ObjectId,
    /// Commits emitted in that file, excluding retained bases.
    pub commits_written: usize,
    /// Number of layers visible through the resulting index (one for standalone output).
    pub layers: usize,
}

/// Publication failure with explicit reader-visible partial-effect information.
#[derive(Debug, thiserror::Error)]
#[error("cannot {operation} commit-graph")]
#[non_exhaustive]
pub struct GraphInstallError {
    /// Whether a standalone file or chain manifest was already published by this operation.
    /// A migrated standalone may still shadow the new manifest after a later removal failure.
    pub index_published: bool,
    /// Failed publication phase.
    pub operation: &'static str,
    /// Underlying filesystem, graph or encoding failure.
    #[source]
    pub source: crate::Error,
}

impl CommitGraph {
    /// Publishes this graph under standalone and split-chain writer locks.
    ///
    /// Full mode atomically replaces `info/commit-graph`. Incremental mode retains base files,
    /// publishes complete checksum-named layers before the manifest, and removes a migrated
    /// standalone file only after manifest publication. Unreferenced layer files can remain after
    /// failure. No objects, packs or references are changed. Output omits new Bloom filters.
    ///
    /// Publication synchronizes file contents but does not promise a power-loss transaction or
    /// prune old layers. The selected object directory must use this graph's hash format.
    /// Loaded readers retain immutable bytes. Callers must exclude hostile
    /// directory/lock replacement and object pruning during construction and publication. Existing
    /// cooperating writer locks are never stolen. Transient locks participate in caller cleanup;
    /// Drop cleanup after an operation failure is best effort.
    ///
    /// # Errors
    ///
    /// Reports locked/inaccessible paths, observed lock replacement, invalid bases, conflicting
    /// metadata or encoding limits. Inspect `index_published` before recovery; a failure after
    /// publication does not roll back the new index. Reopen to inspect current state before retry.
    pub fn install(
        &self,
        object_dir: &Path,
        mode: GraphWriteMode,
    ) -> Result<GraphInstallReport, GraphInstallError> {
        let mut operation = "prepare publication";
        let mut published = false;
        let result = (|| -> crate::Result<GraphInstallReport> {
            let _lock_operation = crate::locks::operation();
            let object_dir = fs::canonicalize(object_dir)?;
            let info = object_dir.join("info");
            let directory = info.join("commit-graphs");
            fs::create_dir_all(&directory)?;
            operation = "acquire standalone writer lock";
            let standalone_lock = GraphLock::acquire(info.join("commit-graph.lock"))?;
            operation = "acquire chain writer lock";
            let chain_lock = GraphLock::acquire(directory.join("commit-graph-chain.lock"))?;
            let standalone = info.join("commit-graph");
            let manifest = directory.join("commit-graph-chain");
            operation = "read base layers";
            let bases = match mode {
                GraphWriteMode::Full => BaseFiles::default(),
                GraphWriteMode::Incremental => read_bases(&object_dir, self.format)?,
            };
            let refs: Vec<_> = bases.layers.iter().map(Vec::as_slice).collect();
            operation = "encode graph";
            let bytes = self.encode_incremental(&refs)?;
            let checksum = id(
                self.format,
                &bytes[bytes.len() - self.format.digest_len()..],
            )?;
            let chunks = Chunks::parse(&bytes, self.format, GraphLimits::default())?;
            let commits_written = word(chunks.required(b"OIDF")?, 1020)? as usize;
            standalone_lock.check()?;
            chain_lock.check()?;
            let layers = match mode {
                GraphWriteMode::Full => {
                    operation = "publish standalone file";
                    replace_file(&standalone, &bytes)?;
                    published = true;
                    1
                }
                GraphWriteMode::Incremental => {
                    operation = "publish layer files";
                    let mut names = String::new();
                    for base in &bases.layers {
                        let checksum =
                            id(self.format, &base[base.len() - self.format.digest_len()..])?;
                        publish_layer(&directory, checksum, base)?;
                        names.push_str(&format!("{checksum}\n"));
                    }
                    publish_layer(&directory, checksum, &bytes)?;
                    names.push_str(&format!("{checksum}\n"));
                    standalone_lock.check()?;
                    chain_lock.check()?;
                    operation = "publish chain manifest";
                    replace_file(&manifest, names.as_bytes())?;
                    published = true;
                    if bases.standalone {
                        let original = &bases.layers[0];
                        operation = "retire migrated standalone file";
                        if super::store::read_optional(
                            &standalone,
                            GraphLimits::default().max_bytes,
                        )?
                        .as_deref()
                            != Some(original.as_slice())
                        {
                            return Err(bad("standalone changed during publication").into());
                        }
                        standalone_lock.check()?;
                        fs::remove_file(&standalone)?;
                    }
                    bases.layers.len() + 1
                }
            };
            operation = "release chain writer lock";
            chain_lock.release()?;
            operation = "release standalone writer lock";
            standalone_lock.release()?;
            Ok(GraphInstallReport {
                checksum,
                commits_written,
                layers,
            })
        })();
        result.map_err(|source| GraphInstallError {
            index_published: published,
            operation,
            source,
        })
    }
}

#[derive(Default)]
struct BaseFiles {
    layers: Vec<Vec<u8>>,
    standalone: bool,
}

fn read_bases(object_dir: &Path, format: crate::ObjectFormat) -> crate::Result<BaseFiles> {
    let limits = GraphLimits::default();
    if let Some(bytes) =
        super::store::read_optional(&object_dir.join("info/commit-graph"), limits.max_bytes)?
    {
        CommitGraph::parse(&bytes, format)?;
        return Ok(BaseFiles {
            layers: vec![bytes],
            standalone: true,
        });
    }
    let directory = object_dir.join("info/commit-graphs");
    let Some(manifest) = super::store::read_optional(
        &directory.join("commit-graph-chain"),
        256 * (format.digest_len() * 2 + 1),
    )?
    else {
        return Ok(BaseFiles::default());
    };
    let text = std::str::from_utf8(&manifest).map_err(|_| bad("chain encoding"))?;
    let mut remaining = limits.max_bytes;
    let mut bases = Vec::new();
    for name in text.lines() {
        if bases.len() == 255
            || name.len() != format.digest_len() * 2
            || !name.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(bad("chain identity").into());
        }
        let expected: crate::ObjectId = name.parse().map_err(|_| bad("chain identity"))?;
        let bytes =
            super::store::read_optional(&directory.join(format!("graph-{name}.graph")), remaining)?
                .ok_or(bad("missing chain layer"))?;
        remaining -= bytes.len();
        let at = bytes
            .len()
            .checked_sub(format.digest_len())
            .ok_or(bad("layer trailer"))?;
        if &bytes[at..] != expected.as_bytes() {
            return Err(bad("chain filename identity").into());
        }
        bases.push(bytes);
    }
    if bases.is_empty() {
        return Err(bad("empty chain").into());
    }
    Ok(BaseFiles {
        layers: bases,
        standalone: false,
    })
}

fn publish_layer(directory: &Path, checksum: crate::ObjectId, bytes: &[u8]) -> crate::Result<()> {
    let path = directory.join(format!("graph-{checksum}.graph"));
    if let Some(existing) = super::store::read_optional(&path, GraphLimits::default().max_bytes)? {
        if existing != bytes {
            return Err(GraphError::Checksum.into());
        }
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    Ok(())
}

fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(io::Error::other(
            "commit-graph destination is not a regular file",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("commit-graph destination has no parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

struct GraphLock {
    path: PathBuf,
    file: Option<crate::locks::SharedFile>,
    registration: crate::locks::Guard,
}

impl GraphLock {
    fn acquire(path: PathBuf) -> io::Result<Self> {
        let _operation = crate::locks::operation();
        let file =
            crate::locks::SharedFile::from(crate::locks::unreplaceable_lock_options().open(&path)?);
        let identity = crate::locks::capture_identity(&path, &file)?;
        let registration = crate::locks::Guard::new(&path, &file, identity);
        Ok(Self {
            path,
            file: Some(file),
            registration,
        })
    }
    fn check(&self) -> io::Result<()> {
        self.registration.check_path()
    }
    fn release(mut self) -> io::Result<()> {
        let _operation = crate::locks::operation();
        self.check()?;
        drop(self.file.take());
        fs::remove_file(&self.path)
    }
}

impl Drop for GraphLock {
    fn drop(&mut self) {
        let _operation = crate::locks::operation();
        if self.file.is_some() && self.check().is_ok() {
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl crate::Repository {
    /// Builds verified metadata for `roots` and publishes it in this repository's object store.
    ///
    /// Uses default history construction bounds. Roots are explicit; this does not select
    /// references, change maintenance policy or honor `gc.writeCommitGraph`. Reading is controlled
    /// separately by `core.commitGraph`. New artifacts omit Bloom filters. Use
    /// [`CommitGraph::from_objects`] and [`CommitGraph::install`] separately to inspect bytes or
    /// retain object storage between construction and publication.
    ///
    /// Given a repository and an explicit commit ID, publish and reopen its indexed history:
    ///
    /// ```
    /// use girt::{Repository, ObjectFormat, InitKind, commit_graph::GraphWriteMode};
    /// # let directory = tempfile::tempdir()?;
    /// # let repository = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
    /// # let commit = include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/doctest/commit.rs"));
    /// # let tip = commit(&repository, &[], "root")?;
    /// let report = repository.write_commit_graph(&[tip], GraphWriteMode::Full)?;
    /// assert_eq!(report.commits_written, 1);
    /// assert!(repository.commit_graph()?.unwrap().get(tip).is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns construction, storage or publication failures. Construction failures have
    /// `index_published == false`; later failures retain the partial-effect contract of
    /// [`CommitGraph::install`].
    pub fn write_commit_graph(
        &self,
        roots: &[crate::ObjectId],
        mode: GraphWriteMode,
    ) -> Result<GraphInstallReport, GraphInstallError> {
        self.write_commit_graph_with(roots, mode, crate::HistoryLimits::default())
    }

    /// Builds and publishes a graph with explicit history-construction bounds.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::write_commit_graph`], including construction limits.
    /// Object-store opening uses default pack/alternate bounds; applications needing different
    /// opening bounds can construct and install artifacts separately.
    pub fn write_commit_graph_with(
        &self,
        roots: &[crate::ObjectId],
        mode: GraphWriteMode,
        limits: crate::HistoryLimits,
    ) -> Result<GraphInstallReport, GraphInstallError> {
        let build = || -> crate::Result<CommitGraph> {
            let objects = self.objects()?;
            Ok(CommitGraph::from_objects_with(&objects, roots, limits)?)
        };
        let graph = build().map_err(|source| GraphInstallError {
            index_published: false,
            operation: "build metadata",
            source,
        })?;
        graph.install(self.object_dir(), mode)
    }
}
