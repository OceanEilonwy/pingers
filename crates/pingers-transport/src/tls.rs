// SPDX-License-Identifier: AGPL-3.0-only
//! TLS for every connection: TLS 1.3 only, ALPN `h2`, and the X25519MLKEM768 hybrid key
//! exchange offered first, so a server that supports it gets a post-quantum handshake.

use std::sync::Arc;

use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

use crate::TransportError;

/// The ALPN protocol every connection asks for.
pub(crate) const ALPN_H2: &[u8] = b"h2";

/// The certificate authorities trusted for HTTPS (relays and the direct API).
#[derive(Clone, Debug)]
pub struct TlsRoots {
    store: Arc<RootCertStore>,
}

impl TlsRoots {
    /// Mozilla's root program, as shipped in `webpki-roots`.
    #[must_use]
    pub fn mozilla() -> Self {
        let store = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self {
            store: Arc::new(store),
        }
    }

    /// Exactly the given DER-encoded certificates, for a self-hosted server with its own CA.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidCertificate`] if a certificate does not parse as a trust
    /// anchor.
    pub fn from_der(certificates: &[Vec<u8>]) -> Result<Self, TransportError> {
        let mut store = RootCertStore::empty();
        for der in certificates {
            store
                .add(CertificateDer::from(der.clone()))
                .map_err(|_| TransportError::InvalidCertificate)?;
        }
        Ok(Self {
            store: Arc::new(store),
        })
    }
}

/// The key exchange groups offered, best first. X25519MLKEM768 is the hybrid post-quantum
/// group (architecture §3); the classical ones follow for servers that lack it.
fn provider() -> CryptoProvider {
    CryptoProvider {
        kx_groups: vec![
            aws_lc_rs::kx_group::X25519MLKEM768,
            aws_lc_rs::kx_group::X25519,
            aws_lc_rs::kx_group::SECP256R1,
        ],
        ..aws_lc_rs::default_provider()
    }
}

/// The client configuration every connection of a transport shares.
pub(crate) fn client_config(roots: &TlsRoots) -> Result<Arc<ClientConfig>, TransportError> {
    let mut config = ClientConfig::builder_with_provider(Arc::new(provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| TransportError::InvalidCertificate)?
        .with_root_certificates(Arc::clone(&roots.store))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN_H2.to_vec()];
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn the_hybrid_group_is_offered_first() {
        let names: Vec<_> = provider()
            .kx_groups
            .iter()
            .map(|group| group.name())
            .collect();
        assert_eq!(
            names,
            [
                rustls::NamedGroup::X25519MLKEM768,
                rustls::NamedGroup::X25519,
                rustls::NamedGroup::secp256r1
            ]
        );
    }

    #[test]
    fn connections_ask_for_h2() -> Result<(), TransportError> {
        let config = client_config(&TlsRoots::from_der(&[])?)?;
        assert_eq!(config.alpn_protocols, [b"h2".to_vec()]);
        Ok(())
    }

    #[test]
    fn mozilla_roots_are_not_empty_and_garbage_is_refused() {
        assert!(!TlsRoots::mozilla().store.is_empty());
        assert!(matches!(
            TlsRoots::from_der(&[vec![1, 2, 3]]),
            Err(TransportError::InvalidCertificate)
        ));
    }
}
