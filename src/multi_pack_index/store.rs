//! Optional lookup hints; failure never makes the ordinary pack inventory disappear.

use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Limits, MultiPackIndex};
use crate::pack::FilePack;
use crate::{ObjectFormat, ObjectId};

#[derive(Clone, Debug)]
struct Layer {
    bytes: Arc<[u8]>,
    ids: Range<usize>,
    offsets: Range<usize>,
    large: Option<Range<usize>>,
    packs: Vec<usize>,
    format: ObjectFormat,
}

#[derive(Clone, Debug)]
pub(crate) struct Lookup(Vec<Layer>);

impl Lookup {
    pub(crate) fn load(
        format: ObjectFormat,
        directory: &Path,
        paths: &[PathBuf],
        max_bytes: usize,
        cancelled: &AtomicBool,
    ) -> Option<Self> {
        let ordinary = directory.join("multi-pack-index");
        if ordinary.exists() {
            return Some(Self(vec![Layer::load(
                format, directory, paths, &ordinary, max_bytes, cancelled,
            )?]));
        }
        let chain_directory = directory.join("multi-pack-index.d");
        let bytes = read_bounded(
            &chain_directory.join("multi-pack-index-chain"),
            256 * (format.digest_len() * 2 + 1),
            cancelled,
        )?;
        let chain = std::str::from_utf8(&bytes).ok()?;
        let mut layers = Vec::new();
        let mut remaining = max_bytes;
        for name in chain.lines() {
            if layers.len() >= 256
                || name.len() != format.digest_len() * 2
                || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return None;
            }
            let path = chain_directory.join(format!("multi-pack-index-{name}.midx"));
            let layer = Layer::load(format, directory, paths, &path, remaining, cancelled)?;
            let end = layer.bytes.len().checked_sub(format.digest_len())?;
            if ObjectId::from_bytes(format, &layer.bytes[end..])
                .ok()?
                .to_string()
                != name
            {
                return None;
            }
            remaining = remaining.checked_sub(layer.bytes.len())?;
            layers.push(layer);
        }
        if layers.is_empty() {
            return None;
        }
        Some(Self(layers))
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.0.iter().map(|layer| layer.bytes.len()).sum()
    }

    pub(crate) fn find(&self, id: ObjectId, packs: &[Arc<FilePack>]) -> Option<(usize, usize)> {
        self.0.iter().find_map(|layer| layer.find(id, packs))
    }
}

impl Layer {
    pub(crate) fn load(
        format: ObjectFormat,
        directory: &Path,
        paths: &[PathBuf],
        path: &Path,
        max_bytes: usize,
        cancelled: &AtomicBool,
    ) -> Option<Self> {
        let limits = Limits {
            max_bytes,
            ..Limits::default()
        };
        let bytes = read_bounded(path, limits.max_bytes, cancelled)?;
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        let parsed = MultiPackIndex::parse_with(&bytes, format, limits).ok()?;
        if parsed.bases != 0 {
            return None;
        }
        let packs = parsed
            .names
            .iter()
            .map(|name| {
                let expected = directory.join(name).with_extension("pack");
                paths.iter().position(|path| *path == expected)
            })
            .collect::<Option<Vec<_>>>()?;
        let range = |part: &[u8]| {
            let start = part.as_ptr() as usize - bytes.as_ptr() as usize;
            start..start + part.len()
        };
        let ids = range(parsed.ids);
        let offsets = range(parsed.offsets);
        let large = parsed.large.map(range);
        Some(Self {
            bytes: bytes.into(),
            ids,
            offsets,
            large,
            packs,
            format,
        })
    }

    pub(crate) fn find(&self, id: ObjectId, packs: &[Arc<FilePack>]) -> Option<(usize, usize)> {
        let parsed = MultiPackIndex {
            format: self.format,
            names: Vec::new(),
            ids: self.bytes.get(self.ids.clone())?,
            offsets: self.bytes.get(self.offsets.clone())?,
            large: self.large.as_ref().map(|range| &self.bytes[range.clone()]),
            bases: 0,
        };
        let location = parsed.find(id)?;
        let ordinal = *self.packs.get(location.pack as usize)?;
        let pack = packs.get(ordinal)?;
        // Keep all existing index checks. A stale offset or identity is only a failed hint.
        let position = pack.find(id).ok()??;
        if pack.offset(position).ok()? as u64 != location.offset {
            return None;
        }
        Some((ordinal, position))
    }
}

fn read_bounded(path: &Path, max_bytes: usize, cancelled: &AtomicBool) -> Option<Vec<u8>> {
    if cancelled.load(Ordering::Relaxed) {
        return None;
    }
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > max_bytes as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take((max_bytes as u64).checked_add(1)?)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    Some(bytes)
}
