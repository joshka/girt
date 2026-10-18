//! Object overlay shared by rename normalization and virtual-base computation.
use std::collections::BTreeMap;

use super::TreeError;
use crate::{EntryMode, ObjectId, Objects, Tree, TreeEntry, TreeValue};

#[derive(Default)]
pub(super) struct Memory {
    pub trees: BTreeMap<ObjectId, Tree>,
    pub order: Vec<ObjectId>,
    pub blobs: BTreeMap<ObjectId, Vec<u8>>,
}

impl Memory {
    pub fn read_tree(&self, objects: &Objects, id: ObjectId) -> Result<Tree, TreeError> {
        match self.trees.get(&id) {
            Some(tree) => Ok(tree.clone()),
            None => Ok(objects.read_tree(id)?),
        }
    }

    pub fn read_tree_with(
        &self,
        objects: &Objects,
        id: ObjectId,
        limits: crate::ReadLimits,
    ) -> Result<Tree, TreeError> {
        match self.trees.get(&id) {
            Some(tree) => {
                if tree.encode().len() > limits.max_object_bytes {
                    return Err(TreeError::Limit("overlay tree bytes"));
                }
                Ok(tree.clone())
            }
            None => Ok(objects.read_tree_with(id, limits)?),
        }
    }

    pub fn read_blob(&self, objects: &Objects, id: ObjectId) -> Result<Vec<u8>, TreeError> {
        match self.blobs.get(&id) {
            Some(bytes) => Ok(bytes.clone()),
            None => Ok(objects.read_blob(id)?),
        }
    }

    pub fn build(
        &mut self,
        objects: &Objects,
        leaves: BTreeMap<Vec<u8>, TreeValue>,
    ) -> Result<ObjectId, TreeError> {
        let mut files = Vec::new();
        let mut directories: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, TreeValue>> = BTreeMap::new();
        for (path, value) in leaves {
            if let Some(slash) = path.iter().position(|b| *b == b'/') {
                directories
                    .entry(path[..slash].to_vec())
                    .or_default()
                    .insert(path[slash + 1..].to_vec(), value);
            } else {
                files.push(TreeEntry {
                    name: path,
                    mode: value.mode,
                    id: value.id,
                });
            }
        }
        for (name, children) in directories {
            let id = self.build(objects, children)?;
            files.push(TreeEntry {
                name,
                mode: EntryMode::Tree,
                id,
            });
        }
        let tree = Tree::new(objects.object_format(), files)?;
        let id = tree.id();
        if !self.trees.contains_key(&id) {
            self.order.push(id);
        }
        self.trees.insert(id, tree);
        Ok(id)
    }
}
