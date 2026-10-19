//! `girt::serve::http` answers smart HTTP as `git http-backend` does, and real Git clients clone,
//! fetch and push through it.
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use girt::config::ConfigInputs;
use girt::serve::Protocol;
use girt::serve::http::{NO_CACHE_HEADERS, Service, request_body};
use pack_git::{git, git_output};

fn pkt(line: &str) -> Vec<u8> {
    let mut bytes = format!("{:04x}", line.len() + 4).into_bytes();
    bytes.extend_from_slice(line.as_bytes());
    bytes
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), name).unwrap();
    git(repo, &["add", name], b"");
    git(repo, &["commit", "-q", "--no-gpg-sign", "-m", name], b"");
}

/// A bare repository with two branches and an annotated tag, under `root/repo.git`.
fn served(root: &Path) -> PathBuf {
    let work = root.join("work");
    git(root, &["init", "-q", "-b", "main", "work"], b"");
    commit(&work, "one");
    commit(&work, "two");
    git(&work, &["tag", "-a", "--no-sign", "-m", "t", "v1"], b"");
    git(&work, &["branch", "side", "HEAD~1"], b"");
    git(root, &["clone", "-q", "--bare", "work", "repo.git"], b"");
    root.join("repo.git")
}

/// Runs `git http-backend` as a CGI program and returns the response body.
fn http_backend(
    root: &Path,
    method: &str,
    path: &str,
    query: &str,
    protocol: &str,
    body: &[u8],
) -> Vec<u8> {
    let service = path.rsplit('/').next().unwrap_or_default();
    let mut child = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REMOTE_USER", "tester")
        .env("REQUEST_METHOD", method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("GIT_PROTOCOL", protocol)
        .env("CONTENT_TYPE", format!("application/x-{service}-request"))
        .env("CONTENT_LENGTH", body.len().to_string())
        .arg("http-backend")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(body).unwrap();
    let output = child.wait_with_output().unwrap().stdout;
    let split = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    output[split + 4..].to_vec()
}

/// Removes the `agent=` capability, which names each implementation.
fn without_agent(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body).into_owned();
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("agent=") {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find([' ', '\n']).unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[rstest::rstest]
#[case::upload_v0("git-upload-pack", "")]
#[case::upload_v2("git-upload-pack", "version=2")]
fn info_refs_matches_git(#[case] service: &str, #[case] protocol: &str) {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path());
    let mut body = Vec::new();
    Service::from_query_value(service.as_bytes())
        .unwrap()
        .advertise(
            &repo,
            &ConfigInputs::default(),
            Protocol::from_parameters(protocol.as_bytes()),
            &mut body,
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected = http_backend(
        root.path(),
        "GET",
        "/repo.git/info/refs",
        &format!("service={service}"),
        protocol,
        b"",
    );
    // Lengths change with the agent string, so compare the text without length prefixes.
    let lines = |body: &[u8]| -> Vec<String> {
        packets(body)
            .iter()
            .map(|line| without_agent(line.as_bytes()))
            .collect()
    };
    assert_eq!(lines(&body), lines(&expected));
}

#[test]
fn receive_pack_info_refs_lists_the_same_refs_as_git() {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path());
    for protocol in ["", "version=2"] {
        let mut body = Vec::new();
        Service::ReceivePack
            .advertise(
                &repo,
                &ConfigInputs::default(),
                Protocol::from_parameters(protocol.as_bytes()),
                &mut body,
                &AtomicBool::new(false),
            )
            .unwrap();
        let expected = http_backend(
            root.path(),
            "GET",
            "/repo.git/info/refs",
            "service=git-receive-pack",
            protocol,
            b"",
        );
        // Capabilities differ while atomic pushes are unadvertised; references and framing match.
        let refs = |body: &[u8]| -> Vec<String> {
            packets(body)
                .into_iter()
                .map(|line| line.split('\0').next().unwrap_or_default().to_owned())
                .collect()
        };
        assert_eq!(refs(&body), refs(&expected), "{protocol:?}");
    }
}

/// The payloads of every pkt-line in `body`, with `0000` for each flush.
fn packets(body: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = body;
    while rest.len() >= 4 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        if length == 0 {
            lines.push("0000".to_owned());
            rest = &rest[4..];
            continue;
        }
        lines.push(String::from_utf8_lossy(&rest[4..length]).into_owned());
        rest = &rest[length..];
    }
    lines
}

fn rev_parse(repo: &Path, revision: &str) -> String {
    String::from_utf8(git(repo, &["rev-parse", revision], b""))
        .unwrap()
        .trim()
        .to_owned()
}

/// Packets up to the first pack data, without progress.
fn transcript(response: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = response;
    while rest.len() >= 4 {
        let length = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
        if length == 0 {
            lines.push("0000".to_owned());
            rest = &rest[4..];
            continue;
        }
        let payload = &rest[4..length];
        rest = &rest[length..];
        match payload.first() {
            Some(1) => {
                lines.push("<pack>".to_owned());
                break;
            }
            Some(2) => continue,
            _ => lines.push(String::from_utf8_lossy(payload).into_owned()),
        }
    }
    lines
}

#[rstest::rstest]
#[case::round("U", false, false)]
#[case::round_with_common("main~1", false, true)]
#[case::done("main~1", true, false)]
#[case::common_without_no_done("main~1", false, false)]
#[case::shallow_with_no_done("main", false, true)]
fn stateless_v0_fetch_matches_git(#[case] have: &str, #[case] done: bool, #[case] no_done: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path());
    let have = if have == "U" {
        "1".repeat(40)
    } else {
        rev_parse(&repo, have)
    };
    let mut capabilities = "multi_ack_detailed side-band-64k".to_owned();
    if no_done {
        capabilities.push_str(" no-done");
    }
    let mut request = pkt(&format!(
        "want {} {capabilities}\n",
        rev_parse(&repo, "main")
    ));
    request.extend_from_slice(b"0000");
    request.extend(pkt(&format!("have {have}\n")));
    if done {
        request.extend(pkt("done\n"));
    } else {
        request.extend_from_slice(b"0000");
    }
    let mut response = Vec::new();
    Service::UploadPack
        .respond(
            &repo,
            &ConfigInputs::default(),
            Protocol::V0,
            &mut request.as_slice(),
            &mut response,
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected = http_backend(
        root.path(),
        "POST",
        "/repo.git/git-upload-pack",
        "",
        "",
        &request,
    );
    assert_eq!(transcript(&response), transcript(&expected));
}

/// A minimal HTTP/1.1 server for the test: one request per connection, routed to `serve::http`.
struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
    /// Requests whose handling panicked.
    failed: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(root: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let failed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failures = failed.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        // A stalled client can't keep the server from stopping.
                        let timeout = Some(std::time::Duration::from_secs(10));
                        stream.set_read_timeout(timeout).unwrap();
                        stream.set_write_timeout(timeout).unwrap();
                        let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle(&root, stream);
                        }));
                        if served.is_err() {
                            failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        Self {
            port,
            stop,
            failed,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/repo.git", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(root: &Path, stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let mut body = Vec::new();
    if header("transfer-encoding").is_some_and(|value| value.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else if let Some(length) = header("content-length") {
        body.resize(length.parse().unwrap(), 0);
        reader.read_exact(&mut body).unwrap();
    }
    let protocol = Protocol::from_parameters(header("git-protocol").unwrap_or_default().as_bytes());
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let repo = root.join(
        path.trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default(),
    );
    let cancel = AtomicBool::new(false);
    let mut response = Vec::new();
    let content_type = if method == "GET" && path.ends_with("/info/refs") {
        let service = Service::from_query_value(
            query
                .strip_prefix("service=")
                .unwrap_or_default()
                .as_bytes(),
        )
        .unwrap();
        service
            .advertise(
                &repo,
                &ConfigInputs::default(),
                protocol,
                &mut response,
                &cancel,
            )
            .unwrap();
        service.advertisement_content_type()
    } else {
        let service =
            Service::from_query_value(path.rsplit('/').next().unwrap_or_default().as_bytes())
                .unwrap();
        let encoding = header("content-encoding");
        let mut decoded =
            request_body(encoding.as_deref().map(str::as_bytes), body.as_slice()).unwrap();
        service
            .respond(
                &repo,
                &ConfigInputs::default(),
                protocol,
                &mut decoded,
                &mut response,
                &cancel,
            )
            .unwrap();
        service.result_content_type()
    };
    let mut stream = stream;
    let mut head = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n");
    for (name, value) in NO_CACHE_HEADERS {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    ));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(&response).unwrap();
}

fn refs(repo: &Path) -> String {
    String::from_utf8(git(
        repo,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
        b"",
    ))
    .unwrap()
}

#[rstest::rstest]
fn git_clones_fetches_and_pushes_over_http(#[values("0", "2")] version: &str) {
    let root = tempfile::tempdir().unwrap();
    let repo = served(root.path());
    let server = Server::start(root.path().to_owned());
    let protocol = format!("protocol.version={version}");
    git(
        root.path(),
        &["-c", &protocol, "clone", "-q", &server.url(), "copy"],
        b"",
    );
    let copy = root.path().join("copy");
    git(&copy, &["fsck", "--strict"], b"");
    assert_eq!(rev_parse(&copy, "origin/main"), rev_parse(&repo, "main"));

    // A large push makes the client send a chunked request.
    std::fs::write(copy.join("big"), vec![b'x'; 2 << 20]).unwrap();
    git(&copy, &["add", "big"], b"");
    git(&copy, &["commit", "-q", "--no-gpg-sign", "-m", "big"], b"");
    let pushed = git_output(
        &copy,
        &[
            "-c",
            &protocol,
            "-c",
            "http.postBuffer=65536",
            "push",
            "-q",
            "origin",
            "main",
        ],
        b"",
    );
    assert!(
        pushed.status.success(),
        "{}",
        String::from_utf8_lossy(&pushed.stderr)
    );
    assert_eq!(rev_parse(&repo, "main"), rev_parse(&copy, "main"));
    git(&repo, &["fsck", "--strict"], b"");

    // Fetch new history pushed by someone else.
    let work = root.path().join("work");
    git(
        &work,
        &["pull", "-q", "--no-rebase", repo.to_str().unwrap(), "main"],
        b"",
    );
    commit(&work, "three");
    git(&work, &["push", "-q", repo.to_str().unwrap(), "main"], b"");
    git(&copy, &["-c", &protocol, "fetch", "-q", "origin"], b"");
    assert_eq!(rev_parse(&copy, "origin/main"), rev_parse(&repo, "main"));
    assert!(refs(&copy).contains("refs/tags/v1"));
    assert_eq!(server.failed.load(Ordering::Relaxed), 0);
    drop(server);
}
