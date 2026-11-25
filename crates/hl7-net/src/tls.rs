//! TLS helpers for tokio-rustls client and server configurations (NET-17, NET-34, D-23).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::error::NetError;
use crate::profile::{ListenerTlsConfig, SenderTlsConfig};

/// Build a tokio-rustls `TlsAcceptor` from listener configuration (NET-17, D-23).
pub fn build_tls_acceptor(config: &ListenerTlsConfig) -> Result<TlsAcceptor, NetError> {
    let cert_path = config.cert_chain_path.as_ref().ok_or_else(|| {
        NetError::Io("Missing certificate chain path for listener TLS".to_string())
    })?;
    let key_path = config
        .private_key_path
        .as_ref()
        .ok_or_else(|| NetError::Io("Missing private key path for listener TLS".to_string()))?;

    let certs = load_certs_from_pem(cert_path)?;
    let key = load_key_from_pem(key_path)?;

    let mut server_config = match &config.client_ca_path {
        Some(ca_path) => {
            let ca_certs = load_certs_from_pem(ca_path)?;
            let mut roots = RootCertStore::empty();
            for c in ca_certs {
                roots
                    .add(c)
                    .map_err(|e| NetError::Io(format!("Failed adding client CA: {e}")))?;
            }
            let client_verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|e| NetError::Io(format!("Failed building client verifier: {e}")))?;
            ServerConfig::builder()
                .with_client_cert_verifier(client_verifier)
                .with_single_cert(certs, key)
                .map_err(|e| NetError::Io(format!("TLS config error: {e}")))?
        }
        None => ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| NetError::Io(format!("TLS config error: {e}")))?,
    };

    server_config.alpn_protocols = vec![b"mllp".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Build a tokio-rustls `TlsConnector` from sender configuration (NET-34, D-23).
pub fn build_tls_connector(config: &SenderTlsConfig) -> Result<TlsConnector, NetError> {
    let mut roots = RootCertStore::empty();

    if let Some(custom_ca) = &config.custom_ca_path {
        let ca_certs = load_certs_from_pem(custom_ca)?;
        for c in ca_certs {
            roots
                .add(c)
                .map_err(|e| NetError::Io(format!("Failed adding custom CA: {e}")))?;
        }
    }

    let client_config_builder = ClientConfig::builder();

    let client_config = if config.allow_invalid_certificates {
        // Opt-in for local test environments
        #[derive(Debug)]
        struct NoCertVerifier;
        impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
            fn verify_server_cert(
                &self,
                _end_entity: &CertificateDer<'_>,
                _intermediates: &[CertificateDer<'_>],
                _server_name: &ServerName<'_>,
                _ocsp_response: &[u8],
                _now: rustls::pki_types::UnixTime,
            ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
                vec![
                    rustls::SignatureScheme::RSA_PKCS1_SHA256,
                    rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                    rustls::SignatureScheme::ED25519,
                ]
            }
        }

        client_config_builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
    } else {
        client_config_builder.with_root_certificates(roots)
    };

    let client_config = match (&config.client_cert_path, &config.client_key_path) {
        (Some(cert_path), Some(key_path)) => {
            let certs = load_certs_from_pem(cert_path)?;
            let key = load_key_from_pem(key_path)?;
            client_config
                .with_client_auth_cert(certs, key)
                .map_err(|e| NetError::Io(format!("Failed configuring mTLS client cert: {e}")))?
        }
        _ => client_config.with_no_client_auth(),
    };

    Ok(TlsConnector::from(Arc::new(client_config)))
}

/// Load DER certificates from a PEM file.
pub fn load_certs_from_pem(path: &Path) -> Result<Vec<CertificateDer<'static>>, NetError> {
    let file = File::open(path).map_err(|e| {
        NetError::Io(format!(
            "Failed opening certificate file {}: {e}",
            path.display()
        ))
    })?;
    let mut reader = BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| NetError::Io(format!("Failed parsing certs from {}: {e}", path.display())))?;
    Ok(certs)
}

/// Load private key from a PEM file.
pub fn load_key_from_pem(path: &Path) -> Result<PrivateKeyDer<'static>, NetError> {
    let file = File::open(path).map_err(|e| {
        NetError::Io(format!(
            "Failed opening private key file {}: {e}",
            path.display()
        ))
    })?;
    let mut reader = BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .map_err(|e| {
            NetError::Io(format!(
                "Failed parsing private key from {}: {e}",
                path.display()
            ))
        })?
        .ok_or_else(|| NetError::Io(format!("No private key found in {}", path.display())))?;
    Ok(key)
}
