//! The two ways to the box's LosOS Login.
//!
//! On the company network the box answers at `<host>.local` with a
//! certificate it made itself, which no authority signs. Joining fetches it
//! over plain HTTP and shows its fingerprint to compare with the box's own
//! pages, as losos-backup does when a person picks a box; from then on a
//! connection there must present exactly that certificate.
//!
//! Away from it, the box answers at its public name through its edge, with
//! a certificate a public authority signed: ureq's own roots check that one.

use std::{sync::Arc, time::Duration};

use base64::Engine;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use sha2::{Digest, Sha256};

use crate::Error;

/// How long a login waits to reach the box before it tries the next way,
/// and then homed's own record: a login screen that hangs on a missing
/// network is worse than one that falls back.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// How long LosOS Login may take to answer once reached: an argon2id hash
/// on a small box takes a second or two.
pub const READ_TIMEOUT: Duration = Duration::from_secs(20);

fn builder() -> ureq::AgentBuilder {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .timeout_write(READ_TIMEOUT)
        // LosOS Login answers each call itself; a redirect would be the
        // http-to-https one, which never applies to these https URLs.
        .redirects(0)
        .user_agent(concat!("losos-login-bridge/", env!("CARGO_PKG_VERSION")))
}

/// An agent with ureq's own roots: for the box's public name, and for
/// plain HTTP, which only fetches the box's certificate.
pub fn agent() -> ureq::Agent {
    builder().build()
}

/// An agent that accepts only `cert` (DER).
pub fn pinned(cert: &[u8]) -> Result<ureq::Agent, Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::new(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            cert: cert.to_vec(),
            provider,
        }))
        .with_no_client_auth();
    Ok(builder().tls_config(Arc::new(config)).build())
}

/// Accepts the one certificate it holds, and checks the handshake's
/// signatures against it as any verifier would.
#[derive(Debug)]
struct Pinned {
    cert: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.cert.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The DER of the first certificate in `pem`.
pub fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body = pem
        .split("-----BEGIN CERTIFICATE-----")
        .nth(1)?
        .split("-----END CERTIFICATE-----")
        .next()?;
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// A certificate's SHA-256 fingerprint as `openssl x509 -fingerprint
/// -sha256` and the box's wizard spell it: `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Fetches `host`'s certificate over plain HTTP and checks it against the
/// fingerprint in the box's `/setup/state.json`. Both come unauthenticated;
/// matching each other only rules out a truncated or mixed-up answer, which
/// is why the fingerprint is shown to compare with the box's own pages.
pub fn fetch_certificate(host: &str) -> Result<String, Error> {
    let agent = agent();
    let base = format!("http://{host}");
    let pem = agent
        .get(&format!("{base}/setup/losos-ca.crt"))
        .call()?
        .into_string()?;
    let der = pem_to_der(&pem).ok_or_else(|| {
        Error::new(format!(
            "{host} answered /setup/losos-ca.crt with no certificate: is it a LosOS box?"
        ))
    })?;
    let state: serde_json::Value = agent
        .get(&format!("{base}/setup/state.json"))
        .call()?
        .into_json()?;
    let stated = state
        .get("fingerprint")
        .and_then(|f| f.as_str())
        .and_then(|f| f.strip_prefix("sha256:"))
        .map(|hex| hex.replace(':', "").to_ascii_uppercase());
    if stated.as_deref() != Some(fingerprint(&der).replace(':', "").as_str()) {
        return Err(Error::new(format!(
            "The certificate {host} sent does not match the fingerprint it states."
        )));
    }
    Ok(pem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_a_fingerprint_as_openssl_does() {
        let pem = "-----BEGIN CERTIFICATE-----\nAAEC\nAw==\n-----END CERTIFICATE-----\n";
        let der = pem_to_der(pem).unwrap();
        assert_eq!(der, [0, 1, 2, 3]);
        assert_eq!(
            fingerprint(&der),
            "05:4E:DE:C1:D0:21:1F:62:4F:ED:0C:BC:A9:D4:F9:40:0B:0E:49:1C:43:74:2A:F2:C5:B0:AB:EB:F0:C9:90:D8"
        );
    }
}
