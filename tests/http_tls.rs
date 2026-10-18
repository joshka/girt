//! TLS file settings compared with the Git CLI against a loopback HTTPS server that requires a
//! client certificate. OpenSSL generates disposable CAs, a localhost server leaf and a client leaf;
//! no system trust store or real remote is used or modified.
#![cfg(feature = "http")]
#[allow(dead_code)]
#[path = "support/http_git.rs"]
mod http_git;
#[allow(dead_code)]
#[path = "support/pack_git.rs"]
mod pack_git;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use girt::Repository;
use girt::workflow::{Environment, NoCallbacks};
use http_git::Server;
use pack_git::{Fixture, git};
use rstest::rstest;

fn openssl(root: &Path, args: &[&str]) {
    let output = Command::new("openssl")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Leaf certificate `name` signed by `ca`, with the given extended key usage.
fn leaf(root: &Path, name: &str, ca: &str, usage: &str) {
    std::fs::write(
        root.join(format!("{name}.cnf")),
        "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n",
    )
    .unwrap();
    std::fs::write(
        root.join(format!("{name}.ext")),
        format!(
            "subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\n\
             keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage={usage}\n"
        ),
    )
    .unwrap();
    let [cnf, key, csr, ext, pem] =
        ["cnf", "key", "csr", "ext", "pem"].map(|e| format!("{name}.{e}"));
    openssl(
        root,
        &[
            "req", "-new", "-newkey", "rsa:2048", "-nodes", "-config", &cnf, "-keyout", &key,
            "-out", &csr,
        ],
    );
    let [ca_pem, ca_key] = ["pem", "key"].map(|e| format!("{ca}.{e}"));
    openssl(
        root,
        &[
            "x509",
            "-req",
            "-in",
            &csr,
            "-CA",
            &ca_pem,
            "-CAkey",
            &ca_key,
            "-CAcreateserial",
            "-days",
            "1",
            "-extfile",
            &ext,
            "-out",
            &pem,
        ],
    );
}

fn certificate_authority(root: &Path, name: &str) {
    std::fs::write(
        root.join(format!("{name}.cnf")),
        format!(
            "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=Girt {name}\n\
             [ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
        ),
    )
    .unwrap();
    let [cnf, key, pem] = ["cnf", "key", "pem"].map(|e| format!("{name}.{e}"));
    openssl(
        root,
        &[
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", &cnf,
            "-keyout", &key, "-out", &pem,
        ],
    );
}

// Files: ca.pem, server.{pem,key}, client.{pem,key}, combined.pem
// (client certificate and key).
fn certificates(root: &Path) {
    certificate_authority(root, "ca");
    leaf(root, "server", "ca", "serverAuth");
    leaf(root, "client", "ca", "clientAuth");
    let mut combined = std::fs::read(root.join("client.pem")).unwrap();
    combined.extend(std::fs::read(root.join("client.key")).unwrap());
    std::fs::write(root.join("combined.pem"), combined).unwrap();
}

// `{certs}` in settings and environment values becomes the certificate directory.
#[rstest]
#[case::no_client_certificate(&["sslCAInfo={certs}/ca.pem"], &[], false)]
#[case::certificate_and_key(&["sslCAInfo={certs}/ca.pem", "sslCert={certs}/client.pem", "sslKey={certs}/client.key"], &[], true)]
#[case::combined_file(&["sslCAInfo={certs}/ca.pem", "sslCert={certs}/combined.pem"], &[], true)]
#[case::certificate_without_key(&["sslCAInfo={certs}/ca.pem", "sslCert={certs}/client.pem"], &[], false)]
#[case::environment_certificate(&["sslCAInfo={certs}/ca.pem"], &[("GIT_SSL_CERT", "{certs}/combined.pem")], true)]
#[case::environment_beats_setting(&["sslCAInfo={certs}/ca.pem", "sslCert={certs}/combined.pem"], &[("GIT_SSL_CERT", "{certs}/client.pem")], false)]
#[case::environment_replaces_empty_setting(&["sslCAInfo={certs}/ca.pem", "sslCert="], &[("GIT_SSL_CERT", "{certs}/combined.pem")], true)]
#[case::environment_key(&["sslCAInfo={certs}/ca.pem", "sslCert={certs}/client.pem"], &[("GIT_SSL_KEY", "{certs}/client.key")], true)]
fn tls_files_match_git(
    #[case] settings: &[&str],
    #[case] variables: &[(&str, &str)],
    #[case] succeeds: bool,
) {
    let certs = tempfile::tempdir().unwrap();
    certificates(certs.path());
    let source = Fixture::new(girt::ObjectFormat::Sha1, true, 2);
    let server = Server::requiring_client_certificate(
        source.root.path(),
        (
            &certs.path().join("server.pem"),
            &certs.path().join("server.key"),
        ),
        &certs.path().join("ca.pem"),
    );
    let url = server.url.replace("127.0.0.1", "localhost");
    let expand = |value: &str| value.replace("{certs}", certs.path().to_str().unwrap());

    let local = tempfile::tempdir().unwrap();
    git(local.path(), &["init", "--quiet", "."], b"");
    git(local.path(), &["remote", "add", "origin", &url], b"");
    for setting in settings {
        let (name, value) = setting.split_once('=').unwrap();
        let value = &expand(value);
        git(
            local.path(),
            &["config", &format!("http.{name}"), value],
            b"",
        );
    }

    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy().to_ascii_lowercase();
        if key_text.starts_with("git_") || key_text.ends_with("_proxy") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(local.path())
        .env("HOME", local.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", local.path().join("absent-config"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("SSH_AUTH_SOCK")
        .args(["-c", "credential.helper=", "-c", "core.askPass="])
        .args(["ls-remote", "origin"])
        .stdin(Stdio::null());
    let mut environment = Environment::default().with("HOME", local.path());
    for (name, value) in variables {
        command.env(name, expand(value));
        environment = environment.with(*name, PathBuf::from(expand(value)));
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "git: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let repository = Repository::open(local.path()).unwrap();
    let result = girt::workflow::remote_head(&repository, "origin", &environment, &mut NoCallbacks);
    assert_eq!(result.is_ok(), succeeds, "girt: {:?}", result.err());
}

#[test]
fn password_protected_certificates_are_refused() {
    let config = girt::Config::parse(
        b"[remote \"r\"]\nurl = https://localhost/repo\n[http]\nsslCertPasswordProtected = true\n",
    )
    .unwrap();
    let destination = girt::remote::Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &girt::remote::ProtocolEnvironment::default())
        .unwrap();
    let resolved = girt::transport::http::HttpSettings::resolve(
        &config,
        &destination,
        &girt::transport::http::HttpEnvironment::default(),
    );
    assert!(matches!(
        resolved,
        Err(girt::transport::http::HttpError::Configuration(_))
    ));
}
