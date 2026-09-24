#!/usr/bin/env python3
"""Compile actual fetch handoffs in a disposable crate; no network calls execute."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
COMMON = '''
use girt::{ObjectId, Objects};
use std::ops::ControlFlow;
use std::sync::{Arc, atomic::AtomicBool};
use girt::fetch::{FetchError, FetchLimits, KnownHistory, ReceivedFetch, TYPE, RECEIVE};
use girt::transport::{TransportControl, MODULE::REMOTE};
pub fn retain_history(objects: &Objects, tip: ObjectId) -> Result<Arc<KnownHistory>, FetchError> {
    KnownHistory::new(objects, &[tip], FetchLimits::default(), &AtomicBool::new(false)).map(Arc::new)
}
'''
BORROWED = '''
pub async fn handoff(download: TYPE<'_>) -> Result<ReceivedFetch, FetchError> {
    tokio::task::spawn_blocking(move || {
        download.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
    }).await.unwrap()
}
// Keep imports exercised and check Send independently from 'static.
pub fn send_sync<T: Send + Sync>() {}
pub fn traits() { send_sync::<TYPE<'_>>(); }
pub async fn download(remote: &REMOTE, known: &KnownHistory, tip: ObjectId) {
    let cancel = AtomicBool::new(false);
    let _ = RECEIVE(remote, |_| vec![tip], known, FetchLimits::default(), TransportControl::new(&cancel)).await;
    let _ = Arc::new(KnownHistory::default());
}
'''
ARC = '''
pub async fn handoff(remote: &REMOTE, known: Arc<KnownHistory>, tip: ObjectId) -> Result<ReceivedFetch, FetchError> {
    let cancel = AtomicBool::new(false);
    let download: TYPE<'_> = RECEIVE(remote, |_| vec![tip], &known, FetchLimits::default(), TransportControl::new(&cancel)).await?;
    let retained = known.clone();
    tokio::task::spawn_blocking(move || {
        let _retained = retained;
        download.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
    }).await.unwrap()
}
'''
WORKAROUNDS = '''
// Own the history before borrowing it INSIDE the blocking closure. This occupies a worker
// throughout network waiting, and requires the outer runtime to remain driven until completion.
pub async fn whole_operation(remote: REMOTE, known: Arc<KnownHistory>, tip: ObjectId) -> Result<ReceivedFetch, FetchError> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let cancel = AtomicBool::new(false);
        runtime.block_on(async {
            let download: TYPE<'_> = RECEIVE(&remote, |_| vec![tip], &known, FetchLimits::default(), TransportControl::new(&cancel)).await?;
            download.validate(&cancel, |_| ControlFlow::Continue(()))
        })
    }).await.unwrap()
}
// Scoped borrowing compiles but scope synchronously joins, blocking its calling executor.
pub fn scoped(download: TYPE<'_>) -> Result<ReceivedFetch, FetchError> {
    std::thread::scope(|scope| scope.spawn(move || {
        download.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
    }).join().unwrap())
}
'''

with tempfile.TemporaryDirectory(prefix="girt-handoff-") as temporary:
    crate = Path(temporary)
    (crate / "src").mkdir()
    shutil.copyfile(ROOT / "Cargo.lock", crate / "Cargo.lock")
    (crate / "Cargo.toml").write_text(f'''[package]
name = "girt-handoff-probe"
version = "0.0.0"
edition = "2024"
[dependencies]
girt = {{ path = "{ROOT}", features = ["http", "ssh"] }}
tokio = {{ version = "1.4", features = ["rt"] }}
''')
    env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target/handoff-probe"))
    for module, remote, fetch in [("http", "HttpRemote", "HttpFetch"), ("ssh", "SshRemote", "SshFetch")]:
        for name, source, expected in [("borrowed", BORROWED, "E0521"), ("external-arc", ARC, "E0597"), ("workarounds", WORKAROUNDS, None)]:
            source = COMMON + source
            for key, value in [("TYPE", fetch), ("RECEIVE", f"receive_{module}"), ("MODULE", module), ("REMOTE", remote)]:
                source = source.replace(key, value)
            (crate / "src/lib.rs").write_text(source)
            result = subprocess.run(["cargo", "check", "--offline", "--manifest-path", str(crate / "Cargo.toml")], env=env, text=True, capture_output=True)
            if expected:
                assert result.returncode != 0 and f"error[{expected}]" in result.stderr, result.stderr
                errors = [line for line in result.stderr.splitlines() if line.startswith("error[")]
                print(f"{fetch}/{name}: expected rejection: {'; '.join(errors)}")
            else:
                assert result.returncode == 0, result.stderr
                print(f"{fetch}/{name}: compiled")
            assert "warning:" not in result.stderr, result.stderr
