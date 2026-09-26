//! Private, categorical span completion. Never format inputs or errors here.

pub(crate) fn finish<T, E>(
    span: &tracing::Span,
    result: &Result<T, E>,
    classify: impl FnOnce(&E) -> &'static str,
) {
    match result {
        Ok(_) => {
            span.record("outcome", "success");
        }
        Err(error) => {
            let class = classify(error);
            span.record(
                "outcome",
                if class == "cancelled" {
                    "cancelled"
                } else {
                    "failure"
                },
            );
            span.record("failure_class", class);
        }
    }
}

fn io(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "missing",
        std::io::ErrorKind::AlreadyExists => "conflict",
        _ => "io",
    }
}

pub(crate) fn loose(error: &crate::Error) -> &'static str {
    use crate::Error::*;
    match error {
        ObjectFormat(_) | UnknownObjectType => "unsupported",
        Io(e) => io(e),
        UnsupportedObjectType => "wrong_kind",
        Corrupt(_) | Tree(_) | Commit(_) | Tag(_) => "corrupt",
        TooLarge => "limit",
        ConflictingObject => "conflict",
    }
}

pub(crate) fn object(error: &crate::ObjectReadError) -> &'static str {
    use crate::ObjectReadError::*;
    match error {
        Cancelled => "cancelled",
        Path { source, .. } => io(source),
        PackArtifacts { source, .. } => object(source),
        Loose(e) => loose(e),
        Unsupported(_) | IndexVersion(_) | PackVersion(_) | ObjectType(_) => "unsupported",
        Alternate { .. } | Corrupt(_) | DeltaCycle => "corrupt",
        Limit(_) => "limit",
        MissingBase(_) => "missing",
    }
}

pub(crate) fn history(error: &crate::HistoryError) -> &'static str {
    use crate::HistoryError::*;
    match error {
        Missing(_) => "missing",
        NotCommit(_) => "wrong_kind",
        Read { source, .. } => object(source),
        Parse { .. } | Cycle => "corrupt",
        Limit(_) => "limit",
    }
}

fn pack(error: &crate::PackWriteError) -> &'static str {
    use crate::PackWriteError::*;
    match error {
        ObjectFormat(_) => "unsupported",
        Identity { .. } => "corrupt",
        ConflictingDuplicate(_) => "conflict",
        Limit(_) => "limit",
        Io(e) => io(e),
    }
}

#[cfg(feature = "http")]
fn http(error: &crate::transport::http::HttpError) -> &'static str {
    use crate::transport::http::HttpError::*;
    match error {
        Configuration(_) => "invalid_input",
        Credential(_) => "authentication",
        Status(_) => "remote",
        Protocol(_) => "protocol",
        Limit(_) => "limit",
        Network => "transport",
        Cancelled => "cancelled",
        Deadline => "deadline",
    }
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
fn ssh(error: &crate::transport::ssh::SshError) -> &'static str {
    use crate::transport::ssh::SshError::*;
    match error {
        Configuration(_) => "invalid_input",
        Io(_) => "io",
        Exit(_) => "remote",
        Protocol(_) => "protocol",
        Limit => "limit",
        Cancelled => "cancelled",
        Deadline => "deadline",
    }
}

pub(crate) fn fetch(error: &crate::fetch::FetchError) -> &'static str {
    use crate::fetch::FetchError::*;
    match error {
        ObjectFormat(_) | Unsupported(_) => "unsupported",
        #[cfg(feature = "http")]
        Http(e) => http(e),
        #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
        Ssh(e) => ssh(e),
        Io(e) => io(e),
        Protocol(_) => "protocol",
        Remote(_) | Process(_) => "remote",
        Unadvertised(_) => "invalid_input",
        Limit(_) => "limit",
        Cancelled => "cancelled",
        Deadline => "deadline",
        Pack(e) | Destination(e) | LocalRead { source: e, .. } => object(e),
        Index(e) | PackWrite(e) => pack(e),
        Peel(_) => "corrupt",
        Missing(_) => "missing",
        Kind(_) => "wrong_kind",
        Commit { .. } | Tree { .. } | Tag { .. } => "corrupt",
        Existing(_) => "conflict",
    }
}

pub(crate) fn push(error: &crate::push::PushError, span: &tracing::Span) -> &'static str {
    match error {
        crate::push::PushError::NotSent(e) => {
            span.record("effects", "not_sent");
            push_failure(e)
        }
        crate::push::PushError::Uncertain { cause, report } => {
            push_report(span, report);
            span.record("effects", "uncertain");
            push_failure(cause)
        }
    }
}

pub(crate) fn push_failure(error: &crate::push::PushFailure) -> &'static str {
    use crate::push::PushFailure::*;
    match error {
        Destination(_) => "invalid_input",
        Install(error) => fetch(error),
        Reference(error) => reference(error),
        ObjectFormat(_) | Unsupported(_) => "unsupported",
        #[cfg(feature = "http")]
        Http(e) => http(e),
        #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
        Ssh(e) => ssh(e),
        Io(e) => io(e),
        Protocol(_) => "protocol",
        Remote(_) | Process(_) => "remote",
        Limit(_) => "limit",
        Cancelled => "cancelled",
        Deadline => "deadline",
        Stale { .. } | KnowledgeChanged(_) | WouldForce(_) => "precondition",
        Command(_) => "invalid_input",
        Missing(_) => "missing",
        Kind(_) => "wrong_kind",
        Read { source, .. } => object(source),
        Pack(e) => pack(e),
        Commit { .. } | Tree { .. } | Tag { .. } => "corrupt",
    }
}

pub(crate) fn reference(error: &crate::refs::ReferenceError) -> &'static str {
    use crate::refs::ReferenceError::*;
    match error {
        ObjectFormat(_) | Unsupported(_) => "unsupported",
        Io { source, .. } | PackedDeleted { source, .. } => io(source),
        Malformed { .. } | Cycle(_) => "corrupt",
        Locked(_) | Conflict(_) => "conflict",
        Mismatch { .. } => "precondition",
        Depth(_) => "limit",
        Cancelled => "cancelled",
        Reftable(error) => match error {
            crate::refs::reftable::Error::Malformed(_) => "corrupt",
            crate::refs::reftable::Error::Unsupported(_) => "unsupported",
            crate::refs::reftable::Error::Limit(_) => "limit",
        },
        InvalidHeadTarget | ZeroId => "invalid_input",
    }
}

pub(crate) fn transaction(
    error: &crate::refs::TransactionError,
    span: &tracing::Span,
) -> &'static str {
    match error {
        crate::refs::TransactionError::Prepare { source, .. } => reference(source),
        crate::refs::TransactionError::Publish { source, .. } => {
            span.record("effects", "possibly_partial");
            reference(source)
        }
    }
}

pub(crate) fn index(error: &crate::index::StorageError, span: &tracing::Span) -> &'static str {
    use crate::index::StorageError::*;
    match error {
        Cleanup { operation, .. } => {
            span.record("effects", "cleanup_failed");
            index(operation, span)
        }
        Io { source, .. } => io(source),
        Locked(_) => "conflict",
        Changed(_) => "precondition",
        NotRegular(_) => "unsupported",
        MissingShared(_) => "missing",
        Format { source, .. } => match source {
            crate::index::Error::Limit(_) => "limit",
            crate::index::Error::ObjectFormat(_)
            | crate::index::Error::Version(_)
            | crate::index::Error::MandatoryExtension(_)
            | crate::index::Error::ExtensionPreventsEdit(_) => "unsupported",
            _ => "corrupt",
        },
    }
}

pub(crate) fn fetch_finish(
    error: &crate::fetch::FetchFinishError,
    span: &tracing::Span,
) -> &'static str {
    use crate::fetch::FetchFinishFailure::*;
    span.record("effects", "possibly_partial");
    match error.source.as_ref() {
        Installation(e) | BeforePublication(e) => fetch(e),
        Publication(e) => transaction(e, span),
        Safety(_) => "precondition",
        Update(e) => match e {
            crate::fetch::FetchUpdateError::Object(e) => fetch(e),
            crate::fetch::FetchUpdateError::History(e) => history(e),
            crate::fetch::FetchUpdateError::Peel(e) => peel(e),
            crate::fetch::FetchUpdateError::NonFastForward(_) => "precondition",
        },
    }
}

pub(crate) fn clone_finish(error: &crate::clone::CloneError, span: &tracing::Span) -> &'static str {
    use crate::clone::CloneFailure::*;
    span.record("effects", "possibly_partial");
    match &error.source {
        Exists(_) => "conflict",
        Url | Plan(_) | FetchPlan(_) => "invalid_input",
        Io(e) | Configuration(e) => io(e),
        Initialization(_) | Open(_) => "repository",
        Fetch(e) => fetch_finish(e, span),
        Verification(e) => fetch(e),
        Reference(e) => reference(e),
        Publication(e) => transaction(e, span),
    }
}

/// Keeps the caller's dispatch and parent together across an owned download handoff.
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
#[derive(Clone)]
pub(crate) struct DownloadContext {
    span: Option<tracing::Span>,
    dispatch: tracing::Dispatch,
}

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
impl DownloadContext {
    pub(crate) fn capture() -> Self {
        Self {
            span: Some(tracing::Span::current()),
            dispatch: tracing::dispatcher::get_default(Clone::clone),
        }
    }

    pub(crate) fn enter<T>(&self, operation: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span
                .as_ref()
                .expect("live download context")
                .in_scope(operation)
        })
    }
}

// Some subscribers close retained parents through the current dispatch. Release the final span
// under its owning dispatch too, including when a download is abandoned on another worker.
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
impl Drop for DownloadContext {
    fn drop(&mut self) {
        let span = self.span.take();
        tracing::dispatcher::with_default(&self.dispatch, || drop(span));
    }
}

pub(crate) fn push_report(span: &tracing::Span, report: &crate::push::PushReport) {
    if span.is_disabled() {
        return;
    }
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    let mut pending = 0usize;
    for reference in &report.refs {
        match &reference.status {
            Some(crate::push::Status::Ok) => accepted += 1,
            Some(crate::push::Status::Rejected(_)) => rejected += 1,
            None => pending += 1,
        }
    }
    let unpack = match &report.unpack {
        Some(crate::push::Status::Ok) => "accepted",
        Some(crate::push::Status::Rejected(_)) => "rejected",
        None => "unreported",
    };
    span.record("accepted", accepted)
        .record("rejected", rejected)
        .record("pending", pending)
        .record("unpack", unpack);
}

/// Categorical read-only tag resolution failures.
pub(crate) fn peel(error: &crate::PeelError) -> &'static str {
    use crate::PeelFailure::*;
    match error.source.as_ref() {
        Missing => "missing",
        Read(e) => object(e),
        Tag(_) | Commit(_) | Cycle => "corrupt",
        Kind { .. } => "wrong_kind",
        Depth | Bytes => "limit",
        Cancelled => "cancelled",
    }
}
