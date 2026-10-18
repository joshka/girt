//! Attachment policy and generation ordering used by bounded history queries.
use std::sync::Arc;

use super::layer::Layer;
use super::{CommitGraph, GraphError, bad};
use crate::Objects;

impl Objects {
    /// Attaches structurally checked auxiliary metadata for subsequent history queries.
    ///
    /// This explicitly trusts the index's object claims without decoding the named commits. Use
    /// [`CommitGraph::verify`] first when object verification is required. Missing index records
    /// use ordinary verified reads. Commit/parent query bounds and shallow boundaries still apply;
    /// per-object byte limits apply only to fallback reads. Typed object reads always use objects.
    /// Clones share the immutable index; storage refresh retains this index until replaced.
    ///
    /// # Errors
    ///
    /// Rejects a different hash format or cyclic indexed ancestry. Parsing has already rejected
    /// parent positions that name no record.
    pub fn with_commit_graph(mut self, graph: CommitGraph) -> Result<Self, GraphError> {
        self.attach_commit_graph(graph)?;
        Ok(self)
    }

    /// Removes the auxiliary index so history queries read commit objects directly.
    ///
    /// Useful when object claims must be checked during each query rather than trusted from an
    /// index. This does not alter repository configuration or other readers.
    pub fn without_commit_graph(mut self) -> Self {
        self.commit_graph = None;
        self
    }

    pub(crate) fn attach_commit_graph(&mut self, graph: CommitGraph) -> Result<(), GraphError> {
        if graph.format != self.object_format() {
            return Err(GraphError::HashFormat);
        }
        if let Err(reason) = graph.history {
            return Err(bad(reason));
        }
        self.commit_graph = Some(Arc::new(graph));
        Ok(())
    }
}

/// The history index of a parsed graph, or why its ancestry cannot be trusted.
pub(super) type Prepared = Result<HistoryIndex, &'static str>;

/// Derives parent positions and a checked generation order from structurally checked layers.
///
/// Parsing checked that every parent position names a record, so no identity lookup is needed.
pub(super) fn prepare<B: AsRef<[u8]>>(layers: &[Layer<B>]) -> Prepared {
    let count = layers
        .last()
        .map_or(0, |layer| layer.start() + layer.count());
    let mut levels = Vec::with_capacity(count);
    let mut dates = Vec::with_capacity(count);
    // Only records need dates: an empty layer without `GDA2` must not disable the order.
    let mut corrected = layers
        .iter()
        .all(|layer| layer.count() == 0 || layer.has_corrected_dates());
    let mut topological = true;
    for layer in layers {
        for index in 0..layer.count() {
            let level = layer.level(index);
            topological &= level != 0 && level < 0x3fffffff;
            levels.push(level);
            if corrected {
                dates.push(layer.corrected_date(index).unwrap_or(0));
            }
        }
    }

    let mut parent_starts = Vec::with_capacity(count + 1);
    let mut parents = Vec::with_capacity(count);
    parent_starts.push(0);
    for layer in layers {
        for index in 0..layer.count() {
            let child = layer.start() + index;
            let first = parents.len();
            layer.parents_into(index, &mut parents);
            for parent in &parents[first..] {
                let parent = *parent as usize;
                corrected = corrected && dates[parent] < dates[child];
                topological = topological && levels[parent] < levels[child];
            }
            parent_starts.push(u32::try_from(parents.len()).map_err(|_| "parent count")?);
        }
    }

    // A strictly decreasing value on every edge also proves acyclic ancestry.
    let (order, generations) = if corrected {
        (GenerationOrder::CorrectedDates, dates)
    } else if topological {
        let levels = levels.into_iter().map(u64::from).collect();
        (GenerationOrder::Topological, levels)
    } else {
        check_ancestry(&parent_starts, &parents)?;
        return Ok(HistoryIndex::default());
    };
    Ok(HistoryIndex {
        order,
        generations,
        parent_starts,
        parents,
    })
}

impl CommitGraph {
    /// Returns the history index when a generation order was checked.
    fn ordered(&self) -> Option<&HistoryIndex> {
        self.history
            .as_ref()
            .ok()
            .filter(|index| !matches!(index.order, GenerationOrder::Unavailable))
    }

    pub(crate) fn generation(&self, id: crate::ObjectId) -> Option<u64> {
        let index = self.ordered()?;
        Some(index.generations[self.locate(id)?])
    }

    /// Finds a record's position for position-indexed walks, when a generation order is checked.
    pub(crate) fn position(&self, id: crate::ObjectId) -> Option<u32> {
        self.ordered()?;
        u32::try_from(self.locate(id)?).ok()
    }

    /// Returns the number of record positions.
    pub(crate) fn positions(&self) -> usize {
        self.len()
    }

    /// Returns the commit at a position from [`Self::position`] or [`Self::parent_positions`].
    pub(crate) fn id_at(&self, position: u32) -> crate::ObjectId {
        let (layer, index) = self.layer_at(position as usize);
        layer.id(index)
    }

    /// Returns the checked generation at a position from [`Self::position`].
    pub(crate) fn generation_at(&self, position: u32) -> u64 {
        self.ordered()
            .map_or(0, |index| index.generations[position as usize])
    }

    /// Returns the parent positions, in stored order, of a position from [`Self::position`].
    pub(crate) fn parent_positions(&self, position: u32) -> &[u32] {
        let Some(index) = self.ordered() else {
            return &[];
        };
        let position = position as usize;
        let starts = &index.parent_starts;
        &index.parents[starts[position] as usize..starts[position + 1] as usize]
    }
}

/// Rejects a parent cycle when no generation order proves ancestry acyclic.
fn check_ancestry(parent_starts: &[u32], parents: &[u32]) -> Result<(), &'static str> {
    let count = parent_starts.len() - 1;
    let mut state = vec![0u8; count];
    let mut stack = Vec::new();
    for root in 0..count {
        if state[root] == 2 {
            continue;
        }
        stack.push((root, false));
        while let Some((index, exiting)) = stack.pop() {
            if exiting {
                state[index] = 2;
                continue;
            }
            if state[index] == 2 {
                continue;
            }
            if state[index] == 1 {
                return Err("cyclic indexed ancestry");
            }
            state[index] = 1;
            stack.push((index, true));
            let range = parent_starts[index] as usize..parent_starts[index + 1] as usize;
            for parent in parents[range].iter().rev() {
                stack.push((*parent as usize, false));
            }
        }
    }
    Ok(())
}

/// A locally checked strict ordering; exact generation equations remain object-verifier work.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum GenerationOrder {
    #[default]
    Unavailable,
    Topological,
    CorrectedDates,
}

/// Generations and parent positions indexed by record position, filled only for a checked order.
///
/// Walks index dense arrays with these positions instead of searching identities per edge.
#[derive(Clone, Debug, Default)]
pub(crate) struct HistoryIndex {
    order: GenerationOrder,
    generations: Vec<u64>,
    /// `parents[parent_starts[i]..parent_starts[i + 1]]` are the parents of position `i`.
    parent_starts: Vec<u32>,
    parents: Vec<u32>,
}
