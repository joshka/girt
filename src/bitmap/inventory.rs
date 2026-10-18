//! Bind optional metadata to pinned, verified primary-pack inventories before using its bits.
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::{Bitmap, BitmapIndex, Error, Limits, QueryLimits, ReverseIndex, check, zero_words};
use crate::{ObjectId, ObjectKind, Objects};

pub(super) struct Bound {
    bitmap: BitmapIndex,
    ids: Vec<ObjectId>,
    order: Vec<usize>,
    inverse: Vec<usize>,
}
impl Bound {
    fn new(
        bitmap: BitmapIndex,
        ids: Vec<ObjectId>,
        order: Vec<usize>,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        // One inverse slot per already-budgeted inventory object; construction is linear once,
        // and selected-commit membership checks are constant time for every subsequent tip.
        let mut inverse = Vec::new();
        inverse
            .try_reserve_exact(ids.len())
            .map_err(|_| Error::Limit("inverse order"))?;
        inverse.resize(ids.len(), 0);
        for (bit, &position) in order.iter().enumerate() {
            check(cancel)?;
            *inverse
                .get_mut(position)
                .ok_or(super::bad("inverse position"))? = bit;
        }
        Ok(Self {
            bitmap,
            ids,
            order,
            inverse,
        })
    }

    pub(super) fn object(&self, position: usize) -> Result<(ObjectId, ObjectKind), Error> {
        let &index = self
            .order
            .get(position)
            .ok_or(super::bad("inventory position"))?;
        let &id = self
            .ids
            .get(index)
            .ok_or(super::bad("inventory identity"))?;
        let kind = [
            ObjectKind::Commit,
            ObjectKind::Tree,
            ObjectKind::Blob,
            ObjectKind::Tag,
        ]
        .into_iter()
        .find(|&kind| self.bitmap.type_bitmap(kind).contains(position))
        .ok_or(super::bad("inventory kind"))?;
        Ok((id, kind))
    }

    /// The common serving case: both sides fit one inventory, so union and subtraction stay
    /// in bitmap space until the final response identities are needed.
    pub(super) fn difference(
        &self,
        include: &[ObjectId],
        exclude: &[ObjectId],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Option<(Bitmap, usize)>, Error> {
        let mut unions = Vec::new();
        for tips in [include, exclude] {
            let mut words = match zero_words(self.ids.len(), budget) {
                Ok(words) => words,
                Err(_) => return Ok(None),
            };
            for &tip in tips {
                check(cancel)?;
                let Some(bitmap) = self.reachable(tip, budget, cancel)? else {
                    return Ok(None);
                };
                if super::charge(budget, words.len()).is_err() {
                    return Ok(None);
                }
                for (word, &other) in words.iter_mut().zip(bitmap.words.iter()) {
                    check(cancel)?;
                    *word |= other;
                }
            }
            unions.push(words);
        }
        let mut included = unions.remove(0);
        let excluded = unions.remove(0);
        let visits = included
            .iter()
            .chain(&excluded)
            .try_fold(0_usize, |count, word| {
                count.checked_add(word.count_ones() as usize)
            })
            .ok_or(Error::Limit("query objects"))?;
        if super::charge(budget, included.len()).is_err() {
            return Ok(None);
        }
        for (word, other) in included.iter_mut().zip(excluded) {
            check(cancel)?;
            *word &= !other;
        }
        Ok(Some((
            Bitmap {
                words: included.into(),
                objects: self.ids.len(),
            },
            visits,
        )))
    }

    pub(super) fn reachable(
        &self,
        id: ObjectId,
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Bitmap>, Error> {
        let Ok(position) = self.ids.binary_search(&id) else {
            return Ok(None);
        };
        let Ok(rank) = self
            .bitmap
            .sorted
            .binary_search_by_key(&(position as u32), |&index| {
                self.bitmap.entries[index].position
            })
        else {
            return Ok(None);
        };
        let mut index = self.bitmap.sorted[rank];
        let mut work = *budget;
        let mut decode = || -> Result<Bitmap, Error> {
            let mut output = zero_words(self.ids.len(), &mut work)?;
            loop {
                check(cancel)?;
                let entry = &self.bitmap.entries[index];
                entry
                    .bitmap
                    .xor_into(&self.bitmap.bytes, &mut output, &mut work, cancel)?;
                if entry.xor == 0 {
                    break;
                }
                index -= entry.xor;
            }
            Ok(Bitmap {
                words: output.into(),
                objects: self.ids.len(),
            })
        };
        let result = decode();
        *budget = work;
        match result {
            Ok(bitmap) => {
                // A claimed commit closure must include the commit itself with commit type.
                let Some(&bit) = self.inverse.get(position) else {
                    return Ok(None);
                };
                if !bitmap.contains(bit)
                    || !self.bitmap.type_bitmap(ObjectKind::Commit).contains(bit)
                {
                    return Ok(None);
                }
                Ok(Some(bitmap))
            }
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Err(_) => {
                *budget = 0;
                Ok(None)
            }
        }
    }
}

pub(super) fn load(
    objects: &Objects,
    tips: &[ObjectId],
    limits: QueryLimits,
    cancel: &AtomicBool,
) -> Result<(Vec<Bound>, usize), Error> {
    if tips.is_empty() || !objects.bitmaps_enabled || !objects.shallow_roots().is_empty() {
        return Ok((Vec::new(), limits.bitmap.max_decoded_words));
    }
    let mut remaining = limits.bitmap;
    let mut output = Vec::new();
    if objects.multi_pack_index_enabled {
        check(cancel)?;
        if let Some(bound) = midx(objects, &mut remaining, cancel) {
            output.push(bound);
        }
    }
    for (path, pack) in objects.primary_packs_with_paths() {
        check(cancel)?;
        if pack.object_count() > remaining.max_objects {
            continue;
        }
        let bitmap_path = path.with_extension("bitmap");
        // Avoid both an all-tip scan for packs without bitmaps and reading unrelated sidecars.
        if !bitmap_path.try_exists().unwrap_or(false)
            || !tips
                .iter()
                .any(|&id| pack.find(id).ok().flatten().is_some())
        {
            continue;
        }
        let Some(bytes) = read(&bitmap_path, &mut remaining) else {
            continue;
        };
        let count = pack.object_count();
        let Ok(bitmap) = parse(&bytes, pack.checksum(), count, &mut remaining) else {
            continue;
        };
        check(cancel)?;
        if pack
            .check_unchanged()
            .and_then(|()| pack.verify_index())
            .is_err()
        {
            continue;
        }
        let Ok(order) = pack.positions_by_offset() else {
            continue;
        };
        let order: Vec<_> = order.collect();
        // A missing or damaged optional .rev can be replaced by the order proved by .idx offsets.
        if let Some(bytes) = read(&path.with_extension("rev"), &mut remaining)
            && let Ok(reverse) = ReverseIndex::parse_with(
                &bytes,
                pack.checksum(),
                count,
                file_limits(&bytes, remaining),
            )
            && !reverse
                .positions()
                .map(|position| position as usize)
                .eq(order.iter().copied())
        {
            continue;
        }
        let ids = (0..count)
            .map(|position| pack.identity_at(position))
            .collect();
        remaining.max_objects -= count;
        remaining.max_entries = remaining.max_entries.saturating_sub(bitmap.entries.len());
        match Bound::new(bitmap, ids, order, cancel) {
            Ok(bound) => output.push(bound),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => {}
        }
    }
    check(cancel)?;
    Ok((output, remaining.max_decoded_words))
}

fn read(path: &Path, limits: &mut Limits) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length > limits.max_bytes {
        return None;
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).ok()?;
    file.take(length as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() != length {
        return None;
    }
    limits.max_bytes -= length;
    Some(bytes)
}

fn midx(objects: &Objects, remaining: &mut Limits, cancel: &AtomicBool) -> Option<Bound> {
    let directory = objects.store_directories().next()?.join("pack");
    let bytes = read(&directory.join("multi-pack-index"), remaining)?;
    // `read` has already charged this file's bytes to the aggregate input bound.
    let limits = crate::multi_pack_index::Limits {
        max_bytes: bytes.len(),
        max_objects: remaining.max_objects,
        ..Default::default()
    };
    let index = crate::multi_pack_index::MultiPackIndex::parse_with(
        &bytes,
        objects.object_format(),
        limits,
    )
    .ok()?;
    if index.base_count() != 0 {
        return None;
    }
    check(cancel).ok()?;
    for (path, pack) in objects.primary_packs_with_paths() {
        check(cancel).ok()?;
        let name = path.with_extension("idx");
        if index
            .pack_names()
            .iter()
            .any(|&candidate| name.file_name().is_some_and(|name| name == candidate))
        {
            pack.check_unchanged().ok()?;
        }
    }
    index.verify(objects).ok()?;
    check(cancel).ok()?;
    let (ids, embedded_order) = index.bitmap_inventory()?;
    let width = objects.object_format().digest_len();
    let anchor = ObjectId::from_bytes(
        objects.object_format(),
        bytes.get(bytes.len().checked_sub(width)?..)?,
    )
    .ok()?;
    let external_order = read(
        &directory.join(format!("multi-pack-index-{anchor}.rev")),
        remaining,
    )
    .and_then(|bytes| {
        ReverseIndex::parse_with(&bytes, anchor, ids.len(), file_limits(&bytes, *remaining)).ok()
    })
    .map(|reverse| {
        reverse
            .positions()
            .map(|position| position as usize)
            .collect::<Vec<_>>()
    });
    if let (Some(embedded), Some(external)) = (&embedded_order, &external_order)
        && embedded != external
    {
        return None;
    }
    let order = embedded_order.or(external_order)?;
    // Prove pseudo-pack order from verified index locations, with the first pack preferred.
    let preferred = index.location(*order.first()?).ok()?.pack;
    let key = |position| {
        let location = index.location(position).ok()?;
        Some((
            if location.pack == preferred {
                0
            } else {
                u64::from(location.pack) + 1
            },
            location.offset,
        ))
    };
    for pair in order.windows(2) {
        check(cancel).ok()?;
        if key(pair[0])? >= key(pair[1])? {
            return None;
        }
    }
    let bitmap_bytes = read(
        &directory.join(format!("multi-pack-index-{anchor}.bitmap")),
        remaining,
    )?;
    let bitmap = parse(&bitmap_bytes, anchor, ids.len(), remaining).ok()?;
    remaining.max_objects -= ids.len();
    remaining.max_entries = remaining.max_entries.saturating_sub(bitmap.entries.len());
    Bound::new(bitmap, ids, order, cancel).ok()
}

// Files have already been charged by `read`; retain the remaining bounds for derived allocations.
fn file_limits(bytes: &[u8], mut limits: Limits) -> Limits {
    limits.max_bytes = bytes.len();
    limits
}

fn parse(
    bytes: &[u8],
    anchor: ObjectId,
    objects: usize,
    remaining: &mut Limits,
) -> Result<BitmapIndex, Error> {
    // Four full-width type outputs and at most four full-width expansions. Reserve the worst
    // case before parsing, so repeated metadata sources share the same bounded work allowance.
    let words = objects
        .div_ceil(64)
        .checked_mul(8)
        .ok_or(Error::Limit("decoded words"))?;
    if words > remaining.max_decoded_words {
        return Err(Error::Limit("decoded words"));
    }
    let result = BitmapIndex::parse_with(bytes, anchor, objects, file_limits(bytes, *remaining));
    remaining.max_decoded_words -= words;
    result
}
