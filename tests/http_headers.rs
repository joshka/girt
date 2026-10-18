//! `http.extraHeader` and `http.userAgent` compared with the Git CLI on one loopback listener.
//! The listener records request headers and answers 404, so neither client gets further.
#![cfg(feature = "http")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use girt::Config;
use girt::fetch::{self, FetchLimits};
use girt::remote::{ProtocolEnvironment, Remote};
use girt::transport::TransportControl;
use girt::transport::http::{HttpEnvironment, HttpError, HttpRemote, HttpSettings};
use rstest::rstest;

// Returns the header lines a client sent, keeping only the fields this test configures.
fn recorded(listener: &TcpListener, send: impl FnOnce()) -> Vec<String> {
    let listener = listener.try_clone().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        // Bounded accept: a client that never connects fails the test instead of hanging it.
        let deadline = Instant::now() + Duration::from_secs(20);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "client never connected");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            headers.push(line.trim_end().to_owned());
        }
        (&stream)
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        headers
    });
    send();
    // Header names are case-insensitive; each client's own default agent is not compared.
    server
        .join()
        .unwrap()
        .into_iter()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.to_ascii_lowercase();
            let value = value.trim();
            let default_agent = value.starts_with("git/") || value.starts_with("girt/");
            (name.starts_with("x-")
                || name == "authorization"
                || (name == "user-agent" && !default_agent))
                .then(|| format!("{name}: {value}"))
        })
        .collect()
}

fn girt_headers(listener: &TcpListener, config: &str, agent: Option<&str>) -> Vec<String> {
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    let config =
        Config::parse(format!("[remote \"r\"]\nurl = {url}\n{config}").as_bytes()).unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let environment = HttpEnvironment::default().with_user_agent(agent.map(str::to_owned));
    let remote =
        HttpRemote::configured(HttpSettings::resolve(&config, &destination, &environment).unwrap())
            .unwrap();
    recorded(listener, || {
        let cancel = AtomicBool::new(false);
        let control = TransportControl::new(&cancel)
            .with_deadline(Some(Instant::now() + Duration::from_secs(20)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(fetch::discover_http(
            &remote,
            FetchLimits::default(),
            control,
        ));
        assert!(result.is_err());
    })
}

fn git_headers(listener: &TcpListener, config: &[&str], agent: Option<&str>) -> Vec<String> {
    let url = format!("http://{}/repo.git", listener.local_addr().unwrap());
    let directory = tempfile::tempdir().unwrap();
    let mut args = vec!["-c", "credential.helper=", "-c", "core.askPass="];
    for entry in config {
        args.extend(["-c", entry]);
    }
    args.extend(["-c", "protocol.version=0", "ls-remote", &url]);
    let agent = agent.map(str::to_owned);
    recorded(listener, || {
        let output = git(directory.path(), &args, agent.as_deref());
        assert!(!output.status.success());
    })
}

// Runs Git with isolated configuration and no inherited `GIT_*` or proxy variables.
fn git(directory: &std::path::Path, args: &[&str], agent: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy().to_ascii_lowercase();
        if key_text.starts_with("git_") || key_text.ends_with("_proxy") {
            command.env_remove(key);
        }
    }
    if let Some(agent) = agent {
        command.env("GIT_HTTP_USER_AGENT", agent);
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .args(args)
        .output()
        .unwrap()
}

// Config text for girt and the same entries as `git -c` arguments.
#[rstest]
#[case::several("[http]\nextraHeader = X-A: 1\nextraHeader = X-B: 2\n", &["http.extraHeader=X-A: 1", "http.extraHeader=X-B: 2"], None)]
#[case::empty_resets("[http]\nextraHeader = X-A: 1\nextraHeader =\nextraHeader = X-C: 3\n", &["http.extraHeader=X-A: 1", "http.extraHeader=", "http.extraHeader=X-C: 3"], None)]
#[case::leading_space_and_no_colon("[http]\nextraHeader = X-S:   lead\nextraHeader = nocolon\n", &["http.extraHeader=X-S:   lead", "http.extraHeader=nocolon"], None)]
#[case::authorization("[http]\nextraHeader = Authorization: Bearer token\n", &["http.extraHeader=Authorization: Bearer token"], None)]
#[case::user_agent("[http]\nuserAgent = configured/1\n", &["http.userAgent=configured/1"], None)]
#[case::environment_beats_user_agent("[http]\nuserAgent = configured/1\n", &["http.userAgent=configured/1"], Some("environment/1"))]
#[case::environment_agent("", &[], Some("environment/1"))]
#[case::extra_header_replaces_agent("[http]\nuserAgent = configured/1\nextraHeader = User-Agent: header/1\n", &["http.userAgent=configured/1", "http.extraHeader=User-Agent: header/1"], None)]
fn configured_headers_match_git(
    #[case] config: &str,
    #[case] git_config: &[&str],
    #[case] agent: Option<&str>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    assert_eq!(
        girt_headers(&listener, config, agent),
        git_headers(&listener, git_config, agent)
    );
}

// Scoped entries count only while they match at least as well as the best entry so far.
#[rstest]
#[case::global_then_scoped(&[("", "X-G: g"), ("/repo.git", "X-S: s"), ("/", "X-T: t")])]
#[case::scoped_then_global(&[("/repo.git", "X-S: s"), ("", "X-G: g")])]
#[case::less_then_more_specific(&[("/", "X-T: t"), ("/repo.git", "X-S: s")])]
#[case::scoped_reset(&[("/repo.git", "X-S: s"), ("/repo.git", ""), ("", "X-G: g")])]
fn scoped_extra_headers_match_git(#[case] entries: &[(&str, &str)]) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut config = String::new();
    let mut git_config = Vec::new();
    for (scope, value) in entries {
        if scope.is_empty() {
            config.push_str(&format!("[http]\nextraHeader = {value}\n"));
            git_config.push(format!("http.extraHeader={value}"));
        } else {
            config.push_str(&format!(
                "[http \"{base}{scope}\"]\nextraHeader = {value}\n"
            ));
            git_config.push(format!("http.{base}{scope}.extraHeader={value}"));
        }
    }
    let git_config: Vec<_> = git_config.iter().map(String::as_str).collect();
    assert_eq!(
        girt_headers(&listener, &config, None),
        git_headers(&listener, &git_config, None)
    );
}

// Git sends these fields as given; girt refuses them before any request.
#[rstest]
#[case::host("Host: elsewhere.test")]
#[case::protocol("Git-Protocol: version=0")]
#[case::framing("Transfer-Encoding: chunked")]
fn extra_headers_cannot_override_protocol_fields(#[case] field: &str) {
    let config = Config::parse(
        format!(
            "[remote \"r\"]\nurl = http://127.0.0.1:9/repo.git\n[http]\nextraHeader = {field}\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    let settings =
        HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).unwrap();
    assert!(matches!(
        HttpRemote::configured(settings),
        Err(HttpError::Configuration(_))
    ));
}
