//! Compact graph data retained from the identity-checked retention walk.
use std::ops::Range;

use crate::{ObjectId, ObjectKind};

#[derive(Debug)]
pub(crate) struct VerifiedGraph {
    pub(crate) nodes: Vec<Node>,
    pub(crate) targets: Vec<u32>,
}

#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) id: ObjectId,
    pub(crate) kind: ObjectKind,
    pub(crate) edges: Range<usize>,
    kind_set: bool,
}

pub(super) struct Builder {
    nodes: Vec<Node>,
    targets: Vec<u32>,
    max_bytes: usize,
}

impl Builder {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            nodes: Vec::new(),
            targets: Vec::new(),
            max_bytes,
        }
    }

    pub(super) fn add_node(&mut self, id: ObjectId) -> Option<u32> {
        let position = u32::try_from(self.nodes.len()).ok()?;
        if self.nodes.len() == self.nodes.capacity() {
            self.nodes.try_reserve_exact(1).ok()?;
            if !self.within_budget(self.nodes.capacity(), self.targets.capacity()) {
                return None;
            }
        }
        self.nodes.push(Node {
            id,
            kind: ObjectKind::Blob,
            edges: 0..0,
            kind_set: false,
        });
        Some(position)
    }

    pub(super) fn set_kind(&mut self, position: u32, kind: ObjectKind) {
        let node = &mut self.nodes[position as usize];
        node.kind = kind;
        node.kind_set = true;
    }

    pub(super) fn begin_edges(&self) -> usize {
        self.targets.len()
    }

    pub(super) fn push_edge(&mut self, target: u32) -> bool {
        if self.targets.len() == self.targets.capacity()
            && (self.targets.try_reserve_exact(1).is_err()
                || !self.within_budget(self.nodes.capacity(), self.targets.capacity()))
        {
            return false;
        }
        self.targets.push(target);
        true
    }

    pub(super) fn finish_edges(&mut self, position: u32, first: usize) {
        self.nodes[position as usize].edges = first..self.targets.len();
    }

    pub(super) fn finish(self) -> Option<VerifiedGraph> {
        self.nodes.iter().all(|node| node.kind_set).then_some(())?;
        Some(VerifiedGraph {
            nodes: self.nodes,
            targets: self.targets,
        })
    }

    fn within_budget(&self, nodes: usize, targets: usize) -> bool {
        let node_bytes = nodes.checked_mul(std::mem::size_of::<Node>());
        let edge_bytes = targets.checked_mul(std::mem::size_of::<u32>());
        node_bytes
            .and_then(|nodes| edge_bytes.and_then(|edges| nodes.checked_add(edges)))
            .is_some_and(|bytes| bytes <= self.max_bytes)
    }
}
