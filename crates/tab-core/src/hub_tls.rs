//! The hub listener's TLS: TLS 1.3 only, on TCP (HTTP/2, HTTP/1.1) and
//! QUIC (HTTP/3), even on localhost.
//!
//! No public CA will sign a certificate for `127.0.0.1` or a LAN address,
//! so each install makes its own: an ECDSA P-256 certificate kept next to
//! `hub.db` (`hub-tls/`). Clients don't trust it through a CA; they pin it.
//! A device learns the hub's fingerprint (SHA-256 of the certificate) when
//! it's paired, along with the token, and [`pinned_client`] refuses any
//! other certificate. That's stronger than CA trust for this case: nobody
//! else can mint a certificate the client accepts.

use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// ALPN ids.
pub const H3: &[u8] = b"h3";
pub const H2: &[u8] = b"h2";
pub const H1: &[u8] = b"http/1.1";

/// This hub's certificate and key.
#[derive(Clone)]
pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: Arc<PrivatePkcs8KeyDer<'static>>,
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// `sha256/<hex>` of a certificate.
pub fn fingerprint(cert: &[u8]) -> String {
    let d = Sha256::digest(cert);
    let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256/{hex}")
}

impl Identity {
    /// Loads the certificate in `dir`, or makes one (valid for localhost,
    /// 127.0.0.1 and ::1, ten years) the first time.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        let (cert_path, key_path) = (dir.join("cert.der"), dir.join("key.der"));
        if let (Ok(cert), Ok(key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            return Ok(Self {
                cert: CertificateDer::from(cert),
                key: Arc::new(PrivatePkcs8KeyDer::from(key)),
            });
        }
        std::fs::create_dir_all(dir)?;
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut params = rcgen::CertificateParams::new(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ])?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "tab hub");
        params.not_before = rcgen::date_time_ymd(2025, 1, 1);
        params.not_after = rcgen::date_time_ymd(2035, 1, 1);
        let cert = params.self_signed(&key)?;
        write_private(&key_path, &key.serialize_der())?;
        std::fs::write(&cert_path, cert.der())?;
        Ok(Self {
            cert: cert.der().clone(),
            key: Arc::new(PrivatePkcs8KeyDer::from(key.serialize_der())),
        })
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }

    /// A TLS 1.3-only server config offering `alpn`.
    pub fn server(&self, alpn: &[&[u8]]) -> anyhow::Result<rustls::ServerConfig> {
        let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(
                vec![self.cert.clone()],
                PrivateKeyDer::Pkcs8(self.key.as_ref().clone_key()),
            )?;
        cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
        Ok(cfg)
    }
}

/// Writes the key readable only by this user.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// A TLS 1.3-only client config that accepts exactly the hub certificate
/// with this fingerprint, offering `alpn`.
pub fn pinned_client(fingerprint: &str, alpn: &[&[u8]]) -> anyhow::Result<rustls::ClientConfig> {
    let verifier = Arc::new(Pinned {
        fingerprint: fingerprint.to_string(),
        provider: provider(),
    });
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    cfg.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
    Ok(cfg)
}

#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // TLS 1.2 is never negotiated.
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // The pinned key must still prove it holds the certificate.
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

/// Compares secrets without leaking where they differ through timing.
pub fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_certificate_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("tab-hub-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(a.fingerprint().starts_with("sha256/") && a.fingerprint().len() == 7 + 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("key.der"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(a.server(&[H2]).is_ok());
        assert!(pinned_client(&a.fingerprint(), &[H2]).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn secrets_compare_exactly() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "ab"));
        assert!(!same_secret("", "x"));
    }
}
