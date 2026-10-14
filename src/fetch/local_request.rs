//! Native source selection and synchronous observer configuration.
use std::ops::ControlFlow;

use super::{FetchLimits, KnownHistory, LocalFetchProgress, ValidationProgress};

/// Work bounds and optional verified destination history for a local fetch.
///
/// Empty knowledge requests full history. The caller keeps verified history available through
/// installation; this request grants no retention against concurrent destination collection.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LocalFetchRequest<'a> {
    /// Native storage and graph bounds.
    pub limits: FetchLimits,
    /// Verified destination history, or none for a full transfer.
    pub known: Option<&'a KnownHistory>,
}

impl<'a> LocalFetchRequest<'a> {
    /// Selects bounds for a full transfer.
    pub fn new(limits: FetchLimits) -> Self {
        Self {
            limits,
            known: None,
        }
    }

    /// Excludes explicitly verified destination history.
    #[must_use]
    pub fn with_known(mut self, known: &'a KnownHistory) -> Self {
        self.known = Some(known);
        self
    }
}

type RemoteMessages<'a> = dyn FnMut(&[u8]) -> ControlFlow<()> + 'a;

/// Independent synchronous native-fetch notifications.
///
/// Messages may cancel by returning `Break`. Construction and validation callbacks cannot undo
/// completed work; set the transport cancellation flag to stop at the next cooperative check.
/// Observers must return promptly. Completion never establishes installation or publication.
#[derive(Default)]
pub struct LocalFetchObserver<'a> {
    pub(super) messages: Option<&'a mut RemoteMessages<'a>>,
    pub(super) construction: Option<&'a mut dyn FnMut(LocalFetchProgress)>,
    pub(super) validation: Option<&'a mut dyn FnMut(ValidationProgress)>,
}

impl std::fmt::Debug for LocalFetchObserver<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalFetchObserver").finish_non_exhaustive()
    }
}

impl<'a> LocalFetchObserver<'a> {
    /// Receives remote messages with the original cancellation contract.
    #[must_use]
    pub fn with_messages(mut self, notify: &'a mut dyn FnMut(&[u8]) -> ControlFlow<()>) -> Self {
        self.messages = Some(notify);
        self
    }

    /// Receives completed source reads and outgoing pack construction work.
    #[must_use]
    pub fn with_construction(mut self, notify: &'a mut dyn FnMut(LocalFetchProgress)) -> Self {
        self.construction = Some(notify);
        self
    }

    /// Receives actual object and delta validation work, before installation.
    ///
    /// Pack-free transfers produce no snapshots. Completion marks validation only; later deadline
    /// checks or planning may still fail. Observer panics unwind before destination publication.
    #[must_use]
    pub fn with_validation(mut self, notify: &'a mut dyn FnMut(ValidationProgress)) -> Self {
        self.validation = Some(notify);
        self
    }
}
