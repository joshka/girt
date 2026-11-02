//! The `serve-async` bridge serves real Git clients from an async server, stops pack work when
//! the future is dropped, and stalls pack generation for a slow reader.
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

#[allow(dead_code)]
#[path = "../examples/serve_http_axum.rs"]
mod server;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use girt::config::ConfigInputs;
use girt::serve::http::Service;
use girt::serve::{BlockingExecutor, Protocol, ServeError};
use pack_git::{git, git_output};
use tokio::io::AsyncReadExt;

fn pkt(line: &str) -> Vec<u8> {
    let mut bytes = format!("{:04x}", line.len() + 4).into_bytes();
    bytes.extend_from_slice(line.as_bytes());
    bytes
}

fn commit(repo: &Path, name: &str, contents: &[u8]) {
    std::fs::write(repo.join(name), contents).unwrap();
    git(repo, &["add", name], b"");
    git(repo, &["commit", "-q", "--no-gpg-sign", "-m", name], b"");
}

fn rev_parse(repo: &Path, revision: &str) -> String {
    String::from_utf8(git(repo, &["rev-parse", revision], b""))
        .unwrap()
        .trim()
        .to_owned()
}

/// Incompressible bytes, so the pack is as large as its contents.
fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// A bare repository at `root/repo.git` whose `main` holds `files` incompressible files of
/// `size` bytes each.
fn served(root: &Path, files: usize, size: usize) -> PathBuf {
    let work = root.join("work");
    git(root, &["init", "-q", "-b", "main", "work"], b"");
    for index in 0..files {
        std::fs::write(
            work.join(format!("f{index}")),
            noise(index as u64 + 1, size),
        )
        .unwrap();
    }
    git(&work, &["add", "."], b"");
    git(
        &work,
        &["commit", "-q", "--no-gpg-sign", "-m", "files"],
        b"",
    );
    git(root, &["clone", "-q", "--bare", "work", "repo.git"], b"");
    root.join("repo.git")
}

/// Runs each job on its own thread and counts the jobs that have finished.
#[derive(Clone, Default)]
struct Tracked {
    finished: Arc<AtomicUsize>,
}

impl BlockingExecutor for Tracked {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        let finished = self.finished.clone();
        std::thread::spawn(move || {
            job();
            finished.fetch_add(1, Ordering::SeqCst);
        });
    }
}

/// A protocol v2 request for every object reachable from `main`.
fn fetch_request(repo: &Path) -> Vec<u8> {
    let mut request = pkt("command=fetch\n");
    request.extend_from_slice(b"0001");
    request.extend(pkt(&format!("want {}\n", rev_parse(repo, "main"))));
    request.extend(pkt("no-progress\n"));
    request.extend(pkt("done\n"));
    request.extend_from_slice(b"0000");
    request
}

async fn wait_for(condition: impl Fn() -> bool) -> bool {
    for _ in 0..1000 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_clones_and_pushes_concurrently_over_async_http() {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path(), 50, 8 * 1024);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    let app = server::router(root.path().to_owned());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let main = rev_parse(&repo, "main");
    let clients = tokio::task::spawn_blocking({
        let root = root.path().to_owned();
        move || {
            std::thread::scope(|scope| {
                let mut threads = Vec::new();
                for (index, version) in ["0", "2", "0", "2"].into_iter().enumerate() {
                    let (root, url) = (&root, &url);
                    threads.push(scope.spawn(move || {
                        let protocol = format!("protocol.version={version}");
                        let copy = format!("clone{index}");
                        git(root, &["-c", &protocol, "clone", "-q", url, &copy], b"");
                        let copy = root.join(copy);
                        git(&copy, &["fsck", "--strict"], b"");
                        // Each clone pushes its own branch, with a pack large enough to stream.
                        commit(&copy, "pushed", &noise(100 + index as u64, 512 * 1024));
                        let branch = format!("HEAD:refs/heads/client{index}");
                        let pushed = git_output(
                            &copy,
                            &["-c", &protocol, "-c", "http.postBuffer=65536", "push", "-q"]
                                .into_iter()
                                .chain(["origin", branch.as_str()])
                                .collect::<Vec<_>>(),
                            b"",
                        );
                        assert!(
                            pushed.status.success(),
                            "{}",
                            String::from_utf8_lossy(&pushed.stderr)
                        );
                        rev_parse(&copy, "HEAD")
                    }));
                }
                threads
                    .into_iter()
                    .map(|thread| thread.join().unwrap())
                    .collect::<Vec<_>>()
            })
        }
    })
    .await
    .unwrap();

    for (index, pushed) in clients.iter().enumerate() {
        assert_eq!(&rev_parse(&repo, &format!("client{index}")), pushed);
        assert_eq!(rev_parse(&repo, &format!("{pushed}~1")), main);
    }
    git(&repo, &["fsck", "--strict"], b"");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_future_cancels_pack_generation() {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path(), 200, 256 * 1024);
    let executor = Tracked::default();
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let request = fetch_request(&repo);
    let serving = tokio::spawn({
        let (executor, repo) = (executor.clone(), repo.clone());
        async move {
            Service::UploadPack
                .respond_async(
                    executor,
                    repo,
                    ConfigInputs::default(),
                    Protocol::V2,
                    None,
                    &request[..],
                    writer,
                )
                .await
        }
    });

    // Read into the pack, then drop the future while the reader is still open.
    let mut received = vec![0; 256 * 1024];
    reader.read_exact(&mut received).await.unwrap();
    serving.abort();
    assert!(serving.await.unwrap_err().is_cancelled());
    let finished = executor.finished.clone();
    assert!(
        wait_for(|| finished.load(Ordering::SeqCst) == 1).await,
        "the pack job kept running after the future was dropped"
    );
    // The pack (about 50 MiB) was cut short.
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).await.unwrap();
    assert!(
        received.len() + rest.len() < 8 << 20,
        "{} bytes",
        rest.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_reader_stalls_pack_generation() {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path(), 100, 128 * 1024);
    let executor = Tracked::default();
    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let request = fetch_request(&repo);
    let written = Arc::new(Mutex::new(None));
    let serving = tokio::spawn({
        let (executor, repo, written) = (executor.clone(), repo.clone(), written.clone());
        async move {
            let result = Service::UploadPack
                .respond_async(
                    executor,
                    repo,
                    ConfigInputs::default(),
                    Protocol::V2,
                    None,
                    &request[..],
                    writer,
                )
                .await;
            *written.lock().unwrap() = Some(result.is_ok());
            result
        }
    });

    // Without a reader, the job blocks with at most a few chunks in flight.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(executor.finished.load(Ordering::SeqCst), 0);
    assert!(written.lock().unwrap().is_none());

    // Reading slowly still receives the whole pack.
    let mut response = Vec::new();
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() < 1 << 20 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    serving.await.unwrap().unwrap();
    assert!(
        response.len() > 100 * 128 * 1024,
        "{} bytes",
        response.len()
    );
    assert!(response.ends_with(b"0000"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_content_encoding_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path(), 1, 16);
    let result = Service::UploadPack
        .respond_async(
            Tracked::default(),
            repo,
            ConfigInputs::default(),
            Protocol::V2,
            Some(b"br"),
            &b""[..],
            tokio::io::sink(),
        )
        .await;
    assert!(
        matches!(result, Err(ServeError::Unsupported(_))),
        "{result:?}"
    );
}
