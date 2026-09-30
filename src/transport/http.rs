//! Explicit smart-HTTP discovery and RPC exchanges.
//!
//! Resolve [`HttpSettings`] from a selected [`crate::remote::Destination`], [`crate::Config`], and
//! explicit [`HttpEnvironment`], then construct [`HttpRemote::configured`] for fetch or push
//! adapters. [`HttpRemote::with_credentials`] attaches an application-approved credential session.
//! [`TransportControl`] supplies cancellation and a deadline for owned
//! waits; the application owns the Tokio runtime and synchronous fetch validation worker.
//!
//! [`HttpRemote`] binds credentials to one repository URL. Fetch and push use protocol v0 over
//! HTTP/1.1. [`HttpSettings`] resolves an R21 destination, HTTP configuration and explicit
//! application environment. The legacy [`HttpRemote::new`] remains an explicit, redirect-free
//! constructor. Cookies, automatic mutating retries and HTTP content compression are disabled.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Url};

use super::TransportControl;
use crate::remote::CredentialSession;

mod configuration;
pub use configuration::{HttpEnvironment, HttpSettings};

/// One repository endpoint and explicit headers, usable with [`crate::fetch::receive_http`] and
/// [`crate::push::send_http`]. No URL or headers are included in Debug output or transport errors.
///
/// Network methods require a Tokio runtime with I/O and time enabled. Cancellation is polled every
/// 20 ms during network waits; absolute deadlines cover discovery and RPC waits without being reset
/// by traffic. Scheduling and synchronous callbacks/pack validation can delay observation. Dropping
/// a push future after polling may abandon an in-flight mutation without producing a report: prefer
/// setting [`TransportControl::cancel`] and awaiting its uncertain result. Only a challenged
/// discovery GET can retry, once, with a bound credential session.
/// OS DNS resolution may finish in Tokio's resolver pool after the network future is cancelled;
/// girt does not wait for it. Already-buffered writes and server processing cannot be recalled.
///
/// Plain HTTP sends supplied credentials without encryption; use HTTPS on untrusted networks.
/// Helper approval/rejection is synchronous and may block the calling executor thread, including
/// process startup and application callbacks. Run configured operations on an application worker
/// when this latency matters; R22's deadline and cancellation limits apply to helper processes.
/// Server-controlled Git progress and rejection messages remain untrusted bytes, and should not be
/// logged as trusted diagnostics. This type emits no logs of its own.
///
/// Response headers are limited to 64 fields and 16 KiB of names/values. Hyper also bounds its
/// HTTP/1 parser buffer (currently about 400 KiB); that allocation precedes the stricter byte
/// check. Request URL length is limited to 8192 bytes and supplied headers to 16 KiB. Body limits
/// come from fetch/push options. Library/TLS/socket buffers and allocator overhead are separate
/// from retained body budgets. Concurrent calls have independent budgets: callers bound
/// concurrency. Futures are `Send` when the fetch selection callback is `Send`; all borrowed inputs
/// must live until completion. No hidden Tokio runtime or detached CPU validation task is created.
pub struct HttpRemote {
    client: Client,
    base: Url,
    headers: HeaderMap,
    credential: Option<Arc<CredentialSession>>,
    credential_approved: Mutex<bool>,
    follow_initial_redirect: bool,
    redirected_base: Mutex<Option<Url>>,
}

impl std::fmt::Debug for HttpRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRemote").finish_non_exhaustive()
    }
}

/// Sanitized HTTP failures. Response bodies, URLs, header values and underlying client diagnostic
/// strings are deliberately excluded because they may contain credentials.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Invalid endpoint, header or trust configuration; no request was sent.
    #[error("invalid HTTP configuration: {0}")]
    Configuration(&'static str),
    /// Credential helper lifecycle failed. Its configured values and process output are redacted.
    #[error("HTTP credential lifecycle failed")]
    Credential(#[from] crate::remote::CredentialError),
    /// Status other than 200, including refused redirects, authentication rejection and errors.
    #[error("HTTP status {0}")]
    Status(u16),
    /// Wrong/missing/duplicate media type, unsupported content encoding, or malformed service
    /// prelude.
    #[error("invalid smart HTTP response: {0}")]
    Protocol(&'static str),
    /// Bounded request, response or headers exceeded the caller's budget.
    #[error("HTTP limit exceeded: {0}")]
    Limit(&'static str),
    /// DNS, connection, TLS validation, HTTP framing or body transport failed. No raw cause is
    /// exposed.
    #[error("HTTP network or TLS failure")]
    Network,
    /// Caller cancellation observed during a network wait.
    #[error("HTTP cancelled")]
    Cancelled,
    /// Absolute transport deadline expired.
    #[error("HTTP deadline expired")]
    Deadline,
}

/// Configured CA bundles replace platform roots; the explicit legacy constructor adds roots.
enum TrustRoots<'a> {
    Configured(&'a [&'a [u8]]),
    Additional(&'a [&'a [u8]]),
}

impl TrustRoots<'_> {
    fn configure(
        self,
        builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, HttpError> {
        let bundles = match self {
            Self::Configured(bundles) | Self::Additional(bundles) => bundles,
        };
        if bundles.is_empty() {
            return Ok(builder);
        }
        let mut certificates = Vec::new();
        let mut bytes = 0usize;
        for bundle in bundles {
            bytes = bytes.saturating_add(bundle.len());
            if bytes > 1024 * 1024 {
                return Err(HttpError::Limit("trust roots"));
            }
            let parsed = reqwest::Certificate::from_pem_bundle(bundle)
                .map_err(|_| HttpError::Configuration("trust roots"))?;
            if parsed.is_empty() {
                return Err(HttpError::Configuration("empty trust roots"));
            }
            certificates.extend(parsed);
        }
        Ok(match self {
            Self::Configured(_) => builder.tls_certs_only(certificates),
            Self::Additional(_) => builder.tls_certs_merge(certificates),
        })
    }
}

impl HttpRemote {
    /// Builds an HTTP connection from an R21 destination and explicit policy inputs.
    ///
    /// [`HttpSettings::resolve`] validates policy before a request can begin. Supplied CA bundles
    /// replace platform roots; absent bundles use the rustls platform verifier.
    ///
    /// # Errors
    ///
    /// Rejects invalid endpoints, empty/malformed CA bundles and client configuration failures.
    /// Errors omit supplied URLs, certificates and credentials.
    pub fn configured(settings: HttpSettings) -> Result<Self, HttpError> {
        let roots: Vec<_> = settings.roots.iter().map(Vec::as_slice).collect();
        let mut remote = Self::build(
            &settings.url,
            &[],
            TrustRoots::Configured(&roots),
            settings.proxy.as_deref(),
            settings.no_proxy.as_deref(),
            settings.proxy_credentials.as_ref(),
            settings.verify_tls,
        )?;
        remote.follow_initial_redirect = settings.follow_initial_redirect;
        Ok(remote)
    }

    /// Binds one filled helper session to exactly this repository origin and path.
    ///
    /// The session must be obtained through application-approved helpers or prompting before
    /// entering the async transport. A mismatched scope is refused before any request. Discovery
    /// first tries anonymously, then uses Basic authentication only when a 401 challenge advertises
    /// Basic. Subsequent requests send that credential only to the same origin; credential-bearing
    /// redirects remain disabled.
    pub fn with_credentials(mut self, session: CredentialSession) -> Result<Self, HttpError> {
        if self
            .redirected_base
            .lock()
            .map_err(|_| HttpError::Configuration("redirect state"))?
            .is_some()
        {
            return Err(HttpError::Configuration("credential scope after redirect"));
        }
        let context = session.context();
        let authority = match self.base.port() {
            Some(port) => format!("{}:{port}", self.base.host_str().unwrap_or_default()),
            None => self.base.host_str().unwrap_or_default().to_owned(),
        };
        if context.protocol != self.base.scheme().as_bytes()
            || context.host != authority.as_bytes()
            || context.path.as_deref() != Some(self.base.path().trim_matches('/').as_bytes())
            || !session.credential().complete()
            || self.headers.contains_key("authorization")
        {
            return Err(HttpError::Configuration("credential scope"));
        }
        self.credential = Some(Arc::new(session));
        *self
            .credential_approved
            .lock()
            .map_err(|_| HttpError::Configuration("credential state"))? = false;
        Ok(self)
    }

    /// Creates an endpoint with explicit headers and optional additional PEM trust roots.
    ///
    /// Headers may be `Authorization`, `User-Agent`, or application-specific `X-*` fields. Values
    /// must be valid single-line HTTP values; all are marked sensitive. No challenge retry occurs:
    /// supply the complete authorization value before calling. Headers apply to both service paths
    /// at this URL. Empty headers and roots use anonymous access and platform trust. Supplied PEM
    /// bundles must contain certificates and total at most 1 MiB.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTP(S) URLs, embedded userinfo (including empty userinfo), queries, fragments,
    /// malformed headers, protocol/routing header overrides and invalid certificates. Errors never
    /// include supplied values. Redirects are rejected even within the same origin.
    pub fn new(url: &str, headers: &[(&str, &str)], roots: &[&[u8]]) -> Result<Self, HttpError> {
        Self::build(
            url,
            headers,
            TrustRoots::Additional(roots),
            None,
            None,
            None,
            true,
        )
    }

    fn build(
        url: &str,
        headers: &[(&str, &str)],
        roots: TrustRoots<'_>,
        proxy: Option<&str>,
        no_proxy: Option<&str>,
        proxy_credentials: Option<&(String, String)>,
        verify_tls: bool,
    ) -> Result<Self, HttpError> {
        if url.len() > 8192
            || url
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            return Err(HttpError::Configuration(
                "repository URL length or whitespace",
            ));
        }
        let mut base = Url::parse(url).map_err(|_| HttpError::Configuration("repository URL"))?;
        let authority = url
            .split_once("://")
            .map(|(_, rest)| rest.split('/').next().unwrap_or(rest));
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || authority.is_some_and(|a| a.contains('@'))
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(HttpError::Configuration(
                "repository URL must omit credentials, query and fragment",
            ));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let mut supplied = HeaderMap::new();
        let mut bytes = 0usize;
        for &(name, value) in headers {
            bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
            if bytes > 16 * 1024 {
                return Err(HttpError::Limit("request headers"));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpError::Configuration("header name"))?;
            if name != "authorization" && name != "user-agent" && !name.as_str().starts_with("x-") {
                return Err(HttpError::Configuration(
                    "header is not Authorization, User-Agent or X-*",
                ));
            }
            let mut value = HeaderValue::from_str(value)
                .map_err(|_| HttpError::Configuration("header value"))?;
            value.set_sensitive(true);
            if supplied.insert(name, value).is_some() {
                return Err(HttpError::Configuration("duplicate header"));
            }
        }
        let mut builder = Client::builder()
            .tls_backend_rustls()
            .http1_only()
            .http1_max_headers(64)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .pool_max_idle_per_host(0);
        if proxy_credentials.is_some() && proxy.is_none() {
            return Err(HttpError::Configuration("proxy credentials without proxy"));
        }
        if proxy_credentials
            .is_some_and(|(user, password)| user.len().saturating_add(password.len()) > 12 * 1024)
        {
            return Err(HttpError::Limit("proxy credentials"));
        }
        if let Some(proxy) = proxy {
            let mut configured =
                reqwest::Proxy::all(proxy).map_err(|_| HttpError::Configuration("proxy URL"))?;
            if let Some((username, password)) = proxy_credentials {
                configured = configured.basic_auth(username, password);
            }
            if let Some(exclusions) = no_proxy {
                configured = configured.no_proxy(reqwest::NoProxy::from_string(exclusions));
            }
            builder = builder.proxy(configured);
        }
        if !verify_tls {
            builder = builder
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        builder = roots.configure(builder)?;
        let client = builder
            .build()
            .map_err(|_| HttpError::Configuration("TLS client"))?;
        Ok(Self {
            client,
            base,
            headers: supplied,
            credential: None,
            credential_approved: Mutex::new(false),
            follow_initial_redirect: false,
            redirected_base: Mutex::new(None),
        })
    }

    pub(crate) async fn discover(
        &self,
        service: &str,
        limit: usize,
        control: TransportControl<'_>,
    ) -> Result<(Vec<u8>, usize), HttpError> {
        let mut response = self.exchange(service, None, limit, control).await;
        response.result?;
        let prelude = format!("# service={service}\n");
        let expected = format!("{:04x}{prelude}0000", prelude.len() + 4);
        if !response.body.starts_with(expected.as_bytes()) {
            return Err(HttpError::Protocol(
                "missing service advertisement (dumb HTTP unsupported)",
            ));
        }
        let wire_bytes = response.body.len();
        response.body.drain(..expected.len());
        Ok((response.body, wire_bytes))
    }

    // Keep the valid body prefix on interrupted/truncated RPCs so push can retain acknowledgements.
    pub(crate) async fn exchange(
        &self,
        service: &str,
        body: Option<RequestBody>,
        limit: usize,
        control: TransportControl<'_>,
    ) -> HttpResponse {
        self.exchange_observed(service, body, limit, control, |_| {})
            .await
    }

    // Observers see only the retained, bounded prefix, including on a body-limit failure.
    pub(crate) async fn exchange_observed(
        &self,
        service: &str,
        body: Option<RequestBody>,
        limit: usize,
        control: TransportControl<'_>,
        mut observe: impl FnMut(&[u8]),
    ) -> HttpResponse {
        let mut output = HttpResponse {
            body: Vec::new(),
            result: Ok(()),
        };
        let receive = |chunk: &[u8]| {
            let accepted = chunk.len().min(limit.saturating_sub(output.body.len()));
            output.body.extend_from_slice(&chunk[..accepted]);
            observe(&output.body);
            if accepted < chunk.len() {
                return Err(HttpError::Limit("response body"));
            }
            Ok(())
        };
        output.result = self.transfer(service, body, control, receive).await;
        output
    }

    async fn transfer(
        &self,
        service: &str,
        body: Option<RequestBody>,
        control: TransportControl<'_>,
        mut receive: impl FnMut(&[u8]) -> Result<(), HttpError>,
    ) -> Result<(), HttpError> {
        check(control)?;
        let rpc = body.is_some();
        let effective_base = self
            .redirected_base
            .lock()
            .map_err(|_| HttpError::Configuration("redirect state"))?
            .clone()
            .unwrap_or_else(|| self.base.clone());
        let mut url = effective_base
            .join(if rpc { service } else { "info/refs" })
            .map_err(|_| HttpError::Configuration("service URL"))?;
        if !rpc {
            url.set_query(Some(&format!("service={service}")));
        }
        let media = format!(
            "application/x-{service}-{}",
            if rpc { "result" } else { "advertisement" }
        );
        let mut request = if let Some(body) = body {
            self.client
                .post(url.clone())
                .header("Content-Type", format!("application/x-{service}-request"))
                .header("Content-Length", body.length)
                .body(body.body)
        } else {
            self.client.get(url.clone())
        };
        request = request
            .headers(self.headers.clone())
            .header("Accept", &media)
            .header("Accept-Encoding", "identity");
        if rpc && let Some(session) = &self.credential {
            request = request.header("Authorization", basic_authorization(session)?);
        }
        let mut response = controlled(request.send(), control)
            .await?
            .map_err(|_| HttpError::Network)?;
        check_header_budget(response.headers())?;
        let mut new_base = None;
        if !rpc
            && self.follow_initial_redirect
            && matches!(response.status().as_u16(), 301 | 302 | 307 | 308)
        {
            let location = response
                .headers()
                .get("location")
                .ok_or(HttpError::Configuration("redirect location"))?
                .to_str()
                .map_err(|_| HttpError::Configuration("redirect location"))?;
            if location.len() > 8192 {
                return Err(HttpError::Limit("redirect location"));
            }
            let redirected = url
                .join(location)
                .map_err(|_| HttpError::Configuration("redirect location"))?;
            let expected_query = format!("service={service}");
            let path = redirected
                .path()
                .strip_suffix("info/refs")
                .ok_or(HttpError::Configuration("redirect service path"))?;
            if redirected.scheme() != url.scheme()
                || redirected.host_str() != url.host_str()
                || redirected.port_or_known_default() != url.port_or_known_default()
                || !redirected.username().is_empty()
                || redirected.password().is_some()
                || redirected.fragment().is_some()
                || redirected.query() != Some(expected_query.as_str())
                || self.credential.is_some()
            {
                return Err(HttpError::Configuration("redirect destination"));
            }
            let mut base = redirected.clone();
            base.set_path(path);
            base.set_query(None);
            let request = self
                .client
                .get(redirected.clone())
                .headers(self.headers.clone())
                .header("Accept", &media)
                .header("Accept-Encoding", "identity");
            response = controlled(request.send(), control)
                .await?
                .map_err(|_| HttpError::Network)?;
            check_header_budget(response.headers())?;
            url = redirected;
            new_base = Some(base);
        }
        if !rpc
            && response.status().as_u16() == 401
            && let Some(session) = &self.credential
            && offers_basic(response.headers())
        {
            check(control)?;
            let request = self
                .client
                .get(url)
                .headers(self.headers.clone())
                .header("Accept", &media)
                .header("Accept-Encoding", "identity")
                .header("Authorization", basic_authorization(session)?);
            response = controlled(request.send(), control)
                .await?
                .map_err(|_| HttpError::Network)?;
            check_header_budget(response.headers())?;
            if response.status().as_u16() == 401 {
                self.authentication_result(session, false, control)?;
            } else if response.status().as_u16() == 200 {
                self.authentication_result(session, true, control)?;
            }
        } else if rpc && let Some(session) = &self.credential {
            if response.status().as_u16() == 401 {
                self.authentication_result(session, false, control)?;
            } else if response.status().as_u16() == 200 {
                self.authentication_result(session, true, control)?;
            }
        }
        if response.status().as_u16() != 200 {
            return Err(HttpError::Status(response.status().as_u16()));
        }
        if let Some(base) = new_base {
            *self
                .redirected_base
                .lock()
                .map_err(|_| HttpError::Configuration("redirect state"))? = Some(base);
        }
        // Hyper bounds its HTTP/1 parser buffer independently (currently ~400 KiB). This tighter
        // application limit applies once headers are parsed, before accepting any Git body bytes.
        let mut types = response.headers().get_all("content-type").iter();
        if types
            .next()
            .is_none_or(|v| !v.as_bytes().eq_ignore_ascii_case(media.as_bytes()))
            || types.next().is_some()
        {
            return Err(HttpError::Protocol("media type (dumb HTTP unsupported)"));
        }
        if response.headers().contains_key("content-encoding") {
            return Err(HttpError::Protocol("content encoding"));
        }
        while let Some(chunk) = controlled(response.chunk(), control)
            .await?
            .map_err(|_| HttpError::Network)?
        {
            receive(&chunk)?;
        }
        Ok(())
    }

    fn authentication_result(
        &self,
        session: &CredentialSession,
        accepted: bool,
        control: TransportControl<'_>,
    ) -> Result<(), HttpError> {
        let mut approved = self
            .credential_approved
            .lock()
            .map_err(|_| HttpError::Configuration("credential state"))?;
        if accepted {
            if !*approved {
                session.approve(control.cancel, control.deadline)?;
                *approved = true;
            }
        } else {
            session.reject(control.cancel, control.deadline)?;
            *approved = false;
        }
        Ok(())
    }
}

fn check_header_budget(headers: &HeaderMap) -> Result<(), HttpError> {
    let bytes: usize = headers
        .iter()
        .map(|(name, value)| name.as_str().len().saturating_add(value.len()))
        .sum();
    if bytes > 16 * 1024 {
        return Err(HttpError::Limit("response headers"));
    }
    Ok(())
}

fn offers_basic(headers: &HeaderMap) -> bool {
    headers.get_all("www-authenticate").iter().any(|value| {
        let bytes = value.as_bytes();
        bytes
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"Basic"))
            && bytes.get(5).is_none_or(u8::is_ascii_whitespace)
    })
}

fn basic_authorization(session: &CredentialSession) -> Result<HeaderValue, HttpError> {
    let credential = session.credential();
    let username = credential
        .username
        .as_deref()
        .ok_or(HttpError::Configuration("incomplete credential"))?;
    let password = credential
        .password
        .as_deref()
        .ok_or(HttpError::Configuration("incomplete credential"))?;
    if username.contains(&b':') || username.contains(&0) || password.contains(&0) {
        return Err(HttpError::Configuration("Basic credential syntax"));
    }
    if username.len().saturating_add(password.len()) > 12 * 1024 {
        return Err(HttpError::Limit("Basic credential"));
    }
    let mut raw = Vec::with_capacity(username.len() + password.len() + 1);
    raw.extend_from_slice(username);
    raw.push(b':');
    raw.extend_from_slice(password);
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = String::with_capacity(6 + raw.len().div_ceil(3) * 4);
    value.push_str("Basic ");
    for chunk in raw.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        value.push(alphabet[((n >> 18) & 63) as usize] as char);
        value.push(alphabet[((n >> 12) & 63) as usize] as char);
        value.push(if chunk.len() > 1 {
            alphabet[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        value.push(if chunk.len() > 2 {
            alphabet[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    let mut header = HeaderValue::from_str(&value)
        .map_err(|_| HttpError::Configuration("Basic credential syntax"))?;
    header.set_sensitive(true);
    Ok(header)
}

// Own both existing buffers so HTTP upload can yield without copying a large prepared pack.
pub(crate) struct RequestBody {
    body: reqwest::Body,
    length: u64,
}
impl RequestBody {
    pub(crate) fn new(request: Vec<u8>, pack: Vec<u8>) -> Self {
        let length = request.len() as u64 + pack.len() as u64;
        let chunks = [Ok::<_, std::io::Error>(request), Ok(pack)];
        Self {
            body: reqwest::Body::wrap_stream(futures_util::stream::iter(chunks)),
            length,
        }
    }
}

pub(crate) struct HttpResponse {
    pub(crate) body: Vec<u8>,
    pub(crate) result: Result<(), HttpError>,
}

fn check(control: TransportControl<'_>) -> Result<(), HttpError> {
    control.check().map_err(|e| match super::interruption(&e) {
        Some(super::Interruption::Cancelled) => HttpError::Cancelled,
        _ => HttpError::Deadline,
    })
}

async fn controlled<T>(
    future: impl Future<Output = T>,
    control: TransportControl<'_>,
) -> Result<T, HttpError> {
    tokio::pin!(future);
    loop {
        check(control)?;
        let interval = control
            .deadline
            .map(|d| {
                d.saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(20))
            })
            .unwrap_or(Duration::from_millis(20));
        tokio::select! {
            biased;
            _ = tokio::time::sleep(interval) => {},
            value = &mut future => return Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use rstest::rstest;

    use super::*;
    use crate::Config;
    use crate::remote::{CredentialContext, Destination, Prompt, ProtocolEnvironment, Remote};

    fn destination(config: &Config) -> Destination {
        Remote::find(config, b"r")
            .unwrap()
            .unwrap()
            .fetch_destination(config, &ProtocolEnvironment::default())
            .unwrap()
    }

    #[rstest]
    #[case::userinfo("https://name:secret@example.com/repo")]
    #[case::empty_user("https://@example.com/repo")]
    #[case::query("https://example.com/repo?token=secret")]
    #[case::fragment("https://example.com/repo#secret")]
    #[case::scheme("file:///secret")]
    #[case::invalid("secret")]
    fn rejects_credential_bearing_or_unsupported_urls(#[case] url: &str) {
        let error = HttpRemote::new(url, &[], &[]).unwrap_err();
        assert!(matches!(error, HttpError::Configuration(_)));
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[rstest]
    #[case::host("Host", "secret")]
    #[case::proxy("Proxy-Authorization", "secret")]
    #[case::protocol("Git-Protocol", "version=2")]
    #[case::length("Content-Length", "0")]
    #[case::connection("Connection", "keep-alive")]
    #[case::injection("Authorization", "secret\r\nHost: other")]
    fn rejects_reserved_headers_and_injection(#[case] name: &str, #[case] value: &str) {
        let error = HttpRemote::new("https://example.com/repo", &[(name, value)], &[]).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret"));
    }

    #[test]
    fn endpoint_debug_redacts_headers_and_path() {
        let remote = HttpRemote::new(
            "https://example.com/secret",
            &[("Authorization", "Bearer secret")],
            &[],
        )
        .unwrap();
        assert!(!format!("{remote:?}").contains("secret"));
    }

    #[test]
    fn transport_types_can_move_between_workers() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<HttpRemote>();
        send_sync::<crate::fetch::DownloadedFetch>();
    }

    #[test]
    fn scoped_http_policy_uses_longest_component_path_and_configured_proxy_override() {
        let config = Config::parse(b"[remote \"r\"]\nurl = https://example.test/repo/sub\n[http]\nproxy = http://global.test\n[http \"https://example.test/repo\"]\nproxy = http://scoped.test\n[http \"https://example.test/repository\"]\nproxy = http://wrong.test\n").unwrap();
        let resolved =
            HttpSettings::resolve(&config, &destination(&config), &HttpEnvironment::default())
                .unwrap();
        assert_eq!(resolved.proxy.as_deref(), Some("http://scoped.test"));
        let environment = HttpEnvironment {
            proxy: Some("http://environment.test".to_owned()),
            ..HttpEnvironment::default()
        };
        let resolved = HttpSettings::resolve(&config, &destination(&config), &environment).unwrap();
        assert_eq!(resolved.proxy.as_deref(), Some("http://scoped.test"));
        assert!(!format!("{environment:?} {resolved:?}").contains("environment.test"));
    }

    #[test]
    fn credential_scope_mismatch_is_refused_before_network() {
        let config = Config::parse(b"[credential]\nusername = user\n").unwrap();
        let context = CredentialContext {
            protocol: b"https".to_vec(),
            host: b"example.test".to_vec(),
            path: Some(b"other".to_vec()),
            use_http_path: false,
        };
        let cancel = AtomicBool::new(false);
        let mut prompt = |field| (field == Prompt::Password).then(|| b"secret".to_vec());
        let session =
            CredentialSession::fill(&config, context, &[], Some(&mut prompt), &cancel, None)
                .unwrap();
        let error = HttpRemote::new("https://example.test/repo", &[], &[])
            .unwrap()
            .with_credentials(session)
            .unwrap_err();
        assert!(matches!(
            error,
            HttpError::Configuration("credential scope")
        ));
        assert!(!format!("{error:?}").contains("secret"));
    }
}
