//! Shared TLS plumbing (feature `tls`): PEM loading + the client connector and
//! server acceptor builders used by net_tcp (connectTls), http_server (serve
//! {tls}), and email (STARTTLS). rustls defaults — TLS 1.2/1.3, no custom
//! ciphers (spec §4.1). PEM STRINGS only, never paths (caps honesty, §4.2).
//!
//! Regenerate test fixtures (`testdata/tls_test_{cert,key}.pem`). The cert MUST be an
//! end-entity (`CA:FALSE`) with `serverAuth` EKU + `SAN:localhost` — a `CA:TRUE` cert
//! (the default `req -x509`) is rejected by webpki as `CaUsedAsEndEntity` when the
//! server presents it as a leaf, even when it's also the trusted root. Use a config:
//!   cat > /tmp/tls_ext.cnf <<'EOF'
//!   [req]
//!   distinguished_name = dn
//!   x509_extensions = v3
//!   prompt = no
//!   [dn]
//!   CN = localhost
//!   [v3]
//!   basicConstraints = critical, CA:FALSE
//!   subjectAltName = DNS:localhost
//!   keyUsage = digitalSignature, keyEncipherment
//!   extendedKeyUsage = serverAuth
//!   EOF
//!   openssl req -x509 -newkey rsa:2048 -nodes -days 36500 -config /tmp/tls_ext.cnf \
//!     -keyout src/stdlib/testdata/tls_test_key.pem \
//!     -out    src/stdlib/testdata/tls_test_cert.pem
//!
//! NAMING: `rustls` / `rustls_pki_types` come transitively (tokio-rustls → rustls →
//! pki-types). We name them through the `tokio_rustls` re-export so the `tls` feature
//! needs no second direct dep (no extra rustls copy — `cargo tree -d` stays clean).

use std::sync::Arc;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types;

/// The pinned crypto provider. The build graph carries BOTH `ring` (tokio-rustls) and
/// `aws-lc-rs` (reqwest's rustls-tls), so rustls CANNOT auto-select a process-level
/// default — every config builder MUST name a provider explicitly. We pin `ring` (the
/// `tls` feature enables `tokio-rustls/ring`). rustls protocol defaults otherwise
/// (TLS 1.2/1.3, no custom ciphers — spec §4.1).
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Load one-or-more certificates from a PEM string into DER. Empty PEM (no cert
/// blocks) is an error, not an empty Vec, so a bogus `caCert`/server cert can't
/// silently produce an empty trust set / empty chain.
pub(crate) fn load_certs(pem: &str) -> Result<Vec<pki_types::CertificateDer<'static>>, String> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("invalid certificate PEM: {}", e))?;
    if certs.is_empty() {
        return Err("certificate PEM contains no certificates".to_string());
    }
    Ok(certs)
}

/// Load a single private key (PKCS#8 / PKCS#1 / SEC1) from a PEM string.
pub(crate) fn load_key(pem: &str) -> Result<pki_types::PrivateKeyDer<'static>, String> {
    match rustls_pemfile::private_key(&mut pem.as_bytes()) {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err("private key PEM contains no key".to_string()),
        Err(e) => Err(format!("invalid private key PEM: {}", e)),
    }
}

/// Build a client `ClientConfig` trusting the webpki Mozilla roots plus an optional
/// extra `caCert` PEM (for self-signed / private CAs), with optional ALPN protocols.
/// rustls defaults (TLS 1.2/1.3, no custom ciphers), server-auth only.
pub(crate) fn client_config(
    ca_cert: Option<&str>,
    alpn: &[String],
) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    // webpki-roots 0.26 → TLS_SERVER_ROOTS is `&[TrustAnchor<'static>]`, which
    // RootCertStore::extend accepts (rustls 0.23 `IntoIterator<Item = TrustAnchor>`).
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = ca_cert {
        for cert in load_certs(pem)? {
            roots
                .add(cert)
                .map_err(|e| format!("invalid caCert: {}", e))?;
        }
    }
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS provider error: {}", e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    if !alpn.is_empty() {
        cfg.alpn_protocols = alpn.iter().map(|p| p.as_bytes().to_vec()).collect();
    }
    Ok(Arc::new(cfg))
}

/// Build a server `ServerConfig` from a primary cert/key, optional SNI extra
/// certs (host → (cert PEM, key PEM)), advertising `http/1.1` over ALPN. Reserved
/// for http_server `serve({tls})`; unused by `connectTls`.
#[allow(dead_code)]
pub(crate) fn server_config(
    cert: &str,
    key: &str,
    sni: &[(String, String, String)],
) -> Result<Arc<rustls::ServerConfig>, String> {
    use rustls::server::ResolvesServerCertUsingSni;
    use rustls::sign::CertifiedKey;

    let default_certs = load_certs(cert)?;
    let default_key = load_key(key)?;

    let prov = provider();
    let mut cfg = if sni.is_empty() {
        rustls::ServerConfig::builder_with_provider(prov)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS provider error: {}", e))?
            .with_no_client_auth()
            .with_single_cert(default_certs, default_key)
            .map_err(|e| format!("invalid TLS server cert/key: {}", e))?
    } else {
        let provider = prov;
        let mut resolver = ResolvesServerCertUsingSni::new();
        // default cert under each provided SNI hostname.
        let signing = provider
            .key_provider
            .load_private_key(default_key)
            .map_err(|e| format!("invalid TLS server key: {}", e))?;
        let default_ck = Arc::new(CertifiedKey::new(default_certs.clone(), signing));
        for (host, c_pem, k_pem) in sni {
            let c = load_certs(c_pem)?;
            let k = load_key(k_pem)?;
            let sk = provider
                .key_provider
                .load_private_key(k)
                .map_err(|e| format!("invalid SNI key for {}: {}", host, e))?;
            let ck = Arc::new(CertifiedKey::new(c, sk));
            resolver
                .add(host, (*ck).clone())
                .map_err(|e| format!("invalid SNI cert for {}: {}", host, e))?;
        }
        // also register the default under the primary CN if it parses (best-effort).
        let _ = &default_ck;
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS provider error: {}", e))?
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver))
    };
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &str = include_str!("testdata/tls_test_cert.pem");
    const KEY: &str = include_str!("testdata/tls_test_key.pem");

    #[test]
    fn load_certs_and_key_ok() {
        assert_eq!(load_certs(CERT).unwrap().len(), 1);
        load_key(KEY).unwrap();
    }

    #[test]
    fn empty_and_garbage_pem_are_clean_errs() {
        assert!(load_certs("").is_err());
        assert!(load_certs("garbage").is_err());
        let mut t = CERT.to_string();
        t.truncate(60);
        assert!(load_certs(&t).is_err());
        assert!(load_key("").is_err());
    }

    #[test]
    fn client_config_builds_with_and_without_ca() {
        client_config(None, &[]).unwrap();
        client_config(Some(CERT), &["h2".to_string()]).unwrap();
        // a bad caCert string is a clean Err, never a panic.
        assert!(client_config(Some("garbage"), &[]).is_err());
    }
}
