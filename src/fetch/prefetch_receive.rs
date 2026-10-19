//! Maintenance transfer from a source already admitted under explicit read budgets.
use std::ops::ControlFlow;
use std::sync::atomic::AtomicBool;

use super::prefetch_plan::PrefetchPlanLimits;
use super::{FetchReady, FetchRequest, FetchWorkflowError};
use crate::fetch::local_native::{AdmittedSource, receive_advertised_controlled};
use crate::fetch::{FetchError, FetchLimits, KnownHistory, check_cancelled};
use crate::transport::TransportControl;

impl FetchRequest {
    /// Plans and receives from the held source snapshot, without reopening or rediscovery.
    ///
    /// The maintenance caller uses controlled preparation and supplies its fixed reflog policy.
    /// Source location, refs, peels and pack indexes were bounded by source admission;
    /// `transfer.source_packs`, advertisement bytes and entry limits are not consulted again.
    /// Planning and transfer use independent budgets. Want IDs are bounded before each push.
    /// Store-backed lookups share caller read controls and aggregate fallback budgets through
    /// selection, import and connectivity. Retained knowledge is admitted before transfer;
    /// tree-boundary discovery keeps its separate bounded phase.
    /// This entry point supports neither requested depth nor filtering; shallow sources refuse.
    /// Cancellation is checked between phases and at most every 64 planned updates.
    /// The existing native transfer's read, graph, compression and import bounds still apply.
    ///
    /// Failure returns no ready handle and publishes nothing. Successful receive owns only the
    /// destination request and validated artifact; it retains no source handle. Installation and
    /// reference publication remain separate, through the controlled ordinary finish path.
    pub(crate) fn receive_prefetch_controlled(
        self,
        source: AdmittedSource,
        known: &KnownHistory,
        planning: PrefetchPlanLimits,
        transfer: FetchLimits,
        cancel: &AtomicBool,
    ) -> Result<FetchReady, FetchWorkflowError> {
        check_cancelled(cancel)?;
        if self.depth.is_some() || self.filter.is_some() {
            return Err(FetchError::Unsupported("maintenance prefetch depth or filter").into());
        }
        self.check_known(known)?;
        let (repository, objects, advertisement) = source.into_parts();
        if !repository.shallow_roots().is_empty() {
            return Err(FetchError::Unsupported("shallow local source").into());
        }
        if repository.object_format() != self.repository.object_format() {
            return Err(FetchError::FormatMismatch {
                remote: repository.object_format(),
                local: self.repository.object_format(),
            }
            .into());
        }
        let updates = self.plan_prefetch_controlled(&advertisement, planning, cancel)?;
        let mut wants = Vec::new();
        for batch in updates.chunks(64) {
            check_cancelled(cancel)?;
            for update in batch {
                if let Some(source) = &update.mapping.source {
                    if wants.len() >= transfer.max_wants {
                        return Err(FetchError::Limit("wants").into());
                    }
                    wants.push(source.id);
                }
            }
        }
        let received = receive_advertised_controlled(
            &repository,
            objects,
            advertisement,
            |_| wants,
            known,
            transfer,
            TransportControl::new(cancel),
            |_| ControlFlow::Continue(()),
            |_| {},
            |_| {},
        )?;
        check_cancelled(cancel)?;
        Ok(FetchReady {
            request: self,
            updates,
            received,
        })
    }
}
