//! Explicit smart-HTTP discovery and RPC exchanges.
//!
//! [`HttpRemote`] binds credentials to one repository URL. Fetch and push use protocol v0 over
//! HTTP/1.1; dumb HTTP, redirects, proxies, cookies, credential discovery, automatic retries and
//! HTTP content compression are disabled. TLS uses platform trust plus optional caller-supplied
//! roots, with certificate and hostname verification always enabled.

use std::future::Future;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Url};

use super::TransportControl;

/// One repository endpoint and explicit headers, usable with [`crate::fetch::receive_http`] and
/// [`crate::push::send_http`]. No URL or headers are included in Debug output or transport errors.
///
/// Network methods require a Tokio runtime with I/O and time enabled. Cancellation is polled every
/// 20 ms during network waits; absolute deadlines cover discovery and RPC waits without being reset
/// by traffic. Scheduling and synchronous callbacks/pack validation can delay observation. Dropping
/// a push future after polling may abandon an in-flight mutation without producing a report: prefer
/// setting [`TransportControl::cancel`] and awaiting its uncertain result. No operation retries.
/// OS DNS resolution may finish in Tokio's resolver pool after the network future is cancelled;
/// girt does not wait for it. Already-buffered writes and server processing cannot be recalled.
///
/// Plain HTTP sends supplied credentials without encryption; use HTTPS on untrusted networks.
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
    /// Status other than 200, including redirects, authentication rejection and server errors.
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

impl HttpRemote {
    /// Creates an endpoint with explicit headers and optional additional PEM trust roots.
    ///
    /// Headers may be `Authorization`, `User-Agent`, or application-specific `X-*` fields. Values
    /// must be valid single-line HTTP values; all are marked sensitive. No challenge retry occurs:
    /// supply the complete authorization value before calling. Headers apply to both service paths
    /// at this URL. Empty headers and roots use anonymous access and platform trust.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTP(S) URLs, embedded userinfo (including empty userinfo), queries, fragments,
    /// malformed headers, protocol/routing header overrides and invalid certificates. Errors never
    /// include supplied values. Redirects are rejected even within the same origin.
    pub fn new(url: &str, headers: &[(&str, &str)], roots: &[&[u8]]) -> Result<Self, HttpError> {
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
        for root in roots {
            let cert = reqwest::Certificate::from_pem(root)
                .map_err(|_| HttpError::Configuration("trust root"))?;
            builder = builder.tls_certs_merge([cert]);
        }
        let client = builder
            .build()
            .map_err(|_| HttpError::Configuration("TLS client"))?;
        Ok(Self {
            client,
            base,
            headers: supplied,
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
        let mut output = HttpResponse {
            body: Vec::new(),
            result: Ok(()),
        };
        output.result = self
            .transfer(service, body, limit, control, &mut output.body)
            .await;
        output
    }

    async fn transfer(
        &self,
        service: &str,
        body: Option<RequestBody>,
        limit: usize,
        control: TransportControl<'_>,
        output: &mut Vec<u8>,
    ) -> Result<(), HttpError> {
        check(control)?;
        let rpc = body.is_some();
        let mut url = self
            .base
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
                .post(url)
                .header("Content-Type", format!("application/x-{service}-request"))
                .header("Content-Length", body.length)
                .body(body.body)
        } else {
            self.client.get(url)
        };
        request = request
            .headers(self.headers.clone())
            .header("Accept", &media)
            .header("Accept-Encoding", "identity");
        let mut response = controlled(request.send(), control)
            .await?
            .map_err(|_| HttpError::Network)?;
        if response.status().as_u16() != 200 {
            return Err(HttpError::Status(response.status().as_u16()));
        }
        // Hyper bounds its HTTP/1 parser buffer independently (currently ~400 KiB). This tighter
        // application limit applies once headers are parsed, before accepting any Git body bytes.
        let header_bytes: usize = response
            .headers()
            .iter()
            .map(|(k, v)| k.as_str().len() + v.len())
            .sum();
        if header_bytes > 16 * 1024 {
            return Err(HttpError::Limit("response headers"));
        }
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
            if chunk.len() > limit.saturating_sub(output.len()) {
                output.extend_from_slice(&chunk[..limit.saturating_sub(output.len())]);
                return Err(HttpError::Limit("response body"));
            }
            output.extend_from_slice(&chunk);
        }
        Ok(())
    }
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
    use rstest::rstest;

    use super::*;

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
        send_sync::<crate::fetch::HttpFetch<'_>>();
    }
}
