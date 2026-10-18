//! Pure HTTP policy selection; callers own environment capture and CA file reads.

use reqwest::Url;

use super::{CertificateCheck, HttpError};
use crate::Config;
use crate::config::boolean;
use crate::remote::{Destination, Protocol};

/// Application-supplied HTTP inputs. Girt does not read Git/proxy environment variables.
///
/// Fill it from the process environment the application chose to honor: `GIT_SSL_CAINFO` (read
/// the file yourself), `GIT_SSL_NO_VERIFY`, `https_proxy` and `no_proxy`. [`crate::workflow`]
/// does this from its [`Environment`](crate::workflow::Environment). The default uses platform
/// trust, no proxy, and refuses to disable TLS verification.
///
/// # Examples
///
/// ```
/// use girt::transport::http::HttpEnvironment;
///
/// let environment = HttpEnvironment::default()
///     .with_proxy(std::env::var("https_proxy").ok())
///     .with_no_proxy(std::env::var("no_proxy").ok());
/// assert!(!environment.allow_insecure_tls);
/// ```
///
/// Default trust uses the TLS backend's platform verifier, which may consult OS trust settings
/// and native certificate environment variables. This is not a Git/libcurl backend selector.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct HttpEnvironment {
    /// Selected CA bundle content, read and bounded by the application (at most 1 MiB).
    ///
    /// Select `GIT_SSL_CAINFO` first, otherwise [`HttpSettings::configured_ca_info`]. A present
    /// bundle replaces platform roots for this connection. It must contain certificates.
    pub ssl_ca_info: Option<Vec<u8>>,
    /// Selected client certificate PEM, from `GIT_SSL_CERT` or [`TlsPaths::certificate`].
    ///
    /// Without [`Self::ssl_key`], the file must also hold the private key, as Git allows.
    /// Together with the key it is limited to 1 MiB.
    pub ssl_cert: Option<Vec<u8>>,
    /// Selected client private key PEM, from `GIT_SSL_KEY` or [`TlsPaths::key`].
    pub ssl_key: Option<Vec<u8>>,
    /// `GIT_SSL_NO_VERIFY`; disabling verification also requires `allow_insecure_tls`.
    pub ssl_no_verify: Option<bool>,
    /// `https_proxy`, `http_proxy`, or `all_proxy` selected by the application.
    /// Configured HTTP and per-remote proxies take precedence; an empty configured proxy disables
    /// it.
    pub proxy: Option<String>,
    /// `no_proxy` selected by the application.
    pub no_proxy: Option<String>,
    /// Approved Basic credentials for the selected proxy, never parsed from a proxy URL.
    pub proxy_credentials: Option<(String, String)>,
    /// Explicit application authorization for disabling certificate and hostname checks.
    pub allow_insecure_tls: bool,
    /// The application's decision about each server certificate, see [`CertificateCheck`].
    ///
    /// Without one, girt's own verification decides.
    pub certificate_check: Option<std::sync::Arc<dyn CertificateCheck>>,
    /// `GIT_HTTP_USER_AGENT` selected by the application. It takes precedence over
    /// `http.userAgent`.
    pub user_agent: Option<String>,
}

impl HttpEnvironment {
    /// Sets [`Self::ssl_ca_info`].
    #[must_use]
    pub fn with_ssl_ca_info(mut self, ssl_ca_info: Option<Vec<u8>>) -> Self {
        self.ssl_ca_info = ssl_ca_info;
        self
    }

    /// Sets [`Self::ssl_cert`] and [`Self::ssl_key`].
    #[must_use]
    pub fn with_ssl_client_certificate(
        mut self,
        ssl_cert: Option<Vec<u8>>,
        ssl_key: Option<Vec<u8>>,
    ) -> Self {
        self.ssl_cert = ssl_cert;
        self.ssl_key = ssl_key;
        self
    }

    /// Sets [`Self::ssl_no_verify`].
    #[must_use]
    pub fn with_ssl_no_verify(mut self, ssl_no_verify: Option<bool>) -> Self {
        self.ssl_no_verify = ssl_no_verify;
        self
    }

    /// Sets [`Self::proxy`].
    #[must_use]
    pub fn with_proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy = proxy;
        self
    }

    /// Sets [`Self::no_proxy`].
    #[must_use]
    pub fn with_no_proxy(mut self, no_proxy: Option<String>) -> Self {
        self.no_proxy = no_proxy;
        self
    }

    /// Sets [`Self::proxy_credentials`].
    #[must_use]
    pub fn with_proxy_credentials(mut self, proxy_credentials: Option<(String, String)>) -> Self {
        self.proxy_credentials = proxy_credentials;
        self
    }

    /// Sets [`Self::certificate_check`].
    #[must_use]
    pub fn with_certificate_check(
        mut self,
        certificate_check: Option<std::sync::Arc<dyn CertificateCheck>>,
    ) -> Self {
        self.certificate_check = certificate_check;
        self
    }

    /// Sets [`Self::user_agent`].
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: Option<String>) -> Self {
        self.user_agent = user_agent;
        self
    }

    /// Sets [`Self::allow_insecure_tls`].
    #[must_use]
    pub fn with_allow_insecure_tls(mut self, allow_insecure_tls: bool) -> Self {
        self.allow_insecure_tls = allow_insecure_tls;
        self
    }
}

impl std::fmt::Debug for HttpEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEnvironment").finish_non_exhaustive()
    }
}

/// Configured TLS file paths for one destination, selected from `http.<url>.*` settings.
///
/// girt opens no files: the application checks for the `GIT_SSL_CAINFO`, `GIT_SSL_CERT` and
/// `GIT_SSL_KEY` environment variables first (each takes precedence over its setting), and only
/// for a setting it actually uses refuses an empty or NUL-containing path, expands Git path
/// syntax, reads a bounded file, and passes the bytes in [`HttpEnvironment`]. A setting written
/// without a value is returned as an empty path. Paths may be private and should not be logged
/// automatically. `http.sslCAPath` is not supported yet and fails resolution.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TlsPaths<'a> {
    /// `http.sslCAInfo`: a CA bundle that replaces platform trust.
    pub ca_info: Option<&'a [u8]>,
    /// `http.sslCert`: the client certificate.
    pub certificate: Option<&'a [u8]>,
    /// `http.sslKey`: the client private key.
    pub key: Option<&'a [u8]>,
}

/// Resolved HTTP policy for one destination. Its Debug output omits URLs and secrets.
///
/// Resolution reads `http.<url>.*` settings scoped to the destination's URL, and fails on settings
/// girt does not implement, such as client certificates, rather than ignoring them. See
/// [`HttpRemote`](super::HttpRemote) for an example.
///
/// # Examples
///
/// Configuration that weakens TLS needs the application's explicit approval. Approving it disables
/// certificate and hostname checks, so anyone on the network path can impersonate the server and
/// read credentials. It is an exceptional trust decision, not a fix for certificate errors: to
/// trust a private certificate authority while keeping verification, pass its certificates in
/// [`HttpEnvironment::ssl_ca_info`] (see [`HttpSettings::configured_ca_info`]). The approved call
/// below only demonstrates the gate:
///
/// ```
/// use girt::Config;
/// use girt::remote::{ProtocolEnvironment, Remote};
/// use girt::transport::http::{HttpEnvironment, HttpError, HttpSettings};
///
/// let config = Config::parse(
///     b"[http]\nsslVerify = false\n[remote \"origin\"]\nurl = https://example.com/project.git\n",
/// )?;
/// let origin = Remote::find(&config, b"origin")?.expect("configured");
/// let destination = origin.fetch_destination(&config, &ProtocolEnvironment::default())?;
///
/// let refused = HttpSettings::resolve(&config, &destination, &HttpEnvironment::default());
/// assert!(matches!(refused, Err(HttpError::Configuration(_))));
///
/// let approved = HttpEnvironment::default().with_allow_insecure_tls(true);
/// assert!(HttpSettings::resolve(&config, &destination, &approved).is_ok());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct HttpSettings {
    pub(super) url: String,
    pub(super) roots: Vec<Vec<u8>>,
    pub(super) client_identity: Option<Vec<u8>>,
    pub(super) certificate_check: Option<std::sync::Arc<dyn CertificateCheck>>,
    pub(super) proxy: Option<String>,
    pub(super) no_proxy: Option<String>,
    pub(super) proxy_credentials: Option<(String, String)>,
    pub(super) verify_tls: bool,
    pub(super) follow_initial_redirect: bool,
    pub(super) user_agent: Option<String>,
    pub(super) extra_headers: Vec<(String, String)>,
}

impl std::fmt::Debug for HttpSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSettings").finish_non_exhaustive()
    }
}

impl HttpSettings {
    /// Resolves HTTP policy for a destination without a named remote.
    ///
    /// Environment CA bytes and verification override configuration. Configured proxies override
    /// the environment proxy, including an empty value that disables proxying. Use
    /// [`Self::resolve_for_remote`] when a named remote supplies proxy policy.
    ///
    /// URL sections match scheme, host (including whole-label `*`), effective port and
    /// component-bounded path. More specific hosts, then longer paths, then later entries win.
    /// Only `initial` and false redirect policies are supported. The default is `initial`.
    ///
    /// # Errors
    ///
    /// Rejects unsupported HTTP settings, malformed policy, configured CA paths without supplied
    /// bytes, and disabled verification without application approval. No file or network I/O
    /// occurs.
    pub fn resolve(
        config: &Config,
        destination: &Destination,
        environment: &HttpEnvironment,
    ) -> Result<Self, HttpError> {
        Self::resolve_policy(config, destination, environment, None)
    }

    /// Resolves HTTP policy including `remote.<name>.proxy` and proxy authentication policy.
    ///
    /// The caller supplies the selected remote's exact name and its resolved destination.
    /// Remote proxy configuration overrides URL-scoped/global HTTP configuration and environment;
    /// an empty value explicitly disables proxying. Only approved Basic proxy credentials are
    /// supported, and proxy URLs must omit userinfo.
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Self::resolve`], including unsupported proxy authentication.
    pub fn resolve_for_remote(
        config: &Config,
        name: &[u8],
        destination: &Destination,
        environment: &HttpEnvironment,
    ) -> Result<Self, HttpError> {
        Self::resolve_policy(config, destination, environment, Some(name))
    }

    /// Returns the selected `http.sslCAInfo` path bytes without opening the file.
    ///
    /// Call only when `GIT_SSL_CAINFO` is absent; that environment path takes precedence.
    /// The caller expands Git path syntax, resolves relative paths against its working directory,
    /// authorizes the read, and loads at most 1 MiB into [`HttpEnvironment::ssl_ca_info`].
    /// Returned bytes may contain private paths and must not be logged automatically.
    ///
    /// # Errors
    ///
    /// Rejects a non-HTTP destination or an implicit, empty, or NUL-containing CA path.
    ///
    /// # Examples
    ///
    /// `http.<url>.*` settings apply only to matching URLs. The application reads the file itself
    /// and passes its bytes in [`HttpEnvironment::ssl_ca_info`]:
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::{ProtocolEnvironment, Remote};
    /// use girt::transport::http::HttpSettings;
    ///
    /// let config = Config::parse(
    ///     b"[http \"https://example.com/\"]\nsslCAInfo = /etc/ssl/example-ca.pem\n\
    ///       [remote \"origin\"]\nurl = https://example.com/project.git\n",
    /// )?;
    /// let origin = Remote::find(&config, b"origin")?.expect("configured");
    /// let destination = origin.fetch_destination(&config, &ProtocolEnvironment::default())?;
    /// assert_eq!(
    ///     HttpSettings::configured_ca_info(&config, &destination)?,
    ///     Some(&b"/etc/ssl/example-ca.pem"[..]),
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn configured_ca_info<'a>(
        config: &'a Config,
        destination: &Destination,
    ) -> Result<Option<&'a [u8]>, HttpError> {
        let target = destination_url(destination)?;
        http_value(config, &target, "sslcainfo")
            .map(|value| {
                value
                    .filter(|path| !path.is_empty() && !path.contains(&0))
                    .ok_or(HttpError::Configuration("TLS CA path"))
            })
            .transpose()
    }

    /// Returns the configured TLS file paths for a destination without opening them.
    ///
    /// See [`TlsPaths`] for how the application turns them into [`HttpEnvironment`] inputs.
    ///
    /// # Errors
    ///
    /// Rejects a non-HTTP destination.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// use girt::remote::{ProtocolEnvironment, Remote};
    /// use girt::transport::http::HttpSettings;
    ///
    /// let config = Config::parse(
    ///     b"[http]\nsslCert = ~/client.pem\nsslKey = ~/client.key\n\
    ///       [remote \"origin\"]\nurl = https://example.com/project.git\n",
    /// )?;
    /// let origin = Remote::find(&config, b"origin")?.expect("configured");
    /// let destination = origin.fetch_destination(&config, &ProtocolEnvironment::default())?;
    /// let paths = HttpSettings::configured_tls_paths(&config, &destination)?;
    /// assert_eq!(paths.certificate, Some(&b"~/client.pem"[..]));
    /// assert_eq!(paths.ca_info, None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn configured_tls_paths<'a>(
        config: &'a Config,
        destination: &Destination,
    ) -> Result<TlsPaths<'a>, HttpError> {
        let target = destination_url(destination)?;
        let path = |name| http_value(config, &target, name).map(|value| value.unwrap_or_default());
        Ok(TlsPaths {
            ca_info: path("sslcainfo"),
            certificate: path("sslcert"),
            key: path("sslkey"),
        })
    }

    fn resolve_policy(
        config: &Config,
        destination: &Destination,
        environment: &HttpEnvironment,
        remote: Option<&[u8]>,
    ) -> Result<Self, HttpError> {
        let target = destination_url(destination)?;
        let url = std::str::from_utf8(destination.bytes())
            .expect("validated URL")
            .to_owned();
        reject_unsupported(config, &target)?;
        let configured_verify = http_value(config, &target, "sslverify")
            .map(|value| boolean(value).ok_or(HttpError::Configuration("TLS verification")))
            .transpose()?;
        let verify_tls = environment
            .ssl_no_verify
            .map(|disabled| !disabled)
            .unwrap_or(configured_verify.unwrap_or(true));
        if !verify_tls && !environment.allow_insecure_tls {
            return Err(HttpError::Configuration(
                "insecure TLS requires application approval",
            ));
        }
        let ca_configured = http_value(config, &target, "sslcainfo").is_some();
        if ca_configured && environment.ssl_ca_info.is_none() {
            return Err(HttpError::Configuration(
                "TLS CA file requires application input",
            ));
        }
        if environment
            .ssl_ca_info
            .as_ref()
            .is_some_and(|ca| ca.len() > 1024 * 1024)
        {
            return Err(HttpError::Limit("trust roots"));
        }
        // Only presence matters here: the application may have replaced a setting with its
        // environment variable, so the setting's value is not validated.
        let paths = Self::configured_tls_paths(config, destination)?;
        if (paths.certificate.is_some() && environment.ssl_cert.is_none())
            || (paths.key.is_some() && environment.ssl_key.is_none())
        {
            return Err(HttpError::Configuration(
                "TLS client certificate requires application input",
            ));
        }
        let password_protected = http_value(config, &target, "sslcertpasswordprotected")
            .map(|value| boolean(value).ok_or(HttpError::Configuration("TLS certificate password")))
            .transpose()?;
        if password_protected == Some(true) {
            return Err(HttpError::Configuration(
                "password-protected TLS client certificates are unsupported",
            ));
        }
        let client_identity = match (&environment.ssl_cert, &environment.ssl_key) {
            (None, None) => None,
            (None, Some(_)) => {
                return Err(HttpError::Configuration(
                    "TLS client key without certificate",
                ));
            }
            (Some(certificate), key) => {
                let length = certificate
                    .len()
                    .saturating_add(key.as_ref().map_or(0, Vec::len));
                if length > 1024 * 1024 {
                    return Err(HttpError::Limit("TLS client certificate"));
                }
                let mut identity = certificate.clone();
                if let Some(key) = key {
                    identity.push(b'\n');
                    identity.extend_from_slice(key);
                }
                Some(identity)
            }
        };
        let roots = environment.ssl_ca_info.iter().cloned().collect();
        let follow_initial_redirect = match http_value(config, &target, "followredirects") {
            None | Some(Some(b"initial")) => true,
            Some(value) if boolean(value) == Some(false) => false,
            _ => return Err(HttpError::Configuration("redirect policy")),
        };
        let proxy_auth = remote
            .and_then(|name| remote_value(config, name, "proxyauthmethod"))
            .or_else(|| http_value(config, &target, "proxyauthmethod"));
        if proxy_auth.is_some_and(|value| value != Some(b"basic")) {
            return Err(HttpError::Configuration("unsupported proxy authentication"));
        }
        let configured_proxy = remote
            .and_then(|name| remote_value(config, name, "proxy"))
            .or_else(|| http_value(config, &target, "proxy"));
        let proxy = match configured_proxy {
            Some(value) => Some(
                std::str::from_utf8(value.ok_or(HttpError::Configuration("proxy"))?)
                    .map_err(|_| HttpError::Configuration("proxy URL encoding"))?
                    .to_owned(),
            ),
            None => environment.proxy.clone(),
        };
        let proxy = proxy.filter(|value| !value.is_empty());
        if let Some(value) = &proxy {
            if value.len() > 8192 {
                return Err(HttpError::Limit("proxy URL"));
            }
            let parsed = Url::parse(value).map_err(|_| HttpError::Configuration("proxy URL"))?;
            if !matches!(parsed.scheme(), "http")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || parsed.path() != "/"
                || value.contains('@')
                || value
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            {
                return Err(HttpError::Configuration("proxy URL"));
            }
        }
        let user_agent = match (
            &environment.user_agent,
            http_value(config, &target, "useragent"),
        ) {
            (Some(agent), _) => Some(agent.clone()),
            (None, Some(value)) => Some(
                std::str::from_utf8(value.ok_or(HttpError::Configuration("user agent"))?)
                    .map_err(|_| HttpError::Configuration("user agent encoding"))?
                    .to_owned(),
            ),
            (None, None) => None,
        };
        Ok(Self {
            url,
            roots,
            client_identity,
            certificate_check: environment.certificate_check.clone(),
            proxy,
            no_proxy: environment.no_proxy.clone(),
            proxy_credentials: environment.proxy_credentials.clone(),
            verify_tls,
            follow_initial_redirect,
            user_agent,
            extra_headers: extra_headers(config, &target)?,
        })
    }
}

fn http_value<'a>(config: &'a Config, target: &Url, name: &str) -> Option<Option<&'a [u8]>> {
    let mut selected = None;
    let mut best = (0, 0);
    let mut scoped = false;
    for entry in config.entries() {
        if !entry.section.eq_ignore_ascii_case(b"http")
            || !entry.name.eq_ignore_ascii_case(name.as_bytes())
        {
            continue;
        }
        let Some(scope) = entry.subsection.as_deref() else {
            if !scoped {
                selected = Some(entry.value.as_deref());
            }
            continue;
        };
        let Some(score) = scope_score(scope, target) else {
            continue;
        };
        if score >= best {
            selected = Some(entry.value.as_deref());
            best = score;
            scoped = true;
        }
    }
    selected
}

// `http.extraHeader` is multi-valued: an entry counts when its URL scope matches at least as well
// as the best one seen so far, and an empty value clears the fields collected before it. Git
// ignores a field without a colon; girt does too.
fn extra_headers(config: &Config, target: &Url) -> Result<Vec<(String, String)>, HttpError> {
    let mut headers = Vec::new();
    let mut best = None;
    for entry in config.entries() {
        if !entry.section.eq_ignore_ascii_case(b"http")
            || !entry.name.eq_ignore_ascii_case(b"extraheader")
        {
            continue;
        }
        let score = match entry.subsection.as_deref() {
            None => Some((0, 0)),
            Some(scope) => scope_score(scope, target),
        };
        let Some(score) = score else {
            continue;
        };
        if best.is_some_and(|best| score < best) {
            continue;
        }
        best = Some(score);
        let value = entry
            .value
            .as_deref()
            .ok_or(HttpError::Configuration("extra header"))?;
        if value.is_empty() {
            headers.clear();
            continue;
        }
        let value = std::str::from_utf8(value)
            .map_err(|_| HttpError::Configuration("extra header encoding"))?;
        if let Some((name, field)) = value.split_once(':') {
            headers.push((name.to_owned(), field.trim_start().to_owned()));
        }
    }
    Ok(headers)
}

// Scores a `http.<url>` scope against the target: host specificity, then path length. `None` when
// the scope does not apply.
fn scope_score(scope: &[u8], target: &Url) -> Option<(usize, usize)> {
    let scope = Url::parse(std::str::from_utf8(scope).ok()?).ok()?;
    let host_score = host_match(scope.host_str(), target.host_str())?;
    let scope_path = normalized_path(scope.path());
    let target_path = normalized_path(target.path());
    let path = scope_path.strip_suffix('/').unwrap_or(&scope_path);
    (scope.scheme() == target.scheme()
        && scope.port_or_known_default() == target.port_or_known_default()
        && scope.username().is_empty()
        && scope.query().is_none()
        && scope.fragment().is_none()
        && target_path.starts_with(path)
        && (target_path.len() == path.len()
            || target_path.as_bytes().get(path.len()) == Some(&b'/')))
    .then_some((host_score, path.len()))
}

fn destination_url(destination: &Destination) -> Result<Url, HttpError> {
    if !matches!(destination.protocol(), Protocol::Http | Protocol::Https) {
        return Err(HttpError::Configuration("HTTP destination"));
    }
    let url = std::str::from_utf8(destination.bytes())
        .map_err(|_| HttpError::Configuration("repository URL encoding"))?;
    Url::parse(url).map_err(|_| HttpError::Configuration("repository URL"))
}

fn remote_value<'a>(config: &'a Config, remote: &[u8], name: &str) -> Option<Option<&'a [u8]>> {
    config
        .entries()
        .iter()
        .rev()
        .find(|entry| {
            entry.section.eq_ignore_ascii_case(b"remote")
                && entry.subsection.as_deref() == Some(remote)
                && entry.name.eq_ignore_ascii_case(name.as_bytes())
        })
        .map(|entry| entry.value.as_deref())
}

// Settings that only tune Git's libcurl backend, with nothing to change in girt's rustls/reqwest
// stack and no effect on what is trusted or where requests go. girt ignores them, so a Git
// configuration that sets them still works.
const BACKEND_ONLY: &[&str] = &[
    // Chooses among libcurl's TLS libraries; girt always uses rustls.
    "sslbackend",
    // Windows Secure Channel options, for the `schannel` backend only.
    "schannelcheckrevoke",
    "schannelusesslcainfo",
    // Buffer and connection-count tuning for libcurl and dumb HTTP.
    "postbuffer",
    "maxrequests",
    "minsessions",
    // The HTTP version libcurl negotiates; girt always speaks HTTP/1.1.
    "version",
];

// Fail closed: a configured pin, CA directory, cookie, delegation, resolve override or other
// setting that affects security or routing must not silently disappear when a caller switches
// from Git. Backend-only tuning is ignored. Unrelated URL scopes remain irrelevant.
fn reject_unsupported(config: &Config, target: &Url) -> Result<(), HttpError> {
    for entry in config.entries() {
        if !entry.section.eq_ignore_ascii_case(b"http") {
            continue;
        }
        let name = std::str::from_utf8(&entry.name)
            .map_err(|_| HttpError::Configuration("HTTP setting name"))?;
        if ![
            "sslverify",
            "sslcainfo",
            "proxy",
            "proxyauthmethod",
            "followredirects",
            "extraheader",
            "useragent",
            "sslcert",
            "sslkey",
            "sslcertpasswordprotected",
        ]
        .iter()
        .chain(BACKEND_ONLY)
        .any(|known| name.eq_ignore_ascii_case(known))
            && http_value(config, target, name).is_some()
        {
            return Err(HttpError::Configuration("unsupported HTTP setting"));
        }
    }
    Ok(())
}

fn host_match(scope: Option<&str>, target: Option<&str>) -> Option<usize> {
    let (scope, target) = (scope?, target?);
    let scope_parts: Vec<_> = scope.split('.').collect();
    let target_parts: Vec<_> = target.split('.').collect();
    (scope_parts.len() == target_parts.len()
        && scope_parts
            .iter()
            .zip(target_parts)
            .all(|(s, t)| *s == "*" || *s == t))
    .then(|| scope.bytes().filter(|byte| *byte != b'*').count())
}

// Git URL matching normalizes unreserved escapes while preserving escaped separators.
fn normalized_path(path: &str) -> String {
    let mut result = String::new();
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next();
            let low = bytes.next();
            if let (Some(high), Some(low)) = (high, low) {
                let value = (high as char)
                    .to_digit(16)
                    .zip((low as char).to_digit(16))
                    .map(|(h, l)| (h * 16 + l) as u8);
                if let Some(value) =
                    value.filter(|b| b.is_ascii_alphanumeric() || b"-._~".contains(b))
                {
                    result.push(value as char);
                } else {
                    result.push('%');
                    result.push(high.to_ascii_uppercase() as char);
                    result.push(low.to_ascii_uppercase() as char);
                }
            } else {
                result.push('%');
                result.extend(high.map(char::from));
            }
        } else {
            result.push(byte as char);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::remote::{ProtocolEnvironment, Remote};

    fn configuration(settings: &str) -> (Config, Destination) {
        let config = Config::parse(
            format!("[remote \"r\"]\nurl=https://example.test/repo/sub\n{settings}").as_bytes(),
        )
        .unwrap();
        let destination = Remote::find(&config, b"r")
            .unwrap()
            .unwrap()
            .fetch_destination(&config, &ProtocolEnvironment::default())
            .unwrap();
        (config, destination)
    }

    #[rstest]
    #[case::default("", None, true)]
    #[case::config_false("[http]\nsslVerify=false\n", None, false)]
    #[case::environment_true("[http]\nsslVerify=true\n", Some(true), false)]
    #[case::environment_false("[http]\nsslVerify=false\n", Some(false), true)]
    fn verification_precedence(
        #[case] config: &str,
        #[case] disabled: Option<bool>,
        #[case] expected: bool,
    ) {
        let (config, destination) = configuration(config);
        let environment = HttpEnvironment {
            ssl_no_verify: disabled,
            allow_insecure_tls: true,
            ..Default::default()
        };
        let settings = HttpSettings::resolve(&config, &destination, &environment).unwrap();
        assert_eq!(settings.verify_tls, expected);
    }

    #[rstest]
    #[case::environment("", Some("http://environment.test"))]
    #[case::global("[http]\nproxy=http://global.test\n", Some("http://global.test"))]
    #[case::disabled("[http]\nproxy=\n", None)]
    #[case::remote(
        "[http]\nproxy=http://global.test\n[remote \"r\"]\nproxy=http://remote.test\n",
        Some("http://remote.test")
    )]
    #[case::remote_disabled("[http]\nproxy=http://global.test\n[remote \"r\"]\nproxy=\n", None)]
    #[case::unrelated_remote(
        "[remote \"other\"]\nproxy=http://other.test\n",
        Some("http://environment.test")
    )]
    fn proxy_precedence(#[case] config: &str, #[case] expected: Option<&str>) {
        let (config, destination) = configuration(config);
        let environment = HttpEnvironment {
            proxy: Some("http://environment.test".into()),
            ..Default::default()
        };
        let settings =
            HttpSettings::resolve_for_remote(&config, b"r", &destination, &environment).unwrap();
        assert_eq!(settings.proxy.as_deref(), expected);
    }

    #[rstest]
    #[case::backend("sslBackend=openssl")]
    #[case::schannel("schannelCheckRevoke=false")]
    #[case::post_buffer("postBuffer=524288000")]
    #[case::version("version=HTTP/2")]
    #[case::schannel_ca("schannelUseSSLCAInfo=true")]
    #[case::max_requests("maxRequests=5")]
    #[case::min_sessions("minSessions=1")]
    fn backend_only_settings_are_ignored(#[case] setting: &str) {
        let (config, destination) = configuration(&format!("[http]\n{setting}\n"));
        assert!(HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).is_ok());
    }

    #[rstest]
    #[case::pin("pinnedPubkey=secret")]
    #[case::ca_directory("sslCAPath=secret")]
    #[case::client_certificate("sslCert=secret")]
    #[case::proxy_ca("proxySSLCAInfo=secret")]
    #[case::low_speed("lowSpeedLimit=1")]
    #[case::resolve("curloptResolve=example.test:443:127.0.0.1")]
    #[case::cookies("cookieFile=/tmp/cookies")]
    #[case::delegation("delegation=always")]
    #[case::auth("proxyAuthMethod=negotiate")]
    fn unsupported_settings_fail_without_values(#[case] setting: &str) {
        let (config, destination) = configuration(&format!("[http]\n{setting}\n"));
        let error =
            HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).unwrap_err();
        assert!(matches!(error, HttpError::Configuration(_)));
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[test]
    fn unsupported_policy_for_another_origin_does_not_apply() {
        let (config, destination) =
            configuration("[http \"https://other.test\"]\npinnedPubkey=secret\n");
        HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).unwrap();
    }

    #[rstest]
    #[case::userinfo("http://user:secret@proxy.test")]
    #[case::empty_userinfo("http://@proxy.test")]
    #[case::socks("socks5://proxy.test")]
    #[case::https_proxy("https://proxy.test")]
    #[case::path("http://proxy.test/secret")]
    #[case::query("http://proxy.test/?secret")]
    fn unsupported_proxy_urls_fail(#[case] proxy: &str) {
        let (config, destination) = configuration("");
        let environment = HttpEnvironment {
            proxy: Some(proxy.into()),
            ..Default::default()
        };
        let error = HttpSettings::resolve(&config, &destination, &environment).unwrap_err();
        assert!(matches!(error, HttpError::Configuration("proxy URL")));
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn ca_path_selection_does_not_open_files() {
        let (config, destination) = configuration(
            "[http]\nsslCAInfo=global\n[http \"https://example.test/repo\"]\nsslCAInfo=selected\n",
        );
        assert_eq!(
            HttpSettings::configured_ca_info(&config, &destination).unwrap(),
            Some(b"selected".as_slice())
        );
        assert!(HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).is_err());
    }

    #[rstest]
    #[case::implicit("sslCAInfo")]
    #[case::empty("sslCAInfo=")]
    fn invalid_ca_paths_are_refused(#[case] setting: &str) {
        let (config, destination) = configuration(&format!("[http]\n{setting}\n"));
        assert!(HttpSettings::configured_ca_info(&config, &destination).is_err());
    }

    #[test]
    fn explicit_verification_needs_no_insecure_approval() {
        let (config, destination) = configuration("[http]\nsslVerify=false\n");
        let environment = HttpEnvironment {
            ssl_no_verify: Some(false),
            ..Default::default()
        };
        let settings = HttpSettings::resolve(&config, &destination, &environment).unwrap();
        assert!(settings.verify_tls);
    }

    #[test]
    fn oversized_ca_input_is_refused_during_resolution() {
        let (config, destination) = configuration("");
        let environment = HttpEnvironment {
            ssl_ca_info: Some(vec![0; 1024 * 1024 + 1]),
            ..Default::default()
        };
        assert!(matches!(
            HttpSettings::resolve(&config, &destination, &environment),
            Err(HttpError::Limit("trust roots"))
        ));
    }

    #[test]
    fn remote_proxy_authentication_overrides_global_policy() {
        let (config, destination) = configuration(
            "[http]\nproxyAuthMethod=negotiate\n[remote \"r\"]\nproxyAuthMethod=basic\n",
        );
        HttpSettings::resolve_for_remote(&config, b"r", &destination, &HttpEnvironment::default())
            .unwrap();
        assert!(HttpSettings::resolve(&config, &destination, &HttpEnvironment::default()).is_err());
    }
}
