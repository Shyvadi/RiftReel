//! TLS for Riot's local endpoints.
//!
//! The game (`127.0.0.1:2999`) and the League client (LCU) both serve the same leaf,
//! `CN=rclient` with SANs `localhost` and `127.0.0.1`, issued by Riot's "LoL Game
//! Engineering Certificate Authority" (captured 2026-10-15, see `fixtures/tls/`).
//! Downplayed trusts that one root (vendored as `certs/riotgames.pem`) and nothing
//! else, and verifies strictly: chain, validity, key usage and the `127.0.0.1` name.
//! There are no system roots and no "accept invalid certificates" switch.

use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

/// Riot's root certificate. Source and fingerprint: `certs/README.md`.
pub const RIOT_ROOT_PEM: &[u8] = include_bytes!("../../certs/riotgames.pem");

#[derive(Debug, thiserror::Error)]
pub enum TlsSetupError {
    #[error("no certificate found in the root PEM")]
    NoCertificates,
    #[error("unreadable root PEM: {0}")]
    Pem(String),
    #[error("root certificate rejected: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("could not build the certificate verifier: {0}")]
    Verifier(#[from] rustls::client::VerifierBuilderError),
}

/// Client config for Riot's local endpoints: trusts only [`RIOT_ROOT_PEM`].
pub fn riot_client_config() -> Result<ClientConfig, TlsSetupError> {
    client_config_for_root(RIOT_ROOT_PEM)
}

/// Client config trusting only the certificates in `root_pem`. Tests use it with a
/// throwaway CA; production code uses [`riot_client_config`].
pub fn client_config_for_root(root_pem: &[u8]) -> Result<ClientConfig, TlsSetupError> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_webpki_verifier(verifier_for_root(root_pem)?)
        .with_no_client_auth();
    Ok(config)
}

/// The certificate verifier behind [`client_config_for_root`].
pub fn verifier_for_root(root_pem: &[u8]) -> Result<Arc<WebPkiServerVerifier>, TlsSetupError> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(root_pem) {
        // Riot's root is an X.509 v1 certificate; webpki accepts v1 trust anchors.
        roots.add(cert.map_err(|e| TlsSetupError::Pem(e.to_string()))?)?;
    }
    if roots.is_empty() {
        return Err(TlsSetupError::NoCertificates);
    }
    Ok(WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider()).build()?)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{ServerName, UnixTime};
    use rustls::CertificateError;

    use super::*;

    /// 2026-10-15, inside the validity of both the Riot root and the captured leaf.
    fn capture_day() -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_secs(1_792_022_400))
    }

    fn loopback() -> ServerName<'static> {
        ServerName::try_from("127.0.0.1").unwrap().to_owned()
    }

    /// The leaf both the League client and the game served on 2026-10-15.
    fn captured_riot_leaf() -> CertificateDer<'static> {
        let pem = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixtures/tls/rclient-leaf-2026.pem"
        ))
        .unwrap();
        CertificateDer::from_pem_slice(&pem).unwrap()
    }

    #[test]
    fn riot_root_loads_as_trust_anchor() {
        riot_client_config().expect("Riot's v1 root must load");
    }

    #[test]
    fn real_riot_leaf_passes_strict_verification_for_loopback() {
        let verifier = verifier_for_root(RIOT_ROOT_PEM).unwrap();
        verifier
            .verify_server_cert(&captured_riot_leaf(), &[], &loopback(), &[], capture_day())
            .expect("leaf chains to riotgames.pem and names 127.0.0.1");
    }

    #[test]
    fn real_riot_leaf_is_not_valid_for_other_names() {
        let verifier = verifier_for_root(RIOT_ROOT_PEM).unwrap();
        let name = ServerName::try_from("example.com").unwrap();
        assert!(verifier
            .verify_server_cert(&captured_riot_leaf(), &[], &name, &[], capture_day())
            .is_err());
    }

    struct TestCa {
        pem: String,
        issuer: Issuer<'static, KeyPair>,
    }

    fn test_ca(name: &str) -> TestCa {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        TestCa {
            pem: cert.pem(),
            issuer: Issuer::new(params, key),
        }
    }

    fn leaf(ca: &TestCa, sans: &[&str]) -> CertificateDer<'static> {
        let mut params =
            CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "rclient");
        let key = KeyPair::generate().unwrap();
        params.signed_by(&key, &ca.issuer).unwrap().der().clone()
    }

    #[test]
    fn rejects_a_leaf_from_another_ca() {
        let trusted = test_ca("trusted");
        let other = test_ca("other");
        let verifier = verifier_for_root(trusted.pem.as_bytes()).unwrap();
        let err = verifier
            .verify_server_cert(
                &leaf(&other, &["127.0.0.1"]),
                &[],
                &loopback(),
                &[],
                UnixTime::now(),
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer)
            ),
            "{err:?}"
        );
    }

    #[test]
    fn riot_root_rejects_foreign_leaves() {
        let other = test_ca("other");
        let verifier = verifier_for_root(RIOT_ROOT_PEM).unwrap();
        let foreign = leaf(&other, &["127.0.0.1"]);
        assert!(verifier
            .verify_server_cert(&foreign, &[], &loopback(), &[], UnixTime::now())
            .is_err());
    }

    #[test]
    fn a_leaf_that_does_not_name_127_0_0_1_is_rejected() {
        let ca = test_ca("trusted");
        let verifier = verifier_for_root(ca.pem.as_bytes()).unwrap();
        let err = verifier
            .verify_server_cert(
                &leaf(&ca, &["rclient"]),
                &[],
                &loopback(),
                &[],
                UnixTime::now(),
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                rustls::Error::InvalidCertificate(
                    CertificateError::NotValidForName
                        | CertificateError::NotValidForNameContext { .. }
                )
            ),
            "{err:?}"
        );
    }
}
