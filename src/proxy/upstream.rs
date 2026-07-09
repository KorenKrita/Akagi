//! Upstream connector for the MITM proxy.
//!
//! Why this isn't `hudsucker::Proxy::builder().with_rustls_connector(...)`:
//! hudsucker's built-in rustls connector calls `with_webpki_roots()` and does
//! strict TLS validation against Mozilla's CA bundle. The Mahjong Soul Steam
//! client (catfood studio build) fetches its `clientBundleSettings` JSON from
//! shared CDN IPs (EdgeNext / ChinaNetCenter / Aliyun OSS) using URLs of the
//! form `https://156.238.128.60:443/...` — the client resolves DNS itself
//! and bakes the IP literal into the URL. Those CDNs serve a default cert
//! for hostnames they front (`*.edgenext.com`, `*.chinanetcenter.com`,
//! `*.aliyuncs.com`, …) — never an IP SAN. The game's own Unity build
//! ships a `CertificateHandler` that accepts anything, so it talks to those
//! IPs fine when there's no proxy in the way. Once we intercept, our
//! upstream client validates strictly, fails, returns 502, and the game
//! retries each IP in a loop until it pops the "獲取[配置初始化文件]失敗
//! [3010]" dialog.
//!
//! The fix is to match the game's own behavior: skip server-cert validation
//! on the proxy → upstream leg. The client → proxy leg is unaffected — the
//! client still validates our MITM cert against the Akagi CA the user
//! installed, so the local security boundary doesn't change.

use anyhow::{Context, Result};
use hudsucker::{
    hyper::{HeaderMap, Uri},
    hyper_util::client::legacy::connect::proxy::Tunnel,
    hyper_util::client::legacy::connect::{Connect, HttpConnector},
    rustls::{
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        crypto::aws_lc_rs,
        pki_types::{CertificateDer, ServerName, UnixTime},
        ClientConfig, DigitallySignedStruct, SignatureScheme,
    },
    tokio_tungstenite::Connector as WsConnector,
};
use std::sync::Arc;

/// `ServerCertVerifier` that approves every chain. See module docs for
/// the threat model justification.
#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, hudsucker::rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, hudsucker::rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, hudsucker::rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}

/// Shared rustls `ClientConfig`. aws-lc-rs as the crypto provider to match
/// hudsucker's `RcgenAuthority` (so we don't load two different providers
/// in the process). TLS 1.2 + TLS 1.3 enabled, no client cert auth.
fn client_config() -> Arc<ClientConfig> {
    let provider = aws_lc_rs::default_provider();
    let cfg = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .expect("aws-lc-rs supports both TLS 1.2 and 1.3 — protocol version selection cannot fail")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();
    Arc::new(cfg)
}

/// HTTPS connector for the proxy's upstream HTTP/WS leg. HTTP/1 + HTTP/2
/// both enabled; ALPN is negotiated per-connection by hyper-rustls.
pub fn http_connector(
    upstream_proxy: Option<Uri>,
) -> Result<impl Connect + Clone + Send + Sync + 'static> {
    let config = client_config();
    let http = match upstream_proxy {
        Some(proxy) => {
            validate_upstream_proxy(&proxy)?;
            EitherConnector::Tunnel(
                Tunnel::new(proxy, HttpConnector::new()).with_headers(HeaderMap::new()),
            )
        }
        None => EitherConnector::Direct(HttpConnector::new()),
    };
    Ok(hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config((*config).clone())
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http))
}

/// WebSocket connector for upstream WS upgrades originating from the
/// proxy. Same `ClientConfig` as `http_connector` so the same NoVerify
/// policy applies.
pub fn websocket_connector() -> WsConnector {
    WsConnector::Rustls(client_config())
}

#[derive(Clone)]
enum EitherConnector {
    Direct(HttpConnector),
    Tunnel(Tunnel<HttpConnector>),
}

impl tower_service::Service<Uri> for EitherConnector {
    type Response = hudsucker::hyper_util::rt::TokioIo<tokio::net::TcpStream>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match self {
            Self::Direct(c) => {
                tower_service::Service::poll_ready(c, cx).map_err(|e| Box::new(e) as Self::Error)
            }
            Self::Tunnel(c) => {
                tower_service::Service::poll_ready(c, cx).map_err(|e| Box::new(e) as Self::Error)
            }
        }
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        match self {
            Self::Direct(c) => {
                let fut = tower_service::Service::call(c, dst);
                Box::pin(async move { fut.await.map_err(|e| Box::new(e) as Self::Error) })
            }
            Self::Tunnel(c) => {
                let fut = tower_service::Service::call(c, dst);
                Box::pin(async move { fut.await.map_err(|e| Box::new(e) as Self::Error) })
            }
        }
    }
}

fn validate_upstream_proxy(uri: &Uri) -> Result<()> {
    match uri.scheme_str() {
        Some("http") => {}
        Some(other) => {
            anyhow::bail!("unsupported upstream proxy scheme `{other}`; use http://host:port")
        }
        None => {
            anyhow::bail!("upstream proxy must include a scheme, for example http://127.0.0.1:7890")
        }
    }
    uri.host()
        .filter(|h| !h.is_empty())
        .context("upstream proxy must include a host")?;
    Ok(())
}
