//! Capped source reads for maintenance selection and pack construction.
//!
//! These additive hooks keep the trusted transfer methods unchanged. Loose input is admitted before
//! inflation; packed reads retain their existing budgets and identity policy. Source transfer
//! normally bypasses replacement refs, matching trusted selection and location. The damaged-pack
//! fallback retains the verified reader's replacement policy.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{ObjectReadError, Objects, ReadLimits, Transfer, UnverifiedObject, check_cancelled};
use crate::{ObjectId, ObjectKind};

impl Objects {
    /// Reads selection edges with compressed loose-input admission and cancellable inflation.
    pub(crate) fn read_unverified_capped(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<UnverifiedObject>, ObjectReadError> {
        check_cancelled(cancel)?;
        for store in &self.stores {
            check_cancelled(cancel)?;
            for pack in &store.packs {
                check_cancelled(cancel)?;
                if let Some(position) = pack.find(id)? {
                    match pack.read_candidate_unverified(position, limits, cancel) {
                        Ok(object) => {
                            check_cancelled(cancel)?;
                            return Ok(Some(object));
                        }
                        Err(crate::pack::ReadFailure::Damaged(_)) => {
                            // A damaged entry cannot provide unchecked selection edges. Preserve
                            // the trusted fallback's full verification, with loose input capped.
                            return self.read_capped(id, limits, cancel).map(|object| {
                                object.map(|object| UnverifiedObject {
                                    kind: object.kind,
                                    data: object.data,
                                })
                            });
                        }
                        Err(crate::pack::ReadFailure::Other(error)) => return Err(error),
                    }
                }
            }
            match store.loose.read_capped(id, limits, cancel) {
                Ok(object) => {
                    check_cancelled(cancel)?;
                    return Ok(Some(UnverifiedObject {
                        kind: object.kind,
                        data: object.data,
                    }));
                }
                Err(ObjectReadError::Loose(crate::loose::Error::Io(error)))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        check_cancelled(cancel)?;
        Ok((id == self.format.empty_tree()).then(|| UnverifiedObject {
            kind: ObjectKind::Tree,
            data: Arc::default(),
        }))
    }

    /// Locates transfer metadata, checking compressed loose input before releasing its payload.
    pub(crate) fn transfer_object_capped(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<(ObjectKind, usize, Option<crate::pack::Packed>)>, ObjectReadError> {
        Ok(match self.locate_transfer_capped(id, limits, cancel)? {
            None => None,
            Some(Transfer::Packed(pack, packed)) => {
                let (kind, length) = pack.object_header(packed.position, limits)?;
                check_cancelled(cancel)?;
                Some((kind, length, Some(packed)))
            }
            Some(Transfer::Stored(kind, length)) => Some((kind, length, None)),
        })
    }

    /// Locates a transfer entry without reading its packed header; loose data is checked fully.
    pub(crate) fn transfer_location_capped(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<Option<crate::pack::Packed>>, ObjectReadError> {
        Ok(self
            .locate_transfer_capped(id, limits, cancel)?
            .map(|entry| match entry {
                Transfer::Packed(_, packed) => Some(packed),
                Transfer::Stored(..) => None,
            }))
    }

    fn locate_transfer_capped(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<Transfer<'_>>, ObjectReadError> {
        check_cancelled(cancel)?;
        id.require_format(self.format)
            .map_err(crate::loose::Error::from)?;
        let mut pack_index = 0;
        for store in &self.stores {
            check_cancelled(cancel)?;
            for pack in &store.packs {
                check_cancelled(cancel)?;
                if let Some(position) = pack.find(id)? {
                    return Ok(Some(Transfer::Packed(
                        pack,
                        crate::pack::Packed {
                            id,
                            pack: pack_index,
                            position,
                        },
                    )));
                }
                pack_index += 1;
            }
            match store.loose.read_capped(id, limits, cancel) {
                Ok(object) => {
                    check_cancelled(cancel)?;
                    return Ok(Some(Transfer::Stored(object.kind(), object.data().len())));
                }
                Err(ObjectReadError::Loose(crate::loose::Error::Io(error)))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        check_cancelled(cancel)?;
        Ok((id == self.format.empty_tree()).then_some(Transfer::Stored(ObjectKind::Tree, 0)))
    }
}
