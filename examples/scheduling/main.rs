//! Bounded scheduling experiment; see docs/experiments/scheduling.md for scope and reproduction.
mod fixture;

use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fixture::{Fixture, Gate};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::{JoinHandle, JoinSet};

#[derive(Default)]
struct Counts {
    operation_ms: Mutex<Vec<f64>>,
    running: AtomicUsize,
    queued: AtomicUsize,
    peak_running: AtomicUsize,
    peak_queued: AtomicUsize,
}

// The supervisor owns completion even when the requesting receiver disappears. There are exactly
// eight requests in each batch: that is this experiment's admission/queue bound, not a public pool.
fn submit(
    fixture: Arc<Fixture>,
    slots: Arc<Semaphore>,
    counts: Arc<Counts>,
    cancel: Arc<AtomicBool>,
    gate: Option<Gate>,
) -> (oneshot::Receiver<&'static str>, JoinHandle<&'static str>) {
    let (send, receive) = oneshot::channel();
    let queued = counts.queued.fetch_add(1, SeqCst) + 1;
    counts.peak_queued.fetch_max(queued, SeqCst);
    let owner = tokio::spawn(async move {
        let permit = slots.acquire_owned().await.unwrap();
        counts.queued.fetch_sub(1, SeqCst);
        let result = if cancel.load(SeqCst) {
            drop(permit);
            "cancelled-before-dispatch"
        } else {
            tokio::task::spawn_blocking(move || {
                // Permit lives in the worker, never in the requesting future.
                let _permit = permit;
                let running = counts.running.fetch_add(1, SeqCst) + 1;
                counts.peak_running.fetch_max(running, SeqCst);
                // Once started, this read-only operation has no cooperative cancellation API.
                let started = Instant::now();
                fixture.operation(gate);
                counts
                    .operation_ms
                    .lock()
                    .unwrap()
                    .push(started.elapsed().as_secs_f64() * 1000.0);
                counts.running.fetch_sub(1, SeqCst);
                "completed"
            })
            .await
            .unwrap()
        };
        let _ = send.send(result);
        result
    });
    (receive, owner)
}

async fn batch(fixture: Arc<Fixture>, workers: bool, concurrency: usize, label: &str) {
    let slots = Arc::new(Semaphore::new(concurrency));
    let counts = Arc::new(Counts::default());
    let stop = Arc::new(AtomicBool::new(false));
    let timer_stop = stop.clone();
    let timer = tokio::spawn(async move {
        let mut samples = Vec::new();
        let period = Duration::from_millis(1);
        let mut next = tokio::time::Instant::now() + period;
        loop {
            tokio::time::sleep_until(next).await;
            let now = tokio::time::Instant::now();
            samples.push(now.saturating_duration_since(next).as_secs_f64() * 1000.0);
            if timer_stop.load(SeqCst) {
                break;
            }
            // Skip missed ticks; record the actual stall, without synthetic catch-up samples.
            next = now + period;
        }
        samples
    });
    tokio::task::yield_now().await;
    let start = Instant::now();
    let mut requests = JoinSet::new();
    let mut owners = Vec::new();
    for _ in 0..8 {
        let requested = Instant::now();
        if workers {
            let (result, owner) = submit(
                fixture.clone(),
                slots.clone(),
                counts.clone(),
                Arc::new(AtomicBool::new(false)),
                None,
            );
            owners.push(owner);
            requests.spawn(async move {
                assert_eq!(result.await.unwrap(), "completed");
                requested.elapsed().as_secs_f64() * 1000.0
            });
        } else {
            let fixture = fixture.clone();
            let slots = slots.clone();
            let counts = counts.clone();
            counts.queued.fetch_add(1, SeqCst);
            counts
                .peak_queued
                .fetch_max(counts.queued.load(SeqCst), SeqCst);
            requests.spawn(async move {
                let _permit = slots.acquire_owned().await.unwrap();
                counts.queued.fetch_sub(1, SeqCst);
                counts.running.fetch_add(1, SeqCst);
                counts.peak_running.fetch_max(1, SeqCst);
                let started = Instant::now();
                fixture.operation(None);
                counts
                    .operation_ms
                    .lock()
                    .unwrap()
                    .push(started.elapsed().as_secs_f64() * 1000.0);
                counts.running.fetch_sub(1, SeqCst);
                requested.elapsed().as_secs_f64() * 1000.0
            });
        }
    }
    let mut latencies = Vec::new();
    while let Some(result) = requests.join_next().await {
        latencies.push(result.unwrap());
    }
    for owner in owners {
        assert_eq!(owner.await.unwrap(), "completed");
    }
    let wall = start.elapsed().as_secs_f64() * 1000.0;
    stop.store(true, SeqCst);
    let mut jitter = timer.await.unwrap();
    jitter.sort_by(f64::total_cmp);
    latencies.sort_by(f64::total_cmp);
    let mut operation_ms = counts.operation_ms.lock().unwrap();
    operation_ms.sort_by(f64::total_cmp);
    println!(
        "{label},{},{concurrency},{wall:.3},{:.3},{:.3},{:.3},{:.3},{},{},{}",
        if workers { "workers" } else { "inline" },
        latencies[4],
        operation_ms[4],
        jitter[(jitter.len() - 1) * 95 / 100],
        jitter.last().unwrap(),
        jitter.len(),
        counts.peak_running.load(SeqCst),
        counts.peak_queued.load(SeqCst)
    );
    assert_eq!(counts.running.load(SeqCst), 0);
    assert_eq!(counts.queued.load(SeqCst), 0);
    assert!(counts.peak_running.load(SeqCst) <= concurrency);
    assert_eq!(slots.available_permits(), concurrency);
}

async fn lifecycle(fixture: Arc<Fixture>) {
    let slots = Arc::new(Semaphore::new(1));
    let counts = Arc::new(Counts::default());
    let hold = slots.clone().acquire_owned().await.unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (result, owner) = submit(
        fixture.clone(),
        slots.clone(),
        counts.clone(),
        cancel.clone(),
        None,
    );
    tokio::task::yield_now().await;
    cancel.store(true, SeqCst);
    drop(hold);
    assert_eq!(result.await.unwrap(), "cancelled-before-dispatch");
    assert_eq!(owner.await.unwrap(), "cancelled-before-dispatch");
    assert_eq!(counts.peak_running.load(SeqCst), 0);
    eprintln!("queued cancellation: no operation dispatched; completion observed");

    let hold = slots.clone().acquire_owned().await.unwrap();
    let (result, owner) = submit(
        fixture.clone(),
        slots.clone(),
        counts.clone(),
        Arc::new(AtomicBool::new(false)),
        None,
    );
    drop(result);
    drop(hold);
    assert_eq!(owner.await.unwrap(), "completed");
    assert_eq!(slots.available_permits(), 1);
    eprintln!("queued request dropped: operation still completed; supervisor joined");

    for drop_request in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let (release, receive) = std::sync::mpsc::channel();
        let (started, observed_start) = oneshot::channel();
        let gate = Gate {
            started,
            release: receive,
        };
        let (result, owner) = submit(
            fixture.clone(),
            slots.clone(),
            counts.clone(),
            cancel.clone(),
            Some(gate),
        );
        observed_start.await.unwrap();
        cancel.store(true, SeqCst);
        assert_eq!(slots.available_permits(), 0);
        if drop_request {
            drop(result);
            release.send(()).unwrap();
        } else {
            release.send(()).unwrap();
            assert_eq!(result.await.unwrap(), "completed");
        }
        assert_eq!(owner.await.unwrap(), "completed");
        assert_eq!(counts.running.load(SeqCst), 0);
        assert_eq!(slots.available_permits(), 1);
        eprintln!(
            "started cancellation (request dropped={drop_request}): operation completed; supervisor joined; permit returned"
        );
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()
        .unwrap();
    println!(
        "storage_touch,mode,concurrency,batch_ms,request_p50_ms,operation_p50_ms,timer_p95_ms,timer_max_ms,timer_samples,peak_running,peak_queued"
    );
    for packed in [false, true] {
        for workers in [false, true] {
            for concurrency in [1, 4] {
                let fixture = Arc::new(Fixture::new(packed));
                let storage = if packed { "packed" } else { "loose" };
                runtime.block_on(batch(
                    fixture.clone(),
                    workers,
                    concurrency,
                    &format!("{storage}-first-touch"),
                ));
                runtime.block_on(batch(
                    fixture,
                    workers,
                    concurrency,
                    &format!("{storage}-warm"),
                ));
            }
        }
    }
    runtime.block_on(lifecycle(Arc::new(Fixture::new(false))));
}
