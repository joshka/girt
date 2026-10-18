use std::sync::atomic::AtomicBool;

use super::encode::{Output, artifacts};
use super::graph::Graph;
use super::{MultiPackWritten, WriteError, WriteLimits, WriteOptions, bound_objects, check};
use crate::multi_pack_index::MultiPackIndex;
use crate::{ObjectId, Objects};

pub(super) fn encode(
    objects: &Objects,
    mut midx: crate::multi_pack_index::WriteOptions,
    options: WriteOptions,
    limits: WriteLimits,
    cancel: &AtomicBool,
) -> Result<MultiPackWritten, WriteError> {
    check(cancel)?;
    if !objects.shallow_roots().is_empty() {
        return Err(WriteError::Shallow);
    }
    let mut packs = objects.primary_packs_with_paths();
    packs.sort_unstable_by_key(|(path, _)| *path);
    if packs.len() > limits.max_packs {
        return Err(WriteError::Limit("packs"));
    }
    if midx.preferred_pack.is_none() {
        midx.preferred_pack = packs
            .iter()
            .max_by_key(|(_, pack)| (pack.object_count(), pack.checksum()))
            .map(|(_, pack)| pack.checksum());
    }
    let base = objects.encode_multi_pack_index_with(
        midx,
        crate::multi_pack_index::WriteLimits {
            max_objects: limits.max_objects,
            max_bytes: limits.max_bytes,
            max_packs: limits.max_packs,
        },
        cancel,
    )?;
    let parse_limits = crate::multi_pack_index::Limits {
        max_bytes: limits.max_bytes,
        max_objects: limits.max_objects,
        max_packs: limits.max_packs,
    };
    let parsed = MultiPackIndex::parse_with(&base, objects.object_format(), parse_limits)?;
    bound_objects(parsed.len(), limits)?;
    let (ids, _) = parsed
        .bitmap_inventory()
        .ok_or(WriteError::Limit("MIDX inventory"))?;
    let preferred = packs
        .iter()
        .position(|(_, pack)| Some(pack.checksum()) == midx.preferred_pack)
        .ok_or(WriteError::Limit("preferred pack"))?;
    let mut locations = Vec::with_capacity(ids.len());
    for position in 0..ids.len() {
        check(cancel)?;
        locations.push(parsed.location(position)?);
    }
    let mut order: Vec<_> = (0..ids.len()).collect();
    order.sort_unstable_by_key(|&position| {
        let location = locations[position];
        (
            if location.pack as usize == preferred {
                0
            } else {
                u64::from(location.pack) + 1
            },
            location.offset,
        )
    });
    let mut reverse = Vec::with_capacity(order.len() * 4);
    let mut ranges = vec![(0_u32, 0_u32); packs.len()];
    for (bit, &position) in order.iter().enumerate() {
        check(cancel)?;
        reverse.extend_from_slice(&(position as u32).to_be_bytes());
        let pack = locations[position].pack as usize;
        if ranges[pack].1 == 0 {
            ranges[pack].0 = bit as u32;
        }
        ranges[pack].1 += 1;
    }
    let mut bitmapped = Vec::with_capacity(packs.len() * 8);
    for (start, count) in ranges {
        bitmapped.extend_from_slice(&start.to_be_bytes());
        bitmapped.extend_from_slice(&count.to_be_bytes());
    }
    let index = add_order(
        &base,
        &reverse,
        &bitmapped,
        objects.object_format(),
        limits.max_bytes,
    )?;
    let width = objects.object_format().digest_len();
    let anchor = ObjectId::from_bytes(objects.object_format(), &index[index.len() - width..])
        .map_err(|_| WriteError::Limit("MIDX checksum"))?;
    let graph = Graph::read(objects.object_format(), &ids, limits, cancel, |position| {
        let location = locations[position];
        let pack = packs[location.pack as usize].1;
        let index_position = pack
            .find(ids[position])?
            .ok_or(crate::ObjectReadError::Corrupt("bitmap MIDX object absent"))?;
        pack.read(index_position, limits.read, cancel)
    })?;
    let remaining = WriteLimits {
        max_bytes: limits
            .max_bytes
            .checked_sub(index.len())
            .ok_or(WriteError::Limit("artifact bytes"))?,
        ..limits
    };
    let bitmap = artifacts(anchor, &order, &graph, options, remaining, true, cancel)?;
    for (_, pack) in packs {
        check(cancel)?;
        pack.check_unchanged()?;
    }
    Ok(MultiPackWritten { index, bitmap })
}

/// Rebuilds the chunk table of our own verified MIDX encoder output with bitmap ordering chunks.
fn add_order(
    base: &[u8],
    reverse: &[u8],
    bitmapped: &[u8],
    format: crate::ObjectFormat,
    max_bytes: usize,
) -> Result<Vec<u8>, WriteError> {
    let count = usize::from(base[6]);
    let end = base.len() - format.digest_len();
    let old_start = 12 + (count + 1) * 12;
    let table_growth = 24;
    let mut output = Output::new(max_bytes);
    output.bytes(&base[..6])?;
    output.bytes(&[(count + 2) as u8])?;
    output.bytes(&base[7..12])?;
    for row in base[12..12 + count * 12].as_chunks::<12>().0 {
        output.bytes(&row[..4])?;
        let offset = u64::from_be_bytes(
            row[4..]
                .try_into()
                .map_err(|_| WriteError::Limit("MIDX chunk"))?,
        );
        output.long(offset + table_growth)?;
    }
    output.bytes(b"RIDX")?;
    output.long(end as u64 + table_growth)?;
    output.bytes(b"BTMP")?;
    output.long(end as u64 + table_growth + reverse.len() as u64)?;
    output.word(0)?;
    output.long(end as u64 + table_growth + reverse.len() as u64 + bitmapped.len() as u64)?;
    output.bytes(&base[old_start..end])?;
    output.bytes(reverse)?;
    output.bytes(bitmapped)?;
    output.checksum(format)?;
    Ok(output.data)
}
