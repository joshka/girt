//! Application decisions about TLS server certificates.
//!
//! A [`CertificateCheck`] sees every server certificate chain together with girt's own verdict,
//! and decides whether the connection proceeds. With a check installed, girt builds the rustls
//! configuration itself, using the same trust sources as without one: configured roots, platform
//! trust plus additional roots, or no verification when the application approved that.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use super::{HttpError, TrustRoots};

/// Decides whether a TLS server certificate is acceptable.
///
/// The check runs during every TLS handshake, on the connection's worker thread, so it must be
/// quick and must not block on user input. girt turns off TLS session resumption while a check is
/// installed, so a later connection cannot skip it. It is implemented for closures.
///
/// # Examples
///
/// Pin one self-signed certificate and otherwise keep girt's verdict:
///
/// ```
/// use std::sync::Arc;
///
/// use girt::transport::http::{CertificateCheck, CertificateDecision, ServerCertificate};
///
/// let pinned: Vec<u8> = Vec::new(); // the DER bytes the application trusts
/// let check = move |server: &ServerCertificate<'_>| {
///     if server.end_entity() == pinned.as_slice() {
///         CertificateDecision::Accept
///     } else {
///         CertificateDecision::UseDefault
///     }
/// };
/// let check: Arc<dyn CertificateCheck> = Arc::new(check);
/// ```
pub trait CertificateCheck: Send + Sync {
    /// Returns the decision for the certificate chain `server` presented.
    fn check(&self, server: &ServerCertificate<'_>) -> CertificateDecision;
}

impl<F> CertificateCheck for F
where
    F: Fn(&ServerCertificate<'_>) -> CertificateDecision + Send + Sync,
{
    fn check(&self, server: &ServerCertificate<'_>) -> CertificateDecision {
        self(server)
    }
}

/// What a [`CertificateCheck`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertificateDecision {
    /// Trust the chain even if girt's own verification failed.
    ///
    /// This skips both chain and hostname verification for this handshake. The server must still
    /// prove it holds the certificate's private key.
    Accept,
    /// Refuse the chain even if girt's own verification succeeded.
    Reject,
    /// Keep girt's verdict, [`ServerCertificate::verification`].
    UseDefault,
}

/// The chain a server presented, as seen by a [`CertificateCheck`].
#[derive(Debug)]
pub struct ServerCertificate<'a> {
    host: &'a str,
    end_entity: &'a [u8],
    intermediates: &'a [CertificateDer<'a>],
    verification: Result<(), CertificateError>,
}

impl ServerCertificate<'_> {
    /// The host name or IP address the connection was made to.
    pub fn host(&self) -> &str {
        self.host
    }

    /// The server's own certificate, DER-encoded.
    pub fn end_entity(&self) -> &[u8] {
        self.end_entity
    }

    /// The intermediate certificates the server sent after its own, DER-encoded, in order.
    pub fn intermediates(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.intermediates.iter().map(|der| der.as_ref())
    }

    /// girt's verdict: chain and hostname verification against the configured trust.
    ///
    /// # Errors
    ///
    /// Returns why verification failed, such as an unknown issuer or a hostname mismatch.
    pub fn verification(&self) -> Result<(), &CertificateError> {
        self.verification.as_ref().map(|_| ())
    }
}

/// Why girt's own verification refused a server certificate.
///
/// The message describes the TLS library's reason and contains no certificate bytes.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct CertificateError(String);

// Verifies chains through `inner` and lets the application decide.
struct Checked {
    inner: Arc<dyn ServerCertVerifier>,
    check: Arc<dyn CertificateCheck>,
    provider: Arc<CryptoProvider>,
}

impl std::fmt::Debug for Checked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checked").finish_non_exhaustive()
    }
}

impl ServerCertVerifier for Checked {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verdict = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        let host = server_name.to_str();
        let server = ServerCertificate {
            host: &host,
            end_entity: end_entity.as_ref(),
            intermediates,
            verification: verdict
                .as_ref()
                .map(|_| ())
                .map_err(|error| CertificateError(error.to_string())),
        };
        match self.check.check(&server) {
            CertificateDecision::Accept => Ok(ServerCertVerified::assertion()),
            CertificateDecision::Reject => Err(rustls::Error::General(
                "certificate rejected by the application".to_owned(),
            )),
            CertificateDecision::UseDefault => verdict,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// Stands in for verification the application disabled; the check still sees every chain.
#[derive(Debug)]
struct Unverified;

impl ServerCertVerifier for Unverified {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        unreachable!("Checked verifies handshake signatures")
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        unreachable!("Checked verifies handshake signatures")
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        Vec::new()
    }
}

// Builds the TLS configuration reqwest would, with `check` deciding each chain.
pub(super) fn client_config(
    roots: &TrustRoots<'_>,
    identity: Option<&[u8]>,
    verify: bool,
    check: Arc<dyn CertificateCheck>,
) -> Result<rustls::ClientConfig, HttpError> {
    // Like reqwest, honour a provider the application installed, falling back to AWS-LC.
    let provider = CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let failure = |_| HttpError::Configuration("TLS client");
    let inner: Arc<dyn ServerCertVerifier> = if !verify {
        Arc::new(Unverified)
    } else {
        match roots {
            TrustRoots::Configured(bundles) if !bundles.is_empty() => {
                let mut store = rustls::RootCertStore::empty();
                for certificate in super::pem_certificates(bundles)? {
                    store
                        .add(certificate)
                        .map_err(|_| HttpError::Configuration("trust roots"))?;
                }
                rustls::client::WebPkiServerVerifier::builder_with_provider(
                    Arc::new(store),
                    provider.clone(),
                )
                .build()
                .map_err(|_| HttpError::Configuration("trust roots"))?
            }
            TrustRoots::Additional(bundles) if !bundles.is_empty() => Arc::new(
                rustls_platform_verifier::Verifier::new_with_extra_roots(
                    super::pem_certificates(bundles)?,
                    provider.clone(),
                )
                .map_err(failure)?,
            ),
            _ => Arc::new(
                rustls_platform_verifier::Verifier::new(provider.clone()).map_err(failure)?,
            ),
        }
    };
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(failure)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Checked {
            inner,
            check,
            provider,
        }));
    let mut config = match identity {
        None => builder.with_no_client_auth(),
        Some(pem) => {
            let invalid = |_| HttpError::Configuration("TLS client certificate");
            let certificates = CertificateDer::pem_slice_iter(pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(invalid)?;
            let key = PrivateKeyDer::from_pem_slice(pem).map_err(invalid)?;
            builder
                .with_client_auth_cert(certificates, key)
                .map_err(|_| HttpError::Configuration("TLS client certificate"))?
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    // A resumed session skips certificate verification, and with it the check.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(config)
}
