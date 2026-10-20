//! Outbound connections to targets: direct, or through an upstream proxy
//! (HTTP or SOCKS5: corporate proxy, Tor, another intercepting tool).

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use anyhow::{anyhow, bail, Result};
use http::{uri::Scheme, HeaderValue, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{
        connect::{Connected, Connection},
        Client,
    },
    rt::{TokioExecutor, TokioIo},
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
};
use tower_service::Service;

use crate::proxy::Body;

pub type HttpClient = Client<HttpsConnector<Connector>, Body>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Upstream proxy protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Ordinary HTTP proxy (absolute-form for http, CONNECT for https).
    Http,
    /// SOCKS5 (RFC 1928), with optional username/password (RFC 1929).
    Socks5,
}

/// An upstream proxy every request is sent through.
#[derive(Clone, Debug, PartialEq)]
pub struct UpstreamProxy {
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl UpstreamProxy {
    /// `http://` or `socks5://[user:pass@]host:port` (scheme optional, default
    /// http); empty means connect directly. `socks5h` is accepted as an alias.
    pub fn parse(url: &str) -> Result<Option<Self>> {
        let url = url.trim();
        if url.is_empty() {
            return Ok(None);
        }
        let full = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
        let uri: Uri = full.parse().map_err(|e| anyhow!("Invalid proxy URL: {e}"))?;
        let kind = match uri.scheme_str() {
            Some("http") => Kind::Http,
            Some("socks5") | Some("socks5h") => Kind::Socks5,
            _ => bail!("Only http:// and socks5:// upstream proxies are supported"),
        };
        let authority = uri.authority().ok_or_else(|| anyhow!("Proxy host is empty"))?;
        let host = authority.host().trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            bail!("Proxy host is empty");
        }
        let port = authority
            .port_u16()
            .ok_or_else(|| anyhow!("Proxy port is required, e.g. http://127.0.0.1:8081"))?;
        let (username, password) = match authority.as_str().rsplit_once('@') {
            Some((userinfo, _)) => {
                let creds = percent_decode(userinfo);
                let text = String::from_utf8_lossy(&creds);
                match text.split_once(':') {
                    Some((u, pw)) => (Some(u.to_string()), Some(pw.to_string())),
                    None => (Some(text.into_owned()), None),
                }
            }
            None => (None, None),
        };
        Ok(Some(Self {
            kind,
            host: host.to_string(),
            port,
            username,
            password,
        }))
    }

    /// `Proxy-Authorization: Basic ...` for an HTTP proxy with credentials.
    pub fn http_auth(&self) -> Option<HeaderValue> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let user = self.username.as_deref()?;
        let creds = format!("{user}:{}", self.password.as_deref().unwrap_or(""));
        HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(creds))).ok()
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Client for reaching targets, optionally through `proxy`. With `http2`,
/// TLS offers `h2` via ALPN (falling back to HTTP/1.1 if the server doesn't
/// take it); otherwise only HTTP/1.1 is spoken. Upstream certificates are not
/// validated (see [`AcceptAnyCert`]).
pub fn client(proxy: Option<Arc<UpstreamProxy>>, http2: bool) -> Result<HttpClient> {
    let builder = HttpsConnectorBuilder::new()
        .with_tls_config(insecure_tls_config()?)
        .https_or_http();
    let connector = Connector { proxy };
    let https = if http2 {
        builder.enable_all_versions().wrap_connector(connector)
    } else {
        builder.enable_http1().wrap_connector(connector)
    };

    Ok(Client::builder(TokioExecutor::new())
        .http1_title_case_headers(true)
        .http1_preserve_header_case(true)
        .build(https))
}

pub fn insecure_tls_config() -> Result<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth())
}

/// TCP connector used under TLS. With an upstream proxy, `https` targets get a
/// CONNECT tunnel; plain `http` goes to the proxy in absolute-form.
#[derive(Clone)]
pub struct Connector {
    proxy: Option<Arc<UpstreamProxy>>,
}

impl Service<Uri> for Connector {
    type Response = TokioIo<Stream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let proxy = self.proxy.clone();
        Box::pin(async move {
            let host = dst
                .host()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "URI without host"))?
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_string();
            let https = dst.scheme() == Some(&Scheme::HTTPS);
            let port = dst.port_u16().unwrap_or(if https { 443 } else { 80 });

            let stream = match proxy.as_deref() {
                None => Stream {
                    tcp: connect(&host, port).await?,
                    proxied: false,
                },
                // SOCKS5 tunnels straight to the target, so requests are origin-form
                // (proxied = false) and TLS, when needed, is layered on top by the caller.
                Some(p) if p.kind == crate::upstream::Kind::Socks5 => {
                    let tcp = socks5_connect(p, &host, port).await.map_err(|e| {
                        io::Error::new(e.kind(), format!("socks5 proxy {}:{}: {e}", p.host, p.port))
                    })?;
                    Stream { tcp, proxied: false }
                }
                Some(p) => {
                    let mut tcp = connect(&p.host, p.port).await.map_err(|e| {
                        io::Error::new(e.kind(), format!("upstream proxy {}:{}: {e}", p.host, p.port))
                    })?;
                    if https {
                        tunnel(&mut tcp, &host, port, p.http_auth().as_ref()).await?;
                    }
                    Stream {
                        tcp,
                        proxied: !https,
                    }
                }
            };
            Ok(TokioIo::new(stream))
        })
    }
}

async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("connect {host}:{port} timed out")))??;
    let _ = tcp.set_nodelay(true);
    Ok(tcp)
}

/// Open a connection to `host:port` through a SOCKS5 proxy (RFC 1928), using
/// username/password auth (RFC 1929) when the proxy offers it and creds are set.
async fn socks5_connect(p: &UpstreamProxy, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut tcp = connect(&p.host, p.port).await?;

    // Greeting: offer no-auth, plus user/pass when we have credentials.
    let has_creds = p.username.is_some();
    let methods: &[u8] = if has_creds { &[0x00, 0x02] } else { &[0x00] };
    let mut greet = vec![0x05, methods.len() as u8];
    greet.extend_from_slice(methods);
    tcp.write_all(&greet).await?;

    let mut sel = [0u8; 2];
    tcp.read_exact(&mut sel).await?;
    if sel[0] != 0x05 {
        return Err(io::Error::other("not a SOCKS5 proxy"));
    }
    match sel[1] {
        0x00 => {}
        0x02 => {
            let user = p.username.clone().unwrap_or_default();
            let pass = p.password.clone().unwrap_or_default();
            if user.len() > 255 || pass.len() > 255 {
                return Err(io::Error::other("SOCKS5 username/password too long"));
            }
            let mut auth = vec![0x01, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass.as_bytes());
            tcp.write_all(&auth).await?;
            let mut st = [0u8; 2];
            tcp.read_exact(&mut st).await?;
            if st[1] != 0x00 {
                return Err(io::Error::other("SOCKS5 authentication failed"));
            }
        }
        0xFF => return Err(io::Error::other("SOCKS5 proxy rejected our auth methods")),
        other => return Err(io::Error::other(format!("SOCKS5 unsupported auth method {other:#x}"))),
    }

    // CONNECT request with the target as a domain name (proxy resolves it).
    if host.len() > 255 {
        return Err(io::Error::other("SOCKS5 hostname too long"));
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&req).await?;

    // Reply: VER REP RSV ATYP BND.ADDR BND.PORT; consume the bound address.
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(io::Error::other(format!("SOCKS5 CONNECT failed (reply {:#x})", head[1])));
    }
    let addr_len = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut n = [0u8; 1];
            tcp.read_exact(&mut n).await?;
            n[0] as usize
        }
        other => return Err(io::Error::other(format!("SOCKS5 unknown address type {other:#x}"))),
    };
    let mut skip = vec![0u8; addr_len + 2];
    tcp.read_exact(&mut skip).await?;
    Ok(tcp)
}

/// Ask the upstream proxy for a raw tunnel to `host:port`.
async fn tunnel(tcp: &mut TcpStream, host: &str, port: u16, auth: Option<&HeaderValue>) -> io::Result<()> {
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(auth) = auth.and_then(|v| v.to_str().ok()) {
        req.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    req.push_str("\r\n");
    tcp.write_all(req.as_bytes()).await?;

    // Byte by byte, so nothing after the reply head is consumed.
    let mut head = Vec::with_capacity(128);
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(io::Error::other("upstream proxy: reply header too large"));
        }
        head.push(tcp.read_u8().await?);
    }
    let head = String::from_utf8_lossy(&head);
    let status_line = head.lines().next().unwrap_or_default();
    if !status_line.split_whitespace().nth(1).is_some_and(|s| s.starts_with('2')) {
        return Err(io::Error::other(format!(
            "upstream proxy menolak CONNECT {target}: {status_line}"
        )));
    }
    Ok(())
}

/// A TCP stream that remembers whether it leads to a proxy (absolute-form requests).
pub struct Stream {
    tcp: TcpStream,
    proxied: bool,
}

impl Connection for Stream {
    fn connected(&self) -> Connected {
        self.tcp.connected().proxy(self.proxied)
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().tcp).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().tcp).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().tcp).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.tcp.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().tcp).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().tcp).poll_shutdown(cx)
    }
}

/// A pentest proxy must reach targets with self-signed/expired certs, so
/// upstream certificates are not validated (signatures still are).
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
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
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proxy_urls() {
        assert_eq!(UpstreamProxy::parse("  ").unwrap(), None);

        let p = UpstreamProxy::parse("127.0.0.1:8081").unwrap().unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port, p.http_auth()), (Kind::Http, "127.0.0.1", 8081, None));

        let p = UpstreamProxy::parse("http://bob:p%40ss@corp.local:3128").unwrap().unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port), (Kind::Http, "corp.local", 3128));
        // base64("bob:p@ss")
        assert_eq!(p.http_auth().unwrap(), "Basic Ym9iOnBAc3M=");

        let p = UpstreamProxy::parse("socks5://tor:9050").unwrap().unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port), (Kind::Socks5, "tor", 9050));
        let p = UpstreamProxy::parse("socks5h://u:pw@127.0.0.1:1080").unwrap().unwrap();
        assert_eq!((p.kind, p.username.as_deref(), p.password.as_deref()), (Kind::Socks5, Some("u"), Some("pw")));

        assert!(UpstreamProxy::parse("https://corp.local:3128").is_err());
        assert!(UpstreamProxy::parse("http://corp.local").is_err(), "port required");
    }
}
