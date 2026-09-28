use std::ops::ControlFlow;
use std::sync::Arc;

use super::progress::ValidationObserver;
use super::{
    Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch, ValidationProgress,
    protocol,
};
use crate::packet::Wire;

/// A bounded network response awaiting synchronous pack and connectivity validation.
///
/// Owns downloaded protocol bytes and, when supplied, an [`Arc`] retaining the exact verified
/// history used for negotiation. History cannot be substituted during validation. It is
/// `Send + Sync + 'static` and can move into a caller-managed blocking worker after the initiating
/// scope ends. No history allocation is retained for a `None` input. Validation consumes this
/// result and releases its history ownership on success or failure; dropping it does the same
/// while discarding the bytes. Other Arc owners can keep history alive independently.
///
/// Downloading has no local filesystem side effects. A successful download is not evidence of a
/// valid pack. With `tracing`, this value also retains the initiating span and subscriber until
/// validation or drop. Validation restores them on the worker, then restores its previous context;
/// the retained network span's lifetime includes queue/validation time. Subscriber state must not
/// assume release on the initiating thread. Callers bound queued downloads, retained bytes and
/// active workers, and observe worker completion even after requesting cancellation; dropping a
/// worker handle does not stop its work.
pub struct DownloadedFetch {
    #[cfg(feature = "tracing")]
    pub(super) trace: crate::trace::DownloadContext,
    pub(super) advertisement: Advertisement,
    pub(super) negotiation: protocol::Negotiation,
    pub(super) known: Option<Arc<KnownHistory>>,
    pub(super) limits: FetchLimits,
    pub(super) remaining: usize,
    pub(super) body: Vec<u8>,
}

impl std::fmt::Debug for DownloadedFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadedFetch")
            .field("wire_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

impl DownloadedFetch {
    /// Validates packet framing, pack objects and selected-tip connectivity synchronously.
    ///
    /// Run on a caller-managed bounded CPU worker for large inputs. This work may process up to
    /// `max_decode_bytes`, `max_resolution_steps` and `max_connectivity_edges`; bounded input does
    /// not imply a short execution time. Cancellation is cooperative between packets/objects/graph
    /// steps, not during a single hash, inflate or parse. The network deadline has ended. Progress
    /// callbacks run here, after network completion, replaying any notices already observed through
    /// a live HTTP callback. Pass a no-op callback to avoid duplicate display. Installation is a
    /// separate synchronous call.
    ///
    /// # Errors
    ///
    /// Returns [`super::receive_with_known`]'s validation failures. No repository is changed.
    pub fn validate(
        self,
        cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<ReceivedFetch, FetchError> {
        self.validate_with_progress(cancel, progress, |_| {})
    }

    /// Validates the download while reporting actual local object and delta work.
    ///
    /// `observe` runs synchronously on the validation worker and receives [`ValidationProgress`]
    /// snapshots. It is advisory; cancellation still uses `cancel` or the sideband callback's
    /// `ControlFlow`. Counts are phase work, not network transfer or publication. The final
    /// `complete` notification occurs only on successful validation, after its last cancellation
    /// check. Earlier notifications can precede a validation failure.
    ///
    /// The `progress` callback retains [`Self::validate`]'s sideband replay behavior; use a no-op
    /// there when live receiver notices were already displayed. Callbacks should return promptly.
    ///
    /// # Errors
    ///
    /// Returns [`Self::validate`]'s errors without storage changes. Failures do not emit
    /// completion.
    pub fn validate_with_progress(
        self,
        cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
        mut observe: impl FnMut(ValidationProgress),
    ) -> Result<ReceivedFetch, FetchError> {
        #[cfg(feature = "tracing")]
        {
            let context = self.trace.clone();
            context.enter(|| self.validate_inner(cancel, progress, &mut observe))
        }
        #[cfg(not(feature = "tracing"))]
        self.validate_inner(cancel, progress, &mut observe)
    }

    fn validate_inner(
        self,
        cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
        observe: &mut dyn FnMut(ValidationProgress),
    ) -> Result<ReceivedFetch, FetchError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "fetch.validate",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
            wire_bytes = self.body.len(),
        );

        let operation = || {
            let mut observer = ValidationObserver::new(cancel, observe);
            let empty = KnownHistory::default();
            let history = self.known.as_deref().unwrap_or(&empty);
            if !self.negotiation.needs_pack {
                let received = ReceivedFetch::without_pack(
                    self.advertisement,
                    self.negotiation.wants,
                    history,
                    self.limits,
                    cancel,
                )?;
                super::check_cancelled(cancel)?;
                observer.complete();
                return Ok(received);
            }
            let mut reader = self.body.as_slice();
            let mut wire = Wire {
                reader: &mut reader,
                remaining: self.remaining,
                cancel,
            };
            protocol::response_observed(
                &mut wire,
                self.advertisement,
                self.negotiation,
                history,
                self.limits,
                progress,
                &mut observer,
            )
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::fetch);

        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rstest::rstest;

    use super::*;
    use crate::{ObjectFormat, ObjectId, ObjectKind, PackObject, PackWriteLimits};

    fn pack(format: ObjectFormat, kind: ObjectKind, data: &[u8]) -> (ObjectId, Vec<u8>) {
        let object = crate::Object {
            format,
            kind,
            data: data.to_vec(),
        };
        let id = object.id();
        let mut pack = Vec::new();
        crate::write_pack(
            format,
            &[PackObject { id, kind, data }],
            &mut pack,
            &mut Vec::new(),
            PackWriteLimits::default(),
        )
        .unwrap();
        (id, pack)
    }

    fn download(
        format: ObjectFormat,
        wanted: ObjectId,
        pack: Vec<u8>,
        known: Option<Arc<KnownHistory>>,
    ) -> DownloadedFetch {
        let advertisement = Advertisement {
            refs: vec![super::super::AdvertisedRef {
                name: crate::refs::RefName::new("refs/heads/main").unwrap(),
                id: wanted,
                peeled: false,
            }],
            capabilities: vec![
                b"side-band-64k".to_vec(),
                format!("object-format={format}").into_bytes(),
            ],
        };
        let empty = KnownHistory::default();
        let cancel = AtomicBool::new(false);
        let limits = FetchLimits::default();
        let negotiation = protocol::request(
            &mut Vec::new(),
            &advertisement,
            vec![wanted],
            known.as_deref().unwrap_or(&empty),
            None,
            limits,
            &cancel,
        )
        .unwrap();
        let mut body = b"0008NAK\n".to_vec();
        let payload = [b"\x01".as_slice(), &pack].concat();
        crate::packet::packet(&mut body, &payload, &cancel).unwrap();
        body.extend(b"0000");
        DownloadedFetch {
            #[cfg(feature = "tracing")]
            trace: crate::trace::DownloadContext::capture(),
            advertisement,
            negotiation,
            known,
            limits,
            remaining: limits.max_wire_bytes,
            body,
        }
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn validation_progress_completes_only_after_all_checks(#[case] format: ObjectFormat) {
        let (id, pack) = pack(format, ObjectKind::Blob, b"object");
        let mut snapshots = Vec::new();
        let received = download(format, id, pack, None)
            .validate_with_progress(
                &AtomicBool::new(false),
                |_| ControlFlow::Continue(()),
                |state| snapshots.push(state),
            )
            .unwrap();
        assert_eq!(received.object_count(), 1);
        assert_eq!(
            snapshots,
            [
                ValidationProgress {
                    objects: (0, 1),
                    deltas: None,
                    complete: false
                },
                ValidationProgress {
                    objects: (1, 1),
                    deltas: None,
                    complete: false
                },
                ValidationProgress {
                    objects: (1, 1),
                    deltas: Some((0, 0)),
                    complete: false
                },
                ValidationProgress {
                    objects: (1, 1),
                    deltas: Some((0, 0)),
                    complete: true
                },
            ]
        );
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn connectivity_failure_never_reports_completion(#[case] format: ObjectFormat) {
        let (id, pack) = pack(format, ObjectKind::Commit, b"invalid commit");
        let mut snapshots = Vec::new();
        let result = download(format, id, pack, None).validate_with_progress(
            &AtomicBool::new(false),
            |_| ControlFlow::Continue(()),
            |state| snapshots.push(state),
        );
        assert!(result.is_err());
        assert_eq!(snapshots.last().unwrap().objects, (1, 1));
        assert!(snapshots.iter().all(|state| !state.complete));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn corrupt_pack_never_reports_decoded_work(#[case] format: ObjectFormat) {
        let (id, mut pack) = pack(format, ObjectKind::Blob, b"object");
        *pack.last_mut().unwrap() ^= 1;
        let mut snapshots = Vec::new();
        let result = download(format, id, pack, None).validate_with_progress(
            &AtomicBool::new(false),
            |_| ControlFlow::Continue(()),
            |state| snapshots.push(state),
        );
        assert!(matches!(
            result,
            Err(FetchError::Pack(crate::ObjectReadError::Corrupt(
                "received pack checksum"
            )))
        ));
        assert!(snapshots.is_empty());
    }

    #[rstest]
    #[case::before_entry(0)]
    #[case::after_entry(1)]
    fn observer_cancellation_preserves_cancelled_outcome(#[case] stop_at: u64) {
        let (id, pack) = pack(ObjectFormat::Sha1, ObjectKind::Blob, b"object");
        let cancel = AtomicBool::new(false);
        let mut snapshots = Vec::new();
        let result = download(ObjectFormat::Sha1, id, pack, None).validate_with_progress(
            &cancel,
            |_| ControlFlow::Continue(()),
            |state| {
                snapshots.push(state);
                if state.objects.0 == stop_at {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(matches!(result, Err(FetchError::Cancelled)));
        assert!(snapshots.iter().all(|state| !state.complete));
    }

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn known_only_validation_completes_without_pack_work(#[case] format: ObjectFormat) {
        let object = crate::Object {
            format,
            kind: ObjectKind::Blob,
            data: b"object".to_vec(),
        };
        let id = object.id();
        let known = Arc::new(KnownHistory {
            objects: [(id, object)].into(),
            ..Default::default()
        });
        let mut snapshots = Vec::new();
        let received = download(format, id, vec![], Some(known))
            .validate_with_progress(
                &AtomicBool::new(false),
                |_| ControlFlow::Continue(()),
                |state| snapshots.push(state),
            )
            .unwrap();
        assert_eq!(received.object_count(), 0);
        assert_eq!(
            snapshots,
            [ValidationProgress {
                objects: (0, 0),
                deltas: Some((0, 0)),
                complete: true
            }]
        );
    }
}
