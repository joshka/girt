use std::ops::ControlFlow;
use std::sync::Arc;

use super::{Advertisement, FetchError, FetchLimits, KnownHistory, ReceivedFetch, protocol};
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
    /// callbacks run here, after network completion. Installation is a separate synchronous call.
    ///
    /// # Errors
    ///
    /// Returns [`super::receive_with_known`]'s validation failures. No repository is changed.
    pub fn validate(
        self,
        cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<ReceivedFetch, FetchError> {
        #[cfg(feature = "tracing")]
        {
            let context = self.trace.clone();
            context.enter(|| self.validate_inner(cancel, progress))
        }
        #[cfg(not(feature = "tracing"))]
        self.validate_inner(cancel, progress)
    }

    fn validate_inner(
        self,
        cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
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
            let empty = KnownHistory::default();
            let history = self.known.as_deref().unwrap_or(&empty);
            if !self.negotiation.needs_pack {
                return ReceivedFetch::without_pack(
                    self.advertisement,
                    self.negotiation.wants,
                    history,
                    self.limits,
                    cancel,
                );
            }
            let mut reader = self.body.as_slice();
            let mut wire = Wire {
                reader: &mut reader,
                remaining: self.remaining,
                cancel,
            };
            protocol::response(
                &mut wire,
                self.advertisement,
                self.negotiation,
                history,
                self.limits,
                cancel,
                progress,
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
