//! Self-signed TLS certificate management for the embedded axum server (slice 8).
//!
//! Generates a persistent self-signed certificate at first boot, stores it in
//! `<data_dir>/tls/` at mode 0600, and provides helpers to load it into a
//! rustls [`ServerConfig`] and to compute its SHA-256 fingerprint for the
//! pairing QR.

use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::CertificateDer;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// In-memory TLS certificate + key pair.
#[derive(Debug, Clone)]
pub struct TlsCert {
    /// PEM-encoded certificate chain (leaf only for self-signed).
    pub cert_pem: Vec<u8>,
    /// PEM-encoded private key.
    pub key_pem: Vec<u8>,
}

impl TlsCert {
    /// Path to the TLS directory under `data_dir`.
    fn tls_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("tls")
    }

    /// Path to the certificate PEM file.
    fn cert_path(data_dir: &Path) -> PathBuf {
        Self::tls_dir(data_dir).join("cert.pem")
    }

    /// Path to the private-key PEM file.
    fn key_path(data_dir: &Path) -> PathBuf {
        Self::tls_dir(data_dir).join("key.pem")
    }

    /// Load an existing certificate from disk or generate a new self-signed one.
    ///
    /// On first boot the `<data_dir>/tls/` directory does not exist; a fresh
    /// ECDSA P-256 certificate for `pialarm.local` (and a few common LAN aliases)
    /// is generated and written with mode 0600. `lan_ip`, when provided, is also
    /// added as a Subject Alternative Name so the self-signed cert is valid when
    /// the phone connects via the IP fallback URL (`https://<lan_ip>:<port>/…`)
    /// rather than the mDNS hostname. On subsequent boots the existing files are
    /// reused so the fingerprint stays stable.
    pub fn ensure(data_dir: &Path) -> Result<Self, String> {
        Self::ensure_with_lan_ip(data_dir, None)
    }

    /// Like [`Self::ensure`] but allows the LAN IP to be baked into the cert's
    /// SANs at first-boot generation (see its doc comment for why).
    pub fn ensure_with_lan_ip(
        data_dir: &Path,
        lan_ip: Option<&str>,
    ) -> Result<Self, String> {
        let cert_path = Self::cert_path(data_dir);
        let key_path = Self::key_path(data_dir);

        if cert_path.exists() && key_path.exists() {
            let cert_pem = fs::read(&cert_path)
                .map_err(|e| format!("failed to read TLS cert: {e}"))?;
            let key_pem = fs::read(&key_path)
                .map_err(|e| format!("failed to read TLS key: {e}"))?;
            return Ok(Self { cert_pem, key_pem });
        }

        let tls_dir = Self::tls_dir(data_dir);
        fs::create_dir_all(&tls_dir)
            .map_err(|e| format!("failed to create tls dir: {e}"))?;

        let mut params = CertificateParams::new(vec![
            "pialarm.local".to_string(),
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ]);
        if let Some(ip) = lan_ip {
            params.subject_alt_names.push(rcgen::SanType::IpAddress(ip.parse().map_err(
                |e| format!("invalid LAN IP for cert SAN: {e}"),
            )?));
        }
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            rcgen::DnValue::Utf8String("Pi Alarm Clock".to_string()),
        );
        params.alg = &PKCS_ECDSA_P256_SHA256;

        let key_pair = KeyPair::generate(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| format!("failed to generate TLS key pair: {e}"))?;
        params.key_pair = Some(key_pair);

        let cert = rcgen::Certificate::from_params(params)
            .map_err(|e| format!("failed to generate self-signed cert: {e}"))?;

        let cert_pem = cert
            .serialize_pem()
            .map_err(|e| format!("failed to serialize cert: {e}"))?
            .into_bytes();
        let key_pem = cert
            .serialize_private_key_pem()
            .into_bytes();

        write_file_0600(&cert_path, &cert_pem)
            .map_err(|e| format!("failed to write TLS cert: {e}"))?;
        write_file_0600(&key_path, &key_pem)
            .map_err(|e| format!("failed to write TLS key: {e}"))?;

        Ok(Self { cert_pem, key_pem })
    }

    /// Compute the SHA-256 fingerprint of the certificate, formatted as
    /// colon-separated hex bytes. This is what the pairing QR exposes for
    /// manual user verification.
    pub fn fingerprint(&self) -> String {
        cert_fingerprint(&self.cert_pem)
    }
}

/// Compute the SHA-256 fingerprint of a PEM-encoded certificate.
pub fn cert_fingerprint(cert_pem: &[u8]) -> String {
    // Parse PEM to DER so we fingerprint the cert bytes, not the base64 wrapper.
    let der = rustls_pemfile::certs(&mut &cert_pem[..])
        .next()
        .and_then(|r| r.ok())
        .map(|cert: CertificateDer<'_>| cert.as_ref().to_vec())
        .unwrap_or_default();

    let hash = ring::digest::digest(&ring::digest::SHA256, &der);
    hash.as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Build a rustls [`ServerConfig`] from PEM-encoded cert + key.
pub fn rustls_server_config(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<rustls::ServerConfig, String> {
    let cert_chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .filter_map(|r| r.ok())
        .collect();
    if cert_chain.is_empty() {
        return Err("no certificates found in cert PEM".to_string());
    }

    let key = rustls_pemfile::private_key(&mut &key_pem[..])
        .map_err(|e| format!("failed to parse private key: {e}"))?
        .ok_or_else(|| "no private key found in key PEM".to_string())?;

    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| format!("failed to build rustls config: {e}"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// Write `data` to `path` with mode 0600, creating the parent directory if
/// needed.
fn write_file_0600(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, data)?;
    let perm = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "alarm_tls_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn generates_self_signed_cert_on_first_boot() {
        let dir = temp_data_dir();
        let cert = TlsCert::ensure(&dir).expect("ensure should generate cert");

        assert!(!cert.cert_pem.is_empty());
        assert!(!cert.key_pem.is_empty());
        assert!(TlsCert::cert_path(&dir).exists());
        assert!(TlsCert::key_path(&dir).exists());

        let mode = fs::metadata(TlsCert::cert_path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cert must be mode 0600");
        let mode = fs::metadata(TlsCert::key_path(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key must be mode 0600");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuses_existing_cert() {
        let dir = temp_data_dir();
        let first = TlsCert::ensure(&dir).unwrap();
        let second = TlsCert::ensure(&dir).unwrap();
        assert_eq!(first.cert_pem, second.cert_pem);
        assert_eq!(first.key_pem, second.key_pem);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_is_sha256_hex_colons() {
        let dir = temp_data_dir();
        let cert = TlsCert::ensure(&dir).unwrap();
        let fp = cert.fingerprint();
        let bytes: Vec<&str> = fp.split(':').collect();
        assert_eq!(bytes.len(), 32);
        for b in bytes {
            assert_eq!(b.len(), 2);
            assert!(u8::from_str_radix(b, 16).is_ok());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lan_ip_added_to_sans_on_first_boot() {
        let dir = temp_data_dir();
        let cert = TlsCert::ensure_with_lan_ip(&dir, Some("10.0.1.61"))
            .expect("ensure should generate cert");
        // Decode the PEM to DER and look for the LAN IP as a DER-encoded
        // IPAddress SAN (4 octets: 0a 00 01 3d) so the IP fallback URL
        // (https://<ip>:<port>/…) validates without a hostname mismatch.
        let der = rustls_pemfile::certs(&mut cert.cert_pem.as_slice())
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>()
            .pop()
            .expect("at least one cert");
        assert!(
            der.as_ref().windows(4).any(|w| w == [0x0a, 0x00, 0x01, 0x3d]),
            "LAN IP 10.0.1.61 should be in cert SANs"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
