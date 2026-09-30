//! Local fetch construction and validation work, separate from receiver sideband messages.
use std::sync::atomic::AtomicBool;

/// Completed work while constructing a fetch from a local repository.
///
/// Observers run synchronously and must return promptly. Cancellation remains controlled by
/// [`crate::transport::TransportControl`]; callbacks cannot undo a completed operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFetchProgress {
    /// Unique reachable source objects read and checked so far. The total is not yet known.
    /// Includes objects excluded from the outgoing pack by verified known history.
    Reading {
        /// Completed source objects; begins at zero and never decreases.
        objects: u64,
    },
    /// Successfully encoded outgoing entries and the selected unique-object total.
    /// Finishing these counts does not establish a finished pack/index pair.
    Packing {
        /// `(done, total)`; excludes objects already in verified known history.
        objects: (u64, u64),
    },
    /// Construction and the final cancellation/deadline check succeeded. Last notification;
    /// setting cancellation here cannot undo completion. Installation and publication are separate.
    Complete,
}

/// A snapshot of completed local fetch-validation work.
///
/// Counts describe pack processing after network download, not network byte transfer. Object and
/// delta counters never decrease during one validation; delta totals become known only after all
/// entries have been decoded. Finishing either count does not establish a valid fetch: index
/// construction, thin-pack rewriting and selected-tip connectivity can still fail. Only
/// `complete` marks successful validation. Installation and reference publication remain separate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ValidationProgress {
    /// Successfully inflated pack entries and the pack header's entry count, `(done, total)`.
    /// Includes delta programs; it does not imply reconstructed or connected objects.
    pub objects: (u64, u64),

    /// Successfully reconstructed delta entries and the actual delta entry count, `(done, total)`.
    /// `None` means entry decoding has not established the total. External thin-pack bases are not
    /// counted as additional received objects or deltas.
    pub deltas: Option<(u64, u64)>,

    /// True only after pack, index, thin-pack rewriting and connectivity validation succeed and
    /// the final cancellation check passes. This is the last notification; callback-triggered
    /// cancellation at this point cannot undo completed validation.
    pub complete: bool,
}

pub(super) struct ValidationObserver<'a> {
    pub cancel: &'a AtomicBool,
    value: ValidationProgress,
    notify: &'a mut dyn FnMut(ValidationProgress),
}

impl<'a> ValidationObserver<'a> {
    pub fn new(cancel: &'a AtomicBool, notify: &'a mut dyn FnMut(ValidationProgress)) -> Self {
        Self {
            cancel,
            value: ValidationProgress::default(),
            notify,
        }
    }

    pub fn decoding(&mut self, total: usize) {
        self.value.objects = (0, total as u64);
        (self.notify)(self.value);
    }

    pub fn decoded(&mut self) {
        self.value.objects.0 += 1;
        (self.notify)(self.value);
    }

    pub fn resolving(&mut self, total: usize) {
        self.value.deltas = Some((0, total as u64));
        (self.notify)(self.value);
    }

    pub fn resolved_delta(&mut self) {
        self.value.deltas.as_mut().unwrap().0 += 1;
        (self.notify)(self.value);
    }

    pub fn complete(&mut self) {
        self.value.deltas.get_or_insert((0, 0));
        self.value.complete = true;
        (self.notify)(self.value);
    }
}
