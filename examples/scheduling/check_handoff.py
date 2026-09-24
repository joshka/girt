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
OWNED = '''
pub async fn handoff(remote: &REMOTE, known: Arc<KnownHistory>, tip: ObjectId) -> Result<ReceivedFetch, FetchError> {
    let download = {
        let cancel = AtomicBool::new(false);
        let download: TYPE = RECEIVE(remote, |_| vec![tip], Some(Arc::clone(&known)), FetchLimits::default(), TransportControl::new(&cancel)).await?;
        drop(known);
        download
    };
    tokio::task::spawn_blocking(move || {
        download.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
    }).await.unwrap()
}
pub async fn full(remote: &REMOTE, tip: ObjectId) -> Result<ReceivedFetch, FetchError> {
    let cancel = AtomicBool::new(false);
    let download = RECEIVE(remote, |_| vec![tip], None, FetchLimits::default(), TransportControl::new(&cancel)).await?;
    tokio::task::spawn_blocking(move || {
        download.validate(&AtomicBool::new(false), |_| ControlFlow::Continue(()))
    }).await.unwrap()
}
pub fn traits() {
    fn send_sync_static<T: Send + Sync + 'static>() {}
    send_sync_static::<TYPE>();
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
        source = COMMON + OWNED
        for key, value in [("TYPE", fetch), ("RECEIVE", f"receive_{module}"), ("MODULE", module), ("REMOTE", remote)]:
            source = source.replace(key, value)
        (crate / "src/lib.rs").write_text(source)
        result = subprocess.run(["cargo", "check", "--offline", "--manifest-path", str(crate / "Cargo.toml")], env=env, text=True, capture_output=True)
        assert result.returncode == 0, result.stderr
        assert "warning:" not in result.stderr, result.stderr
        print(f"{fetch}/owned-history and no-history spawn_blocking handoffs: compiled")
