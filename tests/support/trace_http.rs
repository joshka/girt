//! Original loopback HTTP fixture and manual interleaved polling, with no external service.
use std::future::Future;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use girt::fetch::{FetchLimits, receive_http};
use girt::transport::TransportControl;
use girt::transport::http::HttpRemote;
use tracing::Instrument;

use super::trace_capture::Capture;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn remote(listener: &TcpListener) -> HttpRemote {
    HttpRemote::new(
        &format!("http://{}/R03_SECRET_path", listener.local_addr().unwrap()),
        &[("Authorization", "Bearer R03_SECRET_credential")],
        &[],
    )
    .unwrap()
}
fn require_send<T: Send>(_: &T) {}
fn require_owned<T: Send + Sync + 'static>(_: &T) {}

#[test]
fn interleaved_pending_futures_do_not_leak_entered_spans() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = remote(&listener);
    let cancel = AtomicBool::new(false);
    let capture = Capture::default();
    let runtime = runtime();
    let _runtime = runtime.enter();
    let drop_context = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let drop_probe = DropContext(drop_context.clone());
    tracing::dispatcher::with_default(&capture.dispatch(), || {
        let mut first = Box::pin(
            receive_http(
                &remote,
                move |_| {
                    drop(drop_probe);
                    vec![]
                },
                None,
                FetchLimits::default(),
                TransportControl::new(&cancel),
            )
            .instrument(tracing::info_span!("first")),
        );
        let mut second = Box::pin(
            receive_http(
                &remote,
                |_| vec![],
                None,
                FetchLimits::default(),
                TransportControl::new(&cancel),
            )
            .instrument(tracing::info_span!("second")),
        );
        require_send(&first);
        require_send(&second);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(first.as_mut().poll(&mut cx), Poll::Pending));
        assert!(tracing::Span::current().is_none());
        assert!(matches!(second.as_mut().poll(&mut cx), Poll::Pending));
        assert!(tracing::Span::current().is_none());
        drop(first);
        drop(second);
        assert!(tracing::Span::current().is_none());
    });
    assert_eq!(*drop_context.lock().unwrap(), vec![Some("fetch.http")]);
    let spans = capture.spans();
    let network: Vec<_> = spans.iter().filter(|s| s.name == "fetch.http").collect();
    assert_eq!(network.len(), 2);
    let parents: std::collections::BTreeSet<_> =
        network.iter().map(|s| capture.parent_name(s)).collect();
    assert_eq!(parents, ["first", "second"].into_iter().collect());
    assert!(
        network
            .iter()
            .all(|s| s.closed && s.fields["outcome"] == "incomplete")
    );
    assert!(!format!("{spans:?}").contains("R03_SECRET"));
}

fn serve_empty(listener: TcpListener) {
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
        assert!(request.len() < 8192);
    }
    let body = b"001e# service=git-upload-pack\n00000000";
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(body).unwrap();
}

#[test]
fn owned_download_restores_dispatch_on_worker_and_releases_parent() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = remote(&listener);
    let server = std::thread::spawn(|| serve_empty(listener));
    let capture = Capture::default();
    let cancelled = AtomicBool::new(false);
    let download = tracing::dispatcher::with_default(&capture.dispatch(), || {
        runtime().block_on(
            receive_http(
                &remote,
                |_| vec![],
                None,
                FetchLimits::default(),
                TransportControl::new(&cancelled),
            )
            .instrument(tracing::info_span!("caller")),
        )
    })
    .unwrap();
    require_owned(&download);
    server.join().unwrap();
    assert!(!capture.named("fetch.http").closed);
    let worker_capture = Capture::default();
    let worker_dispatch = worker_capture.dispatch();
    let received = std::thread::spawn(move || {
        tracing::dispatcher::with_default(&worker_dispatch, || {
            let received = download
                .validate(&AtomicBool::new(false), |_| {
                    std::ops::ControlFlow::Continue(())
                })
                .unwrap();
            tracing::info_span!("worker_after_validation").in_scope(|| {});
            received
        })
    })
    .join()
    .unwrap();
    assert_eq!(received.object_count(), 0);
    let validation = capture.named("fetch.validate");
    assert_eq!(capture.parent_name(&validation), "fetch.http");
    assert_eq!(validation.fields["outcome"], "success");
    assert!(capture.named("fetch.http").closed);
    assert!(capture.named("caller").closed);
    assert!(worker_capture.named("worker_after_validation").closed);
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
}

#[test]
fn abandoning_download_on_worker_closes_original_spans() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = remote(&listener);
    let server = std::thread::spawn(|| serve_empty(listener));
    let capture = Capture::default();
    let cancel = AtomicBool::new(false);
    let download = tracing::dispatcher::with_default(&capture.dispatch(), || {
        runtime().block_on(
            receive_http(
                &remote,
                |_| vec![],
                None,
                FetchLimits::default(),
                TransportControl::new(&cancel),
            )
            .instrument(tracing::info_span!("caller")),
        )
    })
    .unwrap();
    server.join().unwrap();
    let other = Capture::default();
    let dispatch = other.dispatch();
    std::thread::spawn(move || tracing::dispatcher::with_default(&dispatch, || drop(download)))
        .join()
        .unwrap();
    assert!(capture.named("fetch.http").closed);
    assert!(capture.named("caller").closed);
    assert!(!capture.spans().iter().any(|s| s.name == "fetch.validate"));
    assert!(other.spans().is_empty());
}

#[test]
fn clone_finish_owns_fetch_installation_phases() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = remote(&listener);
    let server = std::thread::spawn(|| serve_empty(listener));
    let root = tempfile::tempdir().unwrap();
    let request = girt::clone::CloneRequest::prepare_tracking(
        root.path().join("clone"),
        girt::InitKind::Bare,
        b"https://R03_SECRET_metadata",
        girt::clone::BranchSelection::Default,
        girt::refs::Reflog::Preserve,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let ready = runtime()
        .block_on(request.receive_http(
            &remote,
            FetchLimits::default(),
            TransportControl::new(&cancel),
        ))
        .unwrap()
        .validate(&cancel, |_| std::ops::ControlFlow::Continue(()))
        .unwrap();
    server.join().unwrap();
    let capture = Capture::default();
    let report = tracing::dispatcher::with_default(&capture.dispatch(), || {
        ready.finish(Default::default(), &cancel)
    })
    .unwrap();
    assert!(report.repository.is_some());
    assert_eq!(
        capture.parent_name(&capture.named("fetch.finish")),
        "clone.finish"
    );
    assert_eq!(
        capture.parent_name(&capture.named("fetch.install")),
        "fetch.finish"
    );
    assert_eq!(capture.named("clone.finish").fields["outcome"], "success");
    assert!(!format!("{:?}", capture.spans()).contains("R03_SECRET"));
    assert!(capture.events().is_empty());
}

struct DropContext(std::sync::Arc<std::sync::Mutex<Vec<Option<&'static str>>>>);
impl Drop for DropContext {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap()
            .push(tracing::Span::current().metadata().map(|m| m.name()));
    }
}

#[test]
fn cancellation_after_suspension_records_cancelled_outcome() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = remote(&listener);
    let cancel = AtomicBool::new(false);
    let capture = Capture::default();
    let runtime = runtime();
    let _runtime = runtime.enter();
    let result = tracing::dispatcher::with_default(&capture.dispatch(), || {
        let mut future = Box::pin(receive_http(
            &remote,
            |_| vec![],
            None,
            FetchLimits::default(),
            TransportControl {
                cancel: &cancel,
                deadline: Some(std::time::Instant::now() + Duration::from_secs(5)),
            },
        ));
        assert!(matches!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
        assert!(tracing::Span::current().is_none());
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        runtime.block_on(future)
    });
    assert!(matches!(result, Err(girt::fetch::FetchError::Cancelled)));
    let span = capture.named("fetch.http");
    assert_eq!(span.fields["outcome"], "cancelled");
    assert_eq!(span.fields["failure_class"], "cancelled");
    assert!(span.closed);
}
