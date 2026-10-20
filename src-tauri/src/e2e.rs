//! End-to-end tests: a real proxy listener in front of a local origin server.

use std::{
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::BodyExt;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::{
    body::{self, CAPTURE_LIMIT},
    ca::CertAuthority,
    proxy::{self, Body, Ctx, Events},
    store::Store,
    upstream::{self, UpstreamProxy},
};

pub type EventLog = Arc<Mutex<Vec<(String, Value)>>>;

struct Recorder(EventLog);

impl Events for Recorder {
    fn emit_json(&self, event: &str, payload: Value) {
        self.0.lock().unwrap().push((event.to_string(), payload));
    }
}

/// Fresh, empty directory under the system temp dir.
pub fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "intip-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn test_ctx(dir: &Path) -> Ctx {
    test_ctx_with_log(dir).0
}

pub fn test_ctx_with_log(dir: &Path) -> (Ctx, EventLog) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ca = Arc::new(CertAuthority::load_or_create(dir).unwrap());
    let store = Store::open(&dir.join("test.intip")).unwrap();
    let log = EventLog::default();
    let ctx = Ctx::new(Box::new(Recorder(log.clone())), ca, store).unwrap();
    (ctx, log)
}

pub struct Harness {
    pub ctx: Arc<Ctx>,
    pub events: EventLog,
    pub proxy: SocketAddr,
    pub origin: SocketAddr,
    _dir: TempDir,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Harness {
    pub async fn start(name: &str) -> Self {
        let dir = temp_dir(name);
        let (ctx, events) = test_ctx_with_log(&dir);
        let ctx = Arc::new(ctx);
        let proxy = spawn_proxy(ctx.clone()).await;
        Self {
            ctx,
            events,
            proxy,
            origin: spawn_origin().await,
            _dir: TempDir(dir),
        }
    }

    /// `http://127.0.0.1:<origin port><path>`
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.origin)
    }

    /// Send raw bytes to the proxy and return everything it answers.
    pub async fn raw(&self, request: &str) -> String {
        raw_exchange(self.proxy, request.as_bytes()).await
    }

    /// Plain `GET` through the proxy with `Connection: close`.
    pub async fn get(&self, url: &str) -> String {
        self.raw(&format!("GET {url} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", self.origin))
            .await
    }

    pub fn events(&self, name: &str) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, _)| e == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Wait until `n` events called `name` have been emitted.
    pub async fn wait_events(&self, name: &str, n: usize) -> Vec<Value> {
        for _ in 0..200 {
            let found = self.events(name);
            if found.len() >= n {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {n} {name:?} events; got {:?}", self.events(name));
    }
}

pub async fn spawn_proxy(ctx: Arc<Ctx>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(proxy::serve(listener, ctx));
    // `serve` records the listener address on its first poll.
    tokio::task::yield_now().await;
    addr
}

pub async fn raw_exchange(addr: SocketAddr, request: &[u8]) -> String {
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.write_all(request).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), tcp.read_to_end(&mut out))
        .await
        .expect("proxy response timed out")
        .unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

/// Demo cipher shared by the /encrypted origin endpoint and the decrypt extension
/// used in the extension e2e test: XOR with a fixed key, then base64.
const E2E_XOR_KEY: &[u8] = b"e2e-key";

fn e2e_xor_b64_encode(plain: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let x: Vec<u8> = plain.iter().enumerate().map(|(i, b)| b ^ E2E_XOR_KEY[i % E2E_XOR_KEY.len()]).collect();
    STANDARD.encode(x)
}

/// Origin server. `/echo` answers with what it received:
/// the request line, each header on its own line, a blank line, then the body.
async fn spawn_origin() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service_fn(origin))
                    .with_upgrades()
                    .await;
            });
        }
    });
    addr
}

/// TLS server config from `ca` offering only the given ALPN protocols.
fn tls_config(ca: &CertAuthority, alpn: &[&[u8]]) -> Arc<rustls::ServerConfig> {
    let mut cfg = (*ca.server_config("127.0.0.1").unwrap()).clone();
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Arc::new(cfg)
}

/// Same origin behind TLS (certificate from `ca`); speaks HTTP/2 too when `h2`.
async fn spawn_tls_origin_with(ca: &CertAuthority, h2: bool) -> SocketAddr {
    let alpn: &[&[u8]] = if h2 { &[b"h2", b"http/1.1"] } else { &[b"http/1.1"] };
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config(ca, alpn));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else { return };
                if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
                    let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(TokioIo::new(tls), service_fn(origin))
                        .await;
                } else {
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service_fn(origin))
                        .with_upgrades()
                        .await;
                }
            });
        }
    });
    addr
}

async fn spawn_tls_origin(ca: &CertAuthority) -> SocketAddr {
    spawn_tls_origin_with(ca, false).await
}

/// A client that uses `proxy` the way a browser would (absolute-form / CONNECT).
fn client_via(proxy: SocketAddr) -> upstream::HttpClient {
    let p = UpstreamProxy::parse(&proxy.to_string()).unwrap();
    upstream::client(p.map(Arc::new), false).unwrap()
}

/// Like [`client_via`], but offering HTTP/2 inside the tunnel.
fn client_via_h2(proxy: SocketAddr) -> upstream::HttpClient {
    let p = UpstreamProxy::parse(&proxy.to_string()).unwrap();
    upstream::client(p.map(Arc::new), true).unwrap()
}

async fn body_text(res: Response<Incoming>) -> String {
    String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned()
}

async fn origin(req: Request<Incoming>) -> Result<Response<Body>, Infallible> {
    let query = req.uri().query().unwrap_or_default().to_string();
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
    };
    match req.uri().path() {
        // `n` bytes of 'a', sent in 64 KiB chunks.
        "/big" => {
            let n = param("n");
            let chunks = (0..n.div_ceil(65536)).map(move |i| {
                let len = 65536.min(n - i * 65536);
                Ok::<_, Infallible>(http_body::Frame::data(Bytes::from(vec![b'a'; len])))
            });
            let body = http_body_util::StreamBody::new(futures_util::stream::iter(chunks));
            return Ok(Response::new(body::boxed(body)));
        }
        // "first", a pause of `ms`, then "second".
        "/slow" => {
            let ms = param("ms") as u64;
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<http_body::Frame<Bytes>, Infallible>>(2);
            tokio::spawn(async move {
                let _ = tx.send(Ok(http_body::Frame::data(Bytes::from_static(b"first ")))).await;
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = tx.send(Ok(http_body::Frame::data(Bytes::from_static(b"second")))).await;
            });
            let stream = futures_util::stream::unfold(rx, |mut rx| async { rx.recv().await.map(|f| (f, rx)) });
            let res = Response::builder()
                .header("content-type", "text/event-stream")
                .body(body::boxed(http_body_util::StreamBody::new(stream)))
                .unwrap();
            return Ok(res);
        }
        // An "encrypted" body (XOR key "e2e-key" + base64), for testing a decrypt
        // extension end to end. Marked with X-Encrypted so a hook can recognize it.
        "/encrypted" => {
            let plain = br#"{"flag":"SECRET-42","note":"decrypt me"}"#;
            return Ok(Response::builder()
                .header("content-type", "application/octet-stream")
                .header("x-encrypted", "xor-b64")
                .body(body::full(e2e_xor_b64_encode(plain)))
                .unwrap());
        }
        // Size of the request body (read in chunks, never buffered whole).
        "/len" => {
            let mut body = req.into_body();
            let mut n = 0;
            while let Some(Ok(frame)) = body.frame().await {
                n += frame.data_ref().map_or(0, |d| d.len());
            }
            return Ok(Response::new(body::full(n.to_string())));
        }
        _ => {}
    }

    let mut text = format!(
        "{} {} {:?}\n",
        req.method(),
        req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/"),
        req.version()
    );
    for (k, v) in req.headers() {
        text.push_str(&format!("{k}: {}\n", v.to_str().unwrap_or("?")));
    }
    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&body));
    Ok(Response::builder()
        .header("content-type", "text/plain")
        .body(body::full(text))
        .unwrap())
}

#[tokio::test]
async fn forwards_and_records() {
    let h = Harness::start("forward").await;
    let res = h.get(&h.url("/echo?x=1")).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains("GET /echo?x=1"), "{res}");

    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    assert_eq!(flows[0]["path"], "/echo?x=1");
    assert_eq!(flows[0]["status"], 200);
}

#[tokio::test]
async fn folds_split_cookie_headers_before_forwarding() {
    let h = Harness::start("cookie-fold").await;
    // A request that arrives with several separate Cookie headers - as an HTTP/2
    // client splits them, or a request replayed from History carries them - must
    // reach the origin as ONE folded `Cookie:` header. An HTTP/1.1 origin (PHP /
    // CodeIgniter) otherwise reads only the first and drops the session/CSRF cookies.
    let res = h
        .raw(&format!(
            "GET {} HTTP/1.1\r\nCookie: a=1\r\nCookie: b=2\r\nCookie: c=3\r\nConnection: close\r\n\r\n",
            h.url("/echo")
        ))
        .await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    // The origin echoes each header it received; exactly one cookie line arrives,
    // with every value joined by "; ".
    let cookie_lines: Vec<&str> = res
        .lines()
        .filter(|l| l.to_ascii_lowercase().starts_with("cookie:"))
        .collect();
    assert_eq!(cookie_lines.len(), 1, "cookies must fold into one header: {res}");
    assert!(cookie_lines[0].contains("a=1; b=2; c=3"), "{res}");
}

#[tokio::test]
async fn request_to_own_listener_gets_landing_page() {
    let h = Harness::start("loop").await;
    let res = h.get(&format!("http://{}/", h.proxy)).await;
    assert!(res.contains("Ngintip proxy"), "{res}");
    let res = h.get(&format!("http://localhost:{}/ca.pem", h.proxy.port())).await;
    assert!(res.contains("BEGIN CERTIFICATE"), "{res}");
    let res = h.raw("GET /ca.der HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
    assert!(res.contains("filename=\"ngintip-ca.cer\""), "{res}");
    assert!(res.to_ascii_lowercase().contains(&format!("content-length: {}", h.ctx.ca.der().len())), "{res}");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.events(proxy::FLOW_EVENT).is_empty(), "nothing recorded");
}

#[tokio::test]
async fn proxy_auth_is_enforced_when_enabled() {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let h = Harness::start("auth").await;
    h.ctx.set_auth(Some("pentest:s3cret".into()));

    let res = h.get(&h.url("/echo")).await;
    assert!(res.starts_with("HTTP/1.1 407"), "{res}");
    assert!(res.to_ascii_lowercase().contains("proxy-authenticate: basic"), "{res}");

    let connect = h.raw(&format!("CONNECT {0} HTTP/1.1\r\nHost: {0}\r\nConnection: close\r\n\r\n", h.origin)).await;
    assert!(connect.starts_with("HTTP/1.1 407"), "{connect}");

    let wrong = STANDARD.encode("pentest:nope");
    let res = h
        .raw(&format!(
            "GET {} HTTP/1.1\r\nProxy-Authorization: Basic {wrong}\r\nConnection: close\r\n\r\n",
            h.url("/echo")
        ))
        .await;
    assert!(res.starts_with("HTTP/1.1 407"), "{res}");

    let good = STANDARD.encode("pentest:s3cret");
    let res = h
        .raw(&format!(
            "GET {} HTTP/1.1\r\nProxy-Authorization: basic {good}\r\nConnection: close\r\n\r\n",
            h.url("/echo")
        ))
        .await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(!res.to_ascii_lowercase().contains("proxy-authorization"), "credential not forwarded: {res}");

    // The landing page stays reachable so devices can fetch the CA.
    let landing = h.raw("GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
    assert!(landing.contains("Ngintip proxy"), "{landing}");

    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let id = flows[0]["id"].as_u64().unwrap();
    let flow = h.ctx.store.get(id).unwrap().unwrap();
    assert!(
        !flow.req_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("proxy-authorization")),
        "credential not recorded"
    );
}

#[tokio::test]
async fn chains_through_upstream_proxy() {
    let a = Harness::start("chain-a").await;
    let b = Harness::start("chain-b").await;
    b.ctx.set_auth(Some("up:pw".into()));
    let route = UpstreamProxy::parse(&format!("http://up:pw@{}", b.proxy)).unwrap();
    a.ctx.set_upstream(route).unwrap();

    // Plain HTTP: A sends absolute-form to B, with B's credential.
    let res = a.get(&a.url("/echo?plain=1")).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(!res.to_ascii_lowercase().contains("proxy-authorization"), "{res}");
    assert_eq!(b.wait_events(proxy::FLOW_EVENT, 1).await[0]["path"], "/echo?plain=1");

    // HTTPS: A opens a CONNECT tunnel through B; both decrypt and record it.
    let tls_origin = spawn_tls_origin(&a.ctx.ca).await;
    let req = Request::get(format!("https://{tls_origin}/echo?tls=1")).body(body::empty()).unwrap();
    let res = client_via(a.proxy).request(req).await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(body_text(res).await.contains("GET /echo?tls=1"));

    let a_flows = a.wait_events(proxy::FLOW_EVENT, 2).await;
    assert_eq!((a_flows[1]["scheme"].as_str(), a_flows[1]["path"].as_str()), (Some("https"), Some("/echo?tls=1")));
    let b_flows = b.wait_events(proxy::FLOW_EVENT, 2).await;
    assert_eq!((b_flows[1]["scheme"].as_str(), b_flows[1]["path"].as_str()), (Some("https"), Some("/echo?tls=1")));

    // Back to direct: B sees nothing new.
    a.ctx.set_upstream(None).unwrap();
    assert!(a.get(&a.url("/echo")).await.starts_with("HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(b.events(proxy::FLOW_EVENT).len(), 2);
}

#[tokio::test]
async fn upstream_refusing_tunnel_is_reported() {
    let a = Harness::start("chain-refuse-a").await;
    let b = Harness::start("chain-refuse-b").await;
    b.ctx.set_auth(Some("up:pw".into()));
    a.ctx.set_upstream(UpstreamProxy::parse(&b.proxy.to_string()).unwrap()).unwrap();

    let tls_origin = spawn_tls_origin(&a.ctx.ca).await;
    let req = Request::get(format!("https://{tls_origin}/")).body(body::empty()).unwrap();
    let res = client_via(a.proxy).request(req).await.unwrap();
    assert_eq!(res.status(), 502);
    let text = body_text(res).await;
    assert!(text.contains("407"), "{text}");
}

fn flow_by_id(h: &Harness, event: &Value) -> crate::store::Flow {
    h.ctx.store.get(event["id"].as_u64().unwrap()).unwrap().unwrap()
}

#[tokio::test]
async fn large_response_streams_and_history_keeps_the_start() {
    let h = Harness::start("big").await;
    let n = CAPTURE_LIMIT + 4 * 1024 * 1024;
    let req = Request::get(h.url(&format!("/big?n={n}"))).body(body::empty()).unwrap();
    let res = client_via(h.proxy).request(req).await.unwrap();
    let got = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(got.len(), n, "client gets every byte");

    let events = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let last = events.last().unwrap();
    assert_eq!(last["length"].as_u64(), Some(n as u64), "summary has the real size");
    let flow = flow_by_id(&h, last);
    // Bodies are stored in full (below the STORE_LIMIT ceiling), not capped at the buffer size.
    assert_eq!(flow.res_body.len(), n);
    assert!(!flow.res_truncated && !flow.req_truncated);
    assert!(flow.summary.error.is_none());
}

#[tokio::test]
async fn slow_response_reaches_client_before_it_ends() {
    let h = Harness::start("slow").await;
    let req = Request::get(h.url("/slow?ms=1600")).body(body::empty()).unwrap();
    let res = client_via(h.proxy).request(req).await.unwrap();
    let mut body = res.into_body();

    let started = std::time::Instant::now();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(&first[..], b"first ");
    assert!(started.elapsed() < Duration::from_millis(800), "first chunk arrives before the pause ends");

    // After ~1s History gets a preview of the still-open flow...
    let preview = h.wait_events(proxy::FLOW_EVENT, 1).await;
    assert_eq!(preview[0]["streaming"], true);

    // ...and the final version once the body completes.
    let rest = body.collect().await.unwrap().to_bytes();
    assert_eq!(&rest[..], b"second");
    let events = h.wait_events(proxy::FLOW_EVENT, 2).await;
    assert_eq!(events[1]["streaming"], false);
    assert_eq!(events[1]["id"], preview[0]["id"]);
    assert_eq!(&flow_by_id(&h, &events[1]).res_body[..], b"first second");
}

#[tokio::test]
async fn large_upload_streams_to_upstream() {
    let h = Harness::start("upload").await;
    let n = CAPTURE_LIMIT + 1024 * 1024;
    let req = Request::post(h.url("/len")).body(body::full(vec![b'z'; n])).unwrap();
    let res = client_via(h.proxy).request(req).await.unwrap();
    assert_eq!(body_text(res).await, n.to_string());

    let events = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let flow = flow_by_id(&h, events.last().unwrap());
    // Streamed past the buffer threshold but still stored whole.
    assert_eq!(flow.req_body.len(), n);
    assert!(!flow.req_truncated);
}

async fn next_held(h: &Harness, phase: crate::intercept::Phase) -> crate::intercept::PendingItem {
    for _ in 0..500 {
        if let Some(item) = h.ctx.intercept.pending().into_iter().find(|p| p.phase == phase) {
            return item;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing held in phase {phase:?}");
}

#[tokio::test]
async fn intercepts_and_edits_a_response() {
    use crate::intercept::{Decision, Phase};

    let h = Arc::new(Harness::start("intercept-res").await);
    h.ctx.intercept.set_enabled(true);

    let client = tokio::spawn({
        let h = h.clone();
        async move { h.get(&h.url("/echo?q=1")).await }
    });

    let req = next_held(&h, Phase::Request).await;
    assert!(req.raw.starts_with("GET /echo?q=1"), "{}", req.raw);
    h.ctx.intercept.resolve(req.key, Decision::Forward { raw: None, hold_response: true });

    let res = next_held(&h, Phase::Response).await;
    assert_eq!((res.id, res.status), (req.id, Some(200)));
    assert!(res.raw.contains("GET /echo?q=1"), "origin echo visible: {}", res.raw);
    let edited = "HTTP/1.1 418 I'm a teapot\nContent-Type: text/plain\nX-Edited: yes\n\nhacked body";
    h.ctx.intercept.resolve(res.key, Decision::Forward { raw: Some(edited.into()), hold_response: false });

    let out = client.await.unwrap();
    assert!(out.starts_with("HTTP/1.1 418"), "{out}");
    assert!(out.to_ascii_lowercase().contains("x-edited: yes"), "{out}");
    assert!(out.ends_with("hacked body"), "{out}");

    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    assert_eq!(flows[0]["status"], 418, "History shows what the client got");
}

// A Python extension that base64-decodes + XOR-decrypts response bodies (key
// "e2e-key"), matching the /encrypted origin endpoint above.
const DECRYPT_EXT: &str = r#"
import base64
KEY = b"e2e-key"
def _xor(d):
    return bytes(b ^ KEY[i % len(KEY)] for i, b in enumerate(d))
def process_response(res):
    if not res.body:
        return None
    try:
        res.body = _xor(base64.b64decode(res.body))
    except Exception:
        pass
    return None
"#;

#[tokio::test]
async fn extension_decrypts_a_recorded_response_with_original_variant() {
    if crate::extensions::detect_python().is_none() {
        eprintln!("skipping: no python interpreter");
        return;
    }
    let sdir = temp_dir("ext-script");
    let script = sdir.join("decrypt.py");
    std::fs::write(&script, DECRYPT_EXT).unwrap();

    let h = Arc::new(Harness::start("ext-decrypt").await);
    // Load + enable the decrypt extension.
    h.ctx.extensions.configure(sdir.clone(), None, None);
    h.ctx.extensions.set_extensions(vec![crate::extensions::ExtensionSpec {
        id: "dec".into(),
        name: "decrypt".into(),
        path: script.to_string_lossy().into_owned(),
        enabled: true,
        scope: "both".into(),
        requirements: vec![],
        body: String::new(),
    }]);
    h.ctx.extensions.set_enabled(true);
    assert!(h.ctx.extensions.active(), "extension worker should be running");

    // Fetch the encrypted endpoint through the proxy.
    let out = h.get(&h.url("/encrypted")).await;
    // On the wire, the client received the DECRYPTED plaintext (extension transformed it).
    assert!(out.contains("SECRET-42"), "client got the decrypted body:\n{out}");

    // The recorded flow: response is the decrypted plaintext; the Original variant
    // still holds the ciphertext exactly as it came off the server.
    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let id = flows[0]["id"].as_u64().unwrap();
    let flow = h.ctx.store.get(id).unwrap().unwrap();
    assert!(String::from_utf8_lossy(&flow.res_body).contains("SECRET-42"), "recorded response is decrypted");
    let original = flow.variants.res_original.expect("Original response variant captured");
    assert!(original.to_ascii_lowercase().contains("x-encrypted: xor-b64"), "Original keeps the encrypted headers:\n{original}");
    assert!(!original.contains("SECRET-42"), "Original body is still ciphertext, not the plaintext");

    let _ = std::fs::remove_dir_all(&sdir);
}

#[tokio::test]
async fn extensions_are_stored_and_loaded_per_project() {
    let h = Harness::start("ext-per-project").await;

    // A project with no extensions setting loads none (a blank project has none).
    crate::commands::load_extensions_into(&h.ctx);
    assert!(h.ctx.extensions.status().extensions.is_empty(), "blank project has no extensions");

    // Save an extension (with its embedded script copy) + the on/off state into THIS
    // project's settings, the way the per-project persistence does.
    let spec = crate::extensions::ExtensionSpec {
        id: "x1".into(),
        name: "x1".into(),
        path: String::new(),
        enabled: true,
        scope: "all".into(),
        requirements: vec![],
        body: "def process_request(req):\n    pass\n".into(),
    };
    h.ctx.store.put_setting("extensions", &serde_json::to_value(vec![spec]).unwrap()).unwrap();
    h.ctx.store.put_setting("extensions_enabled", &serde_json::json!(true)).unwrap();

    // Loading the project brings the extension AND its on/off state back.
    crate::commands::load_extensions_into(&h.ctx);
    let st = h.ctx.extensions.status();
    assert_eq!(st.extensions.len(), 1, "the project's extension is loaded");
    assert_eq!(st.extensions[0].id, "x1");
    assert!(st.enabled, "the saved on/off state is restored");

    // The extension travels in the project snapshot (settings table is included).
    let snap = h.ctx.store.snapshot().unwrap();
    let raw = String::from_utf8_lossy(&snap);
    assert!(raw.contains("process_request"), "the embedded script is inside the saved project");
}

#[tokio::test]
async fn extension_interpreter_venv_and_console_are_per_project() {
    let sdir = temp_dir("ext-per-project-paths");
    let h = Harness::start("ext-per-project-paths").await;

    // Save a per-project interpreter, custom venv and a console line into THIS project's
    // settings (the way set_python_path / set_extension_venv_path / persist_extension_logs
    // do), then open the project and confirm they come back.
    let fake_python = sdir.join("python3");
    let fake_venv = sdir.join("myvenv");
    h.ctx.store.put_setting("python_path", &serde_json::json!(fake_python.to_string_lossy())).unwrap();
    h.ctx.store.put_setting("venv_path", &serde_json::json!(fake_venv.to_string_lossy())).unwrap();
    h.ctx.store.put_setting("extension_logs", &serde_json::json!(["hello from last session"])).unwrap();

    crate::commands::apply_project_python_venv(&h.ctx, &sdir, None);
    crate::commands::load_extensions_into(&h.ctx);

    let st = h.ctx.extensions.status();
    assert_eq!(st.python.as_deref(), Some(fake_python.to_string_lossy().as_ref()), "the project's interpreter is applied");
    assert_eq!(st.venv.as_deref(), Some(fake_venv.to_string_lossy().as_ref()), "the project's custom venv is applied");
    assert!(
        h.ctx.extensions.logs().iter().any(|l| l == "hello from last session"),
        "the saved debug console is restored"
    );

    // All of it travels inside the project snapshot (settings table is included).
    let snap = h.ctx.store.snapshot().unwrap();
    let raw = String::from_utf8_lossy(&snap);
    assert!(raw.contains("hello from last session"), "the console is inside the saved project");

    let _ = std::fs::remove_dir_all(&sdir);
}

// The shipped AES decrypt extension, embedded so the test and the example stay in sync.
const AES_DECRYPT_EXT: &str = include_str!("../../examples/aes_decrypt_extension.py");

#[tokio::test]
async fn extension_decrypts_an_aes_request_end_to_end() {
    let Some(python) = crate::extensions::detect_python() else {
        eprintln!("skipping: no python interpreter");
        return;
    };
    // Needs pycryptodome (the example's dependency); skip when it is not installed.
    let has_crypto = std::process::Command::new(&python)
        .args(["-c", "import Crypto"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_crypto {
        eprintln!("skipping: pycryptodome not installed in {python}");
        return;
    }

    // Encrypt a known plaintext with AES-128-CBC (key ngintip-demo-key) via python,
    // exactly like the /test-enc page does in the browser.
    let plaintext = "{\"user\":\"admin\",\"action\":\"transfer\"}";
    let enc = std::process::Command::new(&python)
        .arg("-c")
        .arg("import base64,os,sys\nfrom Crypto.Cipher import AES\nfrom Crypto.Util.Padding import pad\nkey=b'ngintip-demo-key'\niv=os.urandom(16)\nct=AES.new(key,AES.MODE_CBC,iv).encrypt(pad(sys.argv[1].encode(),16))\nprint(base64.b64encode(iv).decode())\nprint(base64.b64encode(ct).decode())")
        .arg(plaintext)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&enc.stdout);
    let mut lines = stdout.lines();
    let iv_b64 = lines.next().unwrap_or("").to_string();
    let ct_b64 = lines.next().unwrap_or("").to_string();
    assert!(!iv_b64.is_empty() && !ct_b64.is_empty(), "python encrypt failed: {}", String::from_utf8_lossy(&enc.stderr));

    let sdir = temp_dir("aes-ext");
    let script = sdir.join("aes.py");
    std::fs::write(&script, AES_DECRYPT_EXT).unwrap();

    let h = Arc::new(Harness::start("aes-decrypt").await);
    h.ctx.extensions.configure(sdir.clone(), None, None);
    h.ctx.extensions.set_extensions(vec![crate::extensions::ExtensionSpec {
        id: "aes".into(),
        name: "aes".into(),
        path: script.to_string_lossy().into_owned(),
        enabled: true,
        scope: "both".into(),
        requirements: vec![],
        body: String::new(),
    }]);
    h.ctx.extensions.set_enabled(true);
    assert!(h.ctx.extensions.active(), "extension worker running");

    // POST the ciphertext (with the IV header) through the proxy to the echo origin.
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nX-Enc-IV: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        h.url("/echo"),
        h.origin,
        iv_b64,
        ct_b64.len(),
        ct_b64
    );
    let out = h.raw(&req).await;
    // The origin echoes what reached it: the extension decrypted the body to plaintext.
    assert!(out.contains("\"user\":\"admin\""), "origin saw the decrypted plaintext:\n{out}");

    // History: recorded request is the decrypted plaintext; Original keeps the ciphertext.
    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let id = flows[0]["id"].as_u64().unwrap();
    let flow = h.ctx.store.get(id).unwrap().unwrap();
    assert!(String::from_utf8_lossy(&flow.req_body).contains("\"user\":\"admin\""), "recorded request is decrypted");
    let original = flow.variants.req_original.expect("Original request variant captured");
    assert!(original.contains(&ct_b64), "Original still holds the AES ciphertext");

    let _ = std::fs::remove_dir_all(&sdir);
}

#[tokio::test]
async fn extension_scope_controls_where_it_runs() {
    if crate::extensions::detect_python().is_none() {
        eprintln!("skipping: no python interpreter");
        return;
    }
    let sdir = temp_dir("scope-ext");
    let script = sdir.join("tag.py");
    std::fs::write(&script, "def process_request(req):\n    req.set_header('X-Ext', 'applied')\n").unwrap();

    let h = Arc::new(Harness::start("scope-ext").await);
    h.ctx.extensions.configure(sdir.clone(), None, None);
    let spec = |scope: &str| crate::extensions::ExtensionSpec {
        id: "tag".into(),
        name: "tag".into(),
        path: script.to_string_lossy().into_owned(),
        enabled: true,
        scope: scope.into(),
        requirements: vec![],
        body: String::new(),
    };
    let target = format!("http://{}", h.origin);
    let raw = format!("GET /echo HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", h.origin);

    // Scope "proxy": a Repeater send is NOT transformed.
    h.ctx.extensions.set_extensions(vec![spec("proxy")]);
    h.ctx.extensions.set_enabled(true);
    let res = crate::commands::send_recorded(&h.ctx, None, &target, &raw, true).await.unwrap();
    assert!(!res.response_raw.to_lowercase().contains("x-ext: applied"), "proxy-scope must not run in Repeater:\n{}", res.response_raw);

    // Scope "repeater": the same send IS transformed (origin echoes the added header).
    h.ctx.extensions.set_extensions(vec![spec("repeater")]);
    let res = crate::commands::send_recorded(&h.ctx, None, &target, &raw, true).await.unwrap();
    assert!(res.response_raw.to_lowercase().contains("x-ext: applied"), "repeater-scope should run in Repeater:\n{}", res.response_raw);

    // History keeps the typed request under the Original variant. Both sends record a
    // flow, so wait for the second (the repeater-scope one) before inspecting it.
    let flows = h.wait_events(proxy::FLOW_EVENT, 2).await;
    let last = flows.last().unwrap();
    let id = last["id"].as_u64().unwrap();
    let flow = h.ctx.store.get(id).unwrap().unwrap();
    assert!(flow.req_headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("x-ext") && v == "applied"), "sent request has the header");
    let original = flow.variants.req_original.expect("Original request variant");
    assert!(!original.to_lowercase().contains("x-ext"), "Original is the typed request, without the extension header");
    let _ = std::fs::remove_dir_all(&sdir);
}

#[tokio::test]
async fn interceptor_scope_extension_transforms_held_message() {
    use crate::intercept::{Decision, Phase};
    if crate::extensions::detect_python().is_none() {
        eprintln!("skipping: no python interpreter");
        return;
    }
    let sdir = temp_dir("intercept-ext");
    let script = sdir.join("tag.py");
    std::fs::write(&script, "def process_request(req):\n    req.set_header('X-Ext', 'applied')\n").unwrap();

    let h = Arc::new(Harness::start("intercept-ext").await);
    h.ctx.extensions.configure(sdir.clone(), None, None);
    h.ctx.extensions.set_extensions(vec![crate::extensions::ExtensionSpec {
        id: "tag".into(),
        name: "tag".into(),
        path: script.to_string_lossy().into_owned(),
        enabled: true,
        scope: "interceptor".into(),
        requirements: vec![],
        body: String::new(),
    }]);
    h.ctx.extensions.set_enabled(true);
    assert!(h.ctx.extensions.active(), "extension worker running");
    h.ctx.intercept.set_enabled(true);

    let client = tokio::spawn({
        let h = h.clone();
        async move {
            h.raw(&format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                h.url("/echo"),
                h.origin
            ))
            .await
        }
    });

    // The held request the editor sees is already transformed; Original keeps what arrived.
    let req = next_held(&h, Phase::Request).await;
    assert!(req.raw.to_lowercase().contains("x-ext: applied"), "held request is transformed:\n{}", req.raw);
    // Editor format is LF, not the runner's CRLF (otherwise the Edited tab renders garbled).
    assert!(!req.raw.contains('\r'), "held request must use LF line endings, not CRLF:\n{:?}", req.raw);
    let original = req.original.clone().expect("held Original variant kept");
    assert!(!original.to_lowercase().contains("x-ext"), "held Original is the arrived request:\n{original}");

    // Forwarding unchanged still sends the extension's transform upstream.
    h.ctx.intercept.resolve(req.key, Decision::UNCHANGED);
    let out = client.await.unwrap();
    assert!(out.to_lowercase().contains("x-ext: applied"), "origin saw the transformed request:\n{out}");

    // History keeps the arrived request as its Original variant.
    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let id = flows[0]["id"].as_u64().unwrap();
    let flow = h.ctx.store.get(id).unwrap().unwrap();
    let recorded_original = flow.variants.req_original.expect("History Original variant");
    assert!(!recorded_original.to_lowercase().contains("x-ext"), "History Original is the arrived request");
    let _ = std::fs::remove_dir_all(&sdir);
}

#[tokio::test]
async fn repeater_scope_extension_transforms_ui_send() {
    if crate::extensions::detect_python().is_none() {
        eprintln!("skipping: no python interpreter");
        return;
    }
    let sdir = temp_dir("rep-ext");
    let script = sdir.join("tag.py");
    std::fs::write(
        &script,
        "def process_request(req):\n    req.set_header('X-Ext', 'applied')\ndef process_response(res):\n    res.set_header('X-Ext-Res', 'seen')\n",
    )
    .unwrap();

    let h = Arc::new(Harness::start("rep-ext").await);
    h.ctx.extensions.configure(sdir.clone(), None, None);
    h.ctx.extensions.set_extensions(vec![crate::extensions::ExtensionSpec {
        id: "tag".into(),
        name: "tag".into(),
        path: script.to_string_lossy().into_owned(),
        enabled: true,
        scope: "repeater".into(),
        requirements: vec![],
        body: String::new(),
    }]);
    h.ctx.extensions.set_enabled(true);
    assert!(h.ctx.extensions.active(), "extension worker running");

    let target = format!("http://{}", h.origin);
    let raw = format!("GET /echo HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", h.origin);
    // The interactive Repeater tab path (send_raw with a tab -> send_and_record); bulk
    // Intruder sends (repeater_send, no tab) deliberately skip extensions.
    let res = crate::commands::send_and_record(&h.ctx, 1, &target, &raw, true).await.unwrap();

    // Final response carries the response-transform header; the origin echoed the request one.
    assert!(res.response_raw.to_lowercase().contains("x-ext-res: seen"), "response transformed:\n{}", res.response_raw);
    assert!(res.response_raw.to_lowercase().contains("x-ext: applied"), "sent request reached origin:\n{}", res.response_raw);
    // The request actually sent is surfaced (the typed one stays in the editor).
    let sent = res.request_final.expect("request_final surfaced");
    assert!(sent.to_lowercase().contains("x-ext"), "sent request carries the header:\n{sent}");
    // Original response is what arrived, before the response transform.
    let original = res.response_original.expect("response_original surfaced");
    assert!(!original.to_lowercase().contains("x-ext-res"), "Original response lacks the response-transform header:\n{original}");
    let _ = std::fs::remove_dir_all(&sdir);
}

#[tokio::test]
async fn intercept_edit_keeps_the_original_request_variant() {
    use crate::intercept::{Decision, Phase};

    let h = Arc::new(Harness::start("intercept-req-var").await);
    h.ctx.intercept.set_enabled(true);

    let client = tokio::spawn({
        let h = h.clone();
        async move {
            h.raw(&format!(
                "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                h.url("/echo"),
                h.origin
            ))
            .await
        }
    });

    let req = next_held(&h, Phase::Request).await;
    assert!(req.raw.contains("hello"), "held request has the body: {}", req.raw);
    // Same-length edit so Content-Length stays valid.
    let edited = req.raw.replace("hello", "world");
    h.ctx.intercept.resolve(req.key, Decision::Forward { raw: Some(edited), hold_response: false });

    let out = client.await.unwrap();
    assert!(out.contains("world"), "origin echoed the edited body:\n{out}");

    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let id = flows[0]["id"].as_u64().unwrap();
    let flow = h.ctx.store.get(id).unwrap().unwrap();
    // Recorded (Intercepted/Final) request is the edit; the Original variant is the
    // pre-edit request the client actually sent.
    assert!(String::from_utf8_lossy(&flow.req_body).contains("world"), "recorded request is the edit");
    let original = flow.variants.req_original.expect("Original request variant captured");
    assert!(original.contains("hello") && !original.contains("world"), "Original is the pre-edit body:\n{original}");
}

#[tokio::test]
async fn global_response_intercept_can_drop() {
    use crate::intercept::{Decision, Phase};

    let h = Arc::new(Harness::start("intercept-drop").await);
    h.ctx.intercept.set_enabled(true);
    h.ctx.intercept.set_responses(true);

    let client = tokio::spawn({
        let h = h.clone();
        async move { h.get(&h.url("/echo")).await }
    });
    let req = next_held(&h, Phase::Request).await;
    h.ctx.intercept.resolve(req.key, Decision::UNCHANGED);
    let res = next_held(&h, Phase::Response).await;
    h.ctx.intercept.resolve(res.key, Decision::Drop);

    let out = client.await.unwrap();
    assert!(out.starts_with("HTTP/1.1 502"), "{out}");
    assert!(out.contains("Response dropped"), "{out}");
    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    assert_eq!(flows[0]["error"], "response dropped (intercept)");
}

/// WebSocket echo server; remembers whether a client offered extensions.
async fn spawn_ws_origin(tls: Option<&CertAuthority>) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{handshake::server, Message};

    let acceptor = tls.map(|ca| tokio_rustls::TlsAcceptor::from(tls_config(ca, &[b"http/1.1"])));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen_by_server = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { continue };
            let (acceptor, seen) = (acceptor.clone(), seen_by_server.clone());
            tokio::spawn(async move {
                let check = move |req: &server::Request, res: server::Response| {
                    let ext = req.headers().get("sec-websocket-extensions").map(|v| v.to_str().unwrap().to_string());
                    seen.lock().unwrap().push(format!("{} ext={ext:?}", req.uri().path()));
                    Ok(res)
                };
                macro_rules! echo {
                    ($stream:expr) => {{
                        let mut ws = tokio_tungstenite::accept_hdr_async($stream, check).await.unwrap();
                        while let Some(Ok(msg)) = ws.next().await {
                            match msg {
                                Message::Text(t) if t.as_str() == "bye" => {
                                    let _ = ws.close(None).await;
                                }
                                Message::Text(_) | Message::Binary(_) => ws.send(msg).await.unwrap(),
                                _ => {}
                            }
                        }
                    }};
                }
                match acceptor {
                    Some(acceptor) => echo!(acceptor.accept(tcp).await.unwrap()),
                    None => echo!(tcp),
                }
            });
        }
    });
    (addr, seen)
}

/// Open a CONNECT tunnel through the proxy, as a browser does for ws:// and wss://.
async fn connect_tunnel(proxy: SocketAddr, target: SocketAddr) -> TcpStream {
    let mut tcp = TcpStream::connect(proxy).await.unwrap();
    tcp.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(tcp.read_u8().await.unwrap());
    }
    assert!(head.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&head));
    tcp
}

async fn ws_roundtrip<S>(stream: S, url: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("sec-websocket-extensions", "permessage-deflate".parse().unwrap());
    let (mut ws, res) = tokio_tungstenite::client_async(req, stream).await.unwrap();
    assert_eq!(res.status(), 101);

    ws.send(Message::text(r#"{"hello":"intip"}"#)).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text(r#"{"hello":"intip"}"#));
    ws.send(Message::binary(vec![0u8, 1, 2, 3])).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::binary(vec![0u8, 1, 2, 3]));
    ws.send(Message::text("bye")).await.unwrap();
    while let Some(Ok(_)) = ws.next().await {}
}

/// (direction, opcode, text) of every recorded message, waiting until `n` exist.
async fn ws_log(h: &Harness, flow_id: u64, n: usize) -> Vec<(String, u8, String)> {
    for _ in 0..300 {
        let msgs = h.ctx.store.ws_messages(flow_id).unwrap();
        if msgs.len() >= n {
            return msgs.into_iter().map(|m| (m.direction, m.opcode, m.text)).collect();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} ws messages, got {:?}", h.ctx.store.ws_messages(flow_id).unwrap().len());
}

#[tokio::test]
async fn websocket_through_connect_tunnel_is_bridged_and_logged() {
    let h = Harness::start("ws").await;
    let (origin, seen) = spawn_ws_origin(None).await;

    let tunnel = connect_tunnel(h.proxy, origin).await;
    ws_roundtrip(tunnel, &format!("ws://{origin}/chat")).await;

    assert_eq!(seen.lock().unwrap().as_slice(), ["/chat ext=None"], "extensions stripped");
    let flow = &h.wait_events(proxy::FLOW_EVENT, 1).await[0];
    assert_eq!((flow["status"].as_u64(), flow["scheme"].as_str()), (Some(101), Some("http")));

    let log = ws_log(&h, flow["id"].as_u64().unwrap(), 6).await;
    let json = r#"{"hello":"intip"}"#.to_string();
    assert_eq!(log[0], ("client".into(), 1, json.clone()));
    assert_eq!(log[1], ("server".into(), 1, json));
    assert_eq!((log[2].0.as_str(), log[2].1), ("client", 2));
    assert!(log[2].2.starts_with("00000000  00 01 02 03"), "{}", log[2].2);
    assert_eq!(log[4], ("client".into(), 1, "bye".into()));
    assert!(log.iter().any(|(d, op, _)| d == "server" && *op == 8), "close recorded: {log:?}");
    assert!(!h.events(crate::ws::WS_EVENT).is_empty(), "UI is told about messages");
}

#[tokio::test]
async fn secure_websocket_is_intercepted_too() {
    let h = Harness::start("wss").await;
    let (origin, _) = spawn_ws_origin(Some(&h.ctx.ca)).await;

    let tunnel = connect_tunnel(h.proxy, origin).await;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(upstream::insecure_tls_config().unwrap()))
        .connect("127.0.0.1".try_into().unwrap(), tunnel)
        .await
        .unwrap();
    ws_roundtrip(tls, &format!("wss://{origin}/secure")).await;

    let flow = &h.wait_events(proxy::FLOW_EVENT, 1).await[0];
    assert_eq!((flow["status"].as_u64(), flow["scheme"].as_str(), flow["path"].as_str()), (Some(101), Some("https"), Some("/secure")));
    let log = ws_log(&h, flow["id"].as_u64().unwrap(), 5).await;
    assert_eq!(log[0].2, r#"{"hello":"intip"}"#);
}

#[tokio::test]
async fn match_and_replace_rewrites_traffic() {
    use crate::rules::{Rule, Target};

    let h = Harness::start("rules").await;
    let rule = |target, pattern: &str, replace: &str| Rule {
        target,
        pattern: pattern.into(),
        replace: replace.into(),
        ..Rule::default()
    };
    h.ctx
        .rules
        .set(&[
            rule(Target::RequestHeader, "", "X-Injected: yes"),
            rule(Target::RequestBody, "role=user", "role=admin"),
            rule(Target::ResponseHeader, "^Content-Type: .*$", "Content-Type: text/x-rewritten"),
            rule(Target::ResponseBody, "POST /echo", "POST /rewritten"),
        ])
        .unwrap();

    let res = h
        .raw(&format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 9\r\nConnection: close\r\n\r\nrole=user",
            h.url("/echo"),
            h.origin
        ))
        .await;
    let lower = res.to_ascii_lowercase();
    assert!(lower.contains("content-type: text/x-rewritten"), "{res}");
    assert!(res.contains("POST /rewritten"), "response body rule: {res}");
    assert!(lower.contains("x-injected: yes"), "origin saw the added header: {res}");
    assert!(res.contains("role=admin"), "origin saw the rewritten body: {res}");

    let flows = h.wait_events(proxy::FLOW_EVENT, 1).await;
    let flow = flow_by_id(&h, &flows[0]);
    assert_eq!(&flow.req_body[..], b"role=admin", "History shows what was sent");
    assert!(String::from_utf8_lossy(&flow.res_body).contains("POST /rewritten"));
}

#[tokio::test]
async fn http2_is_negotiated_with_client_and_mirrored_upstream() {
    let h = Harness::start("h2").await;
    let origin = spawn_tls_origin_with(&h.ctx.ca, true).await;

    let req = Request::get(format!("https://{origin}/echo?h2=1")).body(body::empty()).unwrap();
    let res = client_via_h2(h.proxy).request(req).await.unwrap();
    assert_eq!(res.version(), http::Version::HTTP_2, "client <-> proxy is HTTP/2");
    let text = body_text(res).await;
    assert!(text.starts_with("GET /echo?h2=1 HTTP/2.0"), "proxy <-> origin is HTTP/2: {text}");

    let flow = flow_by_id(&h, &h.wait_events(proxy::FLOW_EVENT, 1).await[0]);
    assert_eq!((flow.version.as_str(), flow.res_version.as_str()), ("HTTP/2", "HTTP/2"));
    assert!(
        flow.req_headers.iter().any(|(k, v)| k == "Host" && v == &origin.to_string()),
        ":authority shown as Host: {:?}",
        flow.req_headers
    );
}

#[tokio::test]
async fn http2_post_body_is_buffered_and_forwarded() {
    // h2 request bodies are buffered so a stream reset can be retried over h1;
    // this proves the buffering path forwards the body intact for a normal POST.
    let h = Harness::start("h2post").await;
    let origin = spawn_tls_origin_with(&h.ctx.ca, true).await;
    let body = "field=value&n=42&x=hello";
    let req = Request::post(format!("https://{origin}/echo"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body::full(body))
        .unwrap();
    let res = client_via_h2(h.proxy).request(req).await.unwrap();
    assert_eq!(res.version(), http::Version::HTTP_2);
    let text = body_text(res).await;
    assert!(text.starts_with("POST /echo HTTP/2.0"), "{text}");
    assert!(text.contains(body), "the h2 POST body must reach the origin intact: {text}");
    // History recorded the buffered request body.
    let flow = flow_by_id(&h, &h.wait_events(proxy::FLOW_EVENT, 1).await[0]);
    assert_eq!(&flow.req_body[..], body.as_bytes(), "buffered h2 body recorded in History");
}

#[tokio::test]
async fn http1_client_stays_http1_upstream() {
    let h = Harness::start("h1").await;
    let origin = spawn_tls_origin_with(&h.ctx.ca, true).await;
    let req = Request::get(format!("https://{origin}/echo")).body(body::empty()).unwrap();
    let res = client_via(h.proxy).request(req).await.unwrap();
    assert_eq!(res.version(), http::Version::HTTP_11);
    assert!(body_text(res).await.starts_with("GET /echo HTTP/1.1"));
}

#[tokio::test]
async fn repeater_picks_protocol_from_request_line() {
    let h = Harness::start("repeater").await;
    let origin = spawn_tls_origin_with(&h.ctx.ca, true).await;
    let target = format!("https://{origin}");

    let res = crate::commands::repeater_send(&h.ctx, &target, &format!("GET /echo HTTP/2\nHost: {origin}\nX-A: 1\n\n"), true)
        .await
        .unwrap();
    assert!(res.response_raw.starts_with("HTTP/2 200"), "{}", res.response_raw);
    assert!(res.response_raw.contains("GET /echo HTTP/2.0"), "{}", res.response_raw);
    assert!(!res.response_raw.contains("\nhost:"), "Host folded into :authority: {}", res.response_raw);

    let res = crate::commands::repeater_send(
        &h.ctx,
        &target,
        &format!("POST /echo HTTP/1.1\nHost: {origin}\nContent-Type: multipart/form-data; boundary=x\nContent-Length: 1\n\n--x\nA\n--x--"),
        true,
    )
    .await
    .unwrap();
    assert!(res.response_raw.starts_with("HTTP/1.1 200"), "{}", res.response_raw);
    assert!(res.response_raw.contains("POST /echo HTTP/1.1"), "{}", res.response_raw);
    assert!(res.response_raw.contains("content-length: 13"), "11-byte body, 2 LF -> CRLF: {}", res.response_raw);
}

#[tokio::test]
async fn repeater_history_records_sends_per_tab() {
    use crate::commands::send_and_record;
    use crate::store::REPEATER_HISTORY_LIMIT;

    let h = Harness::start("repeater-history").await;
    let target = format!("http://{}", h.origin);
    let store = &h.ctx.store;

    send_and_record(&h.ctx, 1, &target, "GET /echo?n=1 HTTP/1.1\n\n", true).await.unwrap();
    send_and_record(&h.ctx, 1, &target, "GET /echo?n=2 HTTP/1.1\n\n", true).await.unwrap();
    // Nothing listens on port 9 (discard): recorded as a failed send.
    assert!(send_and_record(&h.ctx, 1, "http://127.0.0.1:9", "GET / HTTP/1.1\n\n", true).await.is_err());
    // Not a request at all: rejected before sending, not recorded.
    assert!(send_and_record(&h.ctx, 1, &target, "\n\n", true).await.is_err());
    send_and_record(&h.ctx, 2, &target, "GET /echo?other HTTP/1.1\n\n", true).await.unwrap();

    let tab1 = store.repeater_history(1).unwrap();
    assert_eq!(tab1.len(), 3);
    assert_eq!(tab1[0].raw, "GET /echo?n=1 HTTP/1.1\n\n");
    assert_eq!((tab1[1].status, tab1[1].target.as_str()), (200, target.as_str()));
    assert!(tab1[1].response_raw.is_empty(), "lists leave responses out");
    assert_eq!(tab1[2].status, 0);
    assert!(tab1[2].error.is_some());

    let full = store.repeater_entry(tab1[1].id).unwrap().unwrap();
    assert!(full.response_raw.contains("GET /echo?n=2"), "{}", full.response_raw);

    // Only the newest entries of a tab are kept.
    for _ in 0..REPEATER_HISTORY_LIMIT {
        store.push_repeater(&full).unwrap();
    }
    let tab1 = store.repeater_history(1).unwrap();
    assert_eq!(tab1.len() as i64, REPEATER_HISTORY_LIMIT);
    assert!(tab1.iter().all(|e| e.raw.contains("n=2")), "oldest dropped first");
    assert_eq!(store.repeater_history(2).unwrap().len(), 1, "other tabs untouched");

    store.clear_repeater_history(1).unwrap();
    assert!(store.repeater_history(1).unwrap().is_empty());
    assert_eq!(store.repeater_history(2).unwrap().len(), 1);
}

/// Minimal SOCKS5 server for tests: optional user/pass, CONNECT by domain/IP,
/// then bidirectional copy to the requested target.
async fn spawn_socks5(auth: Option<(&'static str, &'static str)>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = listener.accept().await else { continue };
            tokio::spawn(async move {
                let mut h = [0u8; 2];
                if c.read_exact(&mut h).await.is_err() {
                    return;
                }
                let mut methods = vec![0u8; h[1] as usize];
                c.read_exact(&mut methods).await.unwrap();
                if let Some((user, pass)) = auth {
                    c.write_all(&[0x05, 0x02]).await.unwrap();
                    let mut a = [0u8; 2];
                    c.read_exact(&mut a).await.unwrap();
                    let mut u = vec![0u8; a[1] as usize];
                    c.read_exact(&mut u).await.unwrap();
                    let mut pl = [0u8; 1];
                    c.read_exact(&mut pl).await.unwrap();
                    let mut pw = vec![0u8; pl[0] as usize];
                    c.read_exact(&mut pw).await.unwrap();
                    let ok = u == user.as_bytes() && pw == pass.as_bytes();
                    c.write_all(&[0x01, if ok { 0x00 } else { 0x01 }]).await.unwrap();
                    if !ok {
                        return;
                    }
                } else {
                    c.write_all(&[0x05, 0x00]).await.unwrap();
                }
                let mut req = [0u8; 4];
                c.read_exact(&mut req).await.unwrap();
                let host = match req[3] {
                    0x01 => {
                        let mut a = [0u8; 4];
                        c.read_exact(&mut a).await.unwrap();
                        std::net::Ipv4Addr::from(a).to_string()
                    }
                    0x03 => {
                        let mut n = [0u8; 1];
                        c.read_exact(&mut n).await.unwrap();
                        let mut d = vec![0u8; n[0] as usize];
                        c.read_exact(&mut d).await.unwrap();
                        String::from_utf8_lossy(&d).into_owned()
                    }
                    _ => return,
                };
                let mut portb = [0u8; 2];
                c.read_exact(&mut portb).await.unwrap();
                let port = u16::from_be_bytes(portb);
                let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
                    let _ = c.write_all(&[0x05, 0x01, 0, 1, 0, 0, 0, 0, 0, 0]).await;
                    return;
                };
                c.write_all(&[0x05, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut c, &mut upstream).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn chains_through_socks5_proxy() {
    let h = Harness::start("socks").await;
    let socks = spawn_socks5(Some(("u", "pw"))).await;
    let route = UpstreamProxy::parse(&format!("socks5://u:pw@{socks}")).unwrap();
    h.ctx.set_upstream(route).unwrap();

    // Plain HTTP over SOCKS.
    let res = h.get(&h.url("/echo?via=socks")).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains("GET /echo?via=socks"), "{res}");

    // HTTPS over SOCKS: TLS is layered on top of the tunnel.
    let tls_origin = spawn_tls_origin(&h.ctx.ca).await;
    let req = Request::get(format!("https://{tls_origin}/echo?tls=1")).body(body::empty()).unwrap();
    let res = client_via(h.proxy).request(req).await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(body_text(res).await.contains("GET /echo?tls=1"));

    let flows = h.wait_events(proxy::FLOW_EVENT, 2).await;
    assert_eq!(flows[0]["path"], "/echo?via=socks");

    // Wrong SOCKS credentials fail cleanly.
    h.ctx.set_upstream(UpstreamProxy::parse(&format!("socks5://u:bad@{socks}")).unwrap()).unwrap();
    assert!(h.get(&h.url("/echo")).await.starts_with("HTTP/1.1 502"));
}

/// Opt-in load test: fire many requests through the proxy and report throughput.
/// Run with: `cargo test --release stress_many_requests -- --ignored --nocapture`
/// Tune with CEGAT_STRESS_N and CEGAT_STRESS_CONC.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn stress_many_requests() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let n = env("CEGAT_STRESS_N", 5000);
    let conc = env("CEGAT_STRESS_CONC", 128);

    let h = Harness::start("stress").await;
    // Pooled keep-alive client through the proxy, so the measurement is proxy
    // throughput rather than TCP connect/teardown churn.
    let client = Arc::new(client_via(h.proxy));
    let sem = Arc::new(tokio::sync::Semaphore::new(conc));
    let ok = Arc::new(AtomicUsize::new(0));
    let started = std::time::Instant::now();

    let mut tasks = Vec::with_capacity(n);
    for i in 0..n {
        let (client, origin, sem, ok) = (client.clone(), h.origin, sem.clone(), ok.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let req = Request::get(format!("http://{origin}/echo?i={i}")).body(body::empty()).unwrap();
            match client.request(req).await {
                Ok(res) if res.status() == 200 => {
                    // Drain the body so the pooled connection can be reused.
                    let _ = res.into_body().collect().await;
                    ok.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let elapsed = started.elapsed();
    let ok = ok.load(Ordering::Relaxed);
    let rps = n as f64 / elapsed.as_secs_f64();
    println!("[stress] {n} requests, concurrency {conc}: {ok} ok in {elapsed:.2?} = {rps:.0} req/s");

    // All requests succeed and every flow is persisted.
    assert_eq!(ok, n, "all requests succeeded");
    for _ in 0..200 {
        if h.events(proxy::FLOW_EVENT).len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.ctx.store.summaries().unwrap().len(), n, "every flow persisted");

    // Save a real, openable project so the run can be browsed in the UI:
    //   CEGAT_STRESS_FILE=/path/to/stress.cegat cargo test --release stress_many_requests -- --ignored --nocapture
    if let Ok(file) = std::env::var("CEGAT_STRESS_FILE") {
        h.ctx.store.save_as(Path::new(&file)).unwrap();
        println!("[stress] saved {n} flows to {file} (open it from the project chooser)");
    }
}

#[tokio::test]
async fn upstream_failure_and_manual_entries_are_logged() {
    let h = Harness::start("logs").await;
    // Nothing listens on port 9 (discard): the forward fails and is logged.
    let res = h.get("http://127.0.0.1:9/x").await;
    assert!(res.starts_with("HTTP/1.1 502"), "{res}");

    let logs = h.ctx.logs();
    assert!(logs.iter().any(|l| l.level == "warn" && l.source == "upstream"), "upstream failure logged: {:?}", logs.iter().map(|l| (&l.source, &l.message)).collect::<Vec<_>>());

    // The log is a ring buffer surfaced to the UI via the LOG_EVENT.
    h.ctx.log_error("tls", "TLS handshake with example.com failed (client may pin)");
    assert!(h.ctx.logs().iter().any(|l| l.source == "tls"));
    let events = h.events(proxy::LOG_EVENT);
    assert!(events.iter().any(|e| e["source"] == "tls" && e["level"] == "error"));

    h.ctx.clear_logs();
    assert!(h.ctx.logs().is_empty());
}

#[tokio::test]
async fn invisible_proxy_routes_plaintext_by_host() {
    let h = Harness::start("invisible").await;
    let direct = |q: &str| {
        format!("GET /echo?{q} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", h.origin)
    };

    // Off: a direct (non-proxy-aware) request just gets the landing page.
    let res = h.raw(&direct("a=1")).await;
    assert!(res.contains("Ngintip proxy"), "landing when invisible is off: {res}");

    // On: the same request is forwarded to the Host it names.
    h.ctx.set_invisible(true);
    let res = h.raw(&direct("a=2")).await;
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains("GET /echo?a=2"), "reached the origin: {res}");
    assert_eq!(h.wait_events(proxy::FLOW_EVENT, 1).await[0]["path"], "/echo?a=2");
}

#[tokio::test]
async fn repeater_send_recomputes_content_length_against_test_server() {
    // The echo test server, running for real.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(crate::testserver::serve(listener));

    let dir = temp_dir("cl");
    let (ctx, _log) = test_ctx_with_log(&dir);
    let ctx = std::sync::Arc::new(ctx);
    let target = format!("http://{addr}");

    // A body was added/edited but no Content-Length was set: send must add one.
    let raw = format!("POST /echo HTTP/1.1\r\nHost: {addr}\r\n\r\nrole=admin");
    let res = crate::commands::repeater_send(&ctx, &target, &raw, true).await.unwrap();
    assert!(res.response_raw.contains("Actual body bytes: 10"), "{}", res.response_raw);
    assert!(res.response_raw.contains("Content-Length header: 10"), "auto-added CL: {}", res.response_raw);
    assert!(res.response_raw.contains("Content-Length matches body: yes"), "{}", res.response_raw);
    assert!(res.response_raw.contains("role=admin"), "{}", res.response_raw);

    // A stale Content-Length is corrected to match the (edited) body.
    let stale = format!("POST /echo HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 3\r\n\r\nrole=admin");
    let res = crate::commands::repeater_send(&ctx, &target, &stale, true).await.unwrap();
    assert!(res.response_raw.contains("Content-Length header: 10"), "recomputed CL: {}", res.response_raw);
    assert!(res.response_raw.contains("Content-Length matches body: yes"), "{}", res.response_raw);

    // Our request builder keeps a deliberately wrong Content-Length when auto is
    // off, and recomputes it when auto is on. (What hyper puts on the wire is a
    // separate concern; here we verify the header we hand it.)
    let wrong = format!("POST /echo HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 4\r\n\r\nrole=admin");
    let (kept, _) = crate::commands::repeater_request(&target, &wrong, false, None).unwrap();
    assert_eq!(kept.headers().get("content-length").unwrap(), "4", "auto off keeps it verbatim");
    let (fixed, _) = crate::commands::repeater_request(&target, &wrong, true, None).unwrap();
    assert_eq!(fixed.headers().get("content-length").unwrap(), "10", "auto on recomputes");
}

#[test]
fn h2_repeater_request_drops_connection_headers() {
    use crate::proxy::Proto;
    let raw = "GET /x HTTP/2\nHost: example.com\nTe: trailers\nConnection: keep-alive\nUpgrade: h2c\nAccept: */*\n\n";

    // Over HTTP/2, connection-specific headers must be dropped (they'd cause a
    // "stream error: protocol error" reset upstream).
    let (req, proto) = crate::commands::repeater_request("https://example.com", raw, true, None).unwrap();
    assert_eq!(proto, Proto::PreferHttp2);
    assert!(req.headers().get("te").is_none(), "te dropped for h2");
    assert!(req.headers().get("connection").is_none(), "connection dropped for h2");
    assert!(req.headers().get("upgrade").is_none(), "upgrade dropped for h2");
    assert!(req.headers().get("accept").is_some(), "normal headers kept");

    // HTTP/1.1 keeps them (they're legitimate there).
    let raw1 = raw.replacen("HTTP/2", "HTTP/1.1", 1);
    let (req1, proto1) = crate::commands::repeater_request("https://example.com", &raw1, true, None).unwrap();
    assert_eq!(proto1, Proto::Http1);
    assert!(req1.headers().get("connection").is_some());
    assert!(req1.headers().get("te").is_some());

    // Forcing HTTP/1.1 on an HTTP/2 request line also keeps them (the retry path).
    let (req2, proto2) = crate::commands::repeater_request("https://example.com", raw, true, Some(Proto::Http1)).unwrap();
    assert_eq!(proto2, Proto::Http1);
    assert!(req2.headers().get("te").is_some());
}

/// Prints raw vs compressed sizes for stress-sized projects. Ignored by default;
/// run with: `cargo test stress_compression_stats -- --ignored --nocapture`.
#[test]
#[ignore]
fn stress_compression_stats() {
    use crate::commands::compress_bytes;
    use http::{HeaderMap, Method, StatusCode, Uri, Version};

    fn build(n: usize) -> (usize, usize) {
        let dir = temp_dir(&format!("stress-{n}"));
        let store = Store::open(&dir.join("s.cegat")).unwrap();
        for i in 0..n {
            let uri: Uri = format!("https://api.example.com/v1/users/{}/orders?page={}", i % 5000, i % 20).parse().unwrap();
            let mut rh = HeaderMap::new();
            rh.insert("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36".parse().unwrap());
            rh.insert("accept", "application/json, text/plain, */*".parse().unwrap());
            rh.insert("cookie", format!("session=ab{i:08x}cd; theme=dark; lang=en-US").parse().unwrap());
            rh.insert("authorization", format!("Bearer eyJhbGciOiJIUzI1NiJ9.{i:016x}payload.sig{i:08x}").parse().unwrap());
            let req_body = bytes::Bytes::from(format!("{{\"query\":\"orders\",\"userId\":{i},\"filters\":{{\"status\":\"paid\"}}}}"));
            let mut flow = crate::store::Flow::request(store.next_id(), &Method::POST, &uri, Version::HTTP_11, &rh, req_body);

            let mut sh = HeaderMap::new();
            sh.insert("content-type", "application/json".parse().unwrap());
            sh.insert("server", "nginx".parse().unwrap());
            let res_body = bytes::Bytes::from(format!(
                "{{\"id\":{i},\"user\":{{\"id\":{},\"name\":\"User {i}\",\"email\":\"user{i}@example.com\"}},\"orders\":[{{\"id\":{i},\"total\":19.99,\"items\":3,\"status\":\"paid\",\"created\":\"2026-09-24T10:{:02}:00Z\"}}],\"meta\":{{\"page\":{},\"perPage\":20,\"total\":1234}}}}",
                i % 5000, i % 60, i % 20
            ));
            flow.set_response(StatusCode::OK, Version::HTTP_11, &sh, res_body);
            store.push(flow).unwrap();
        }
        let raw = store.snapshot().unwrap();
        let packed = compress_bytes(&raw).unwrap();
        (raw.len(), packed.len())
    }

    fn mb(n: usize) -> f64 {
        n as f64 / (1024.0 * 1024.0)
    }

    println!("\n  flows |    uncompressed |      compressed |  saved");
    println!("  ------+-----------------+-----------------+-------");
    for n in [5_000usize, 30_000, 50_000] {
        let (raw, packed) = build(n);
        let pct = 100.0 - (packed as f64 * 100.0 / raw as f64);
        println!("  {n:>5} | {:>10.2} MB | {:>10.2} MB | {pct:>4.1}%", mb(raw), mb(packed));
    }
    println!();
}

#[test]
fn compressed_cegat_round_trips_and_shrinks() {
    use crate::commands::{compress_bytes, decompress_bytes, is_compressed};

    // Build a real project with some repetitive, compressible content.
    let dir = temp_dir("compress");
    let store = Store::open(&dir.join("p.cegat")).unwrap();
    let uri: http::Uri = "https://example.com/api".parse().unwrap();
    for _ in 0..200 {
        let body = bytes::Bytes::from_static(b"{\"token\":\"aaaaaaaaaaaaaaaaaaaaaaaa\",\"ok\":true}");
        let mut flow = crate::store::Flow::request(store.next_id(), &http::Method::POST, &uri, http::Version::HTTP_11, &http::HeaderMap::new(), body.clone());
        flow.set_response(http::StatusCode::OK, http::Version::HTTP_11, &http::HeaderMap::new(), body);
        store.push(flow).unwrap();
    }

    let raw = store.snapshot().unwrap();
    assert!(raw.starts_with(b"SQLite format 3\0"));
    assert!(!is_compressed(&raw));

    let packed = compress_bytes(&raw).unwrap();
    assert!(is_compressed(&packed));
    assert!(packed.len() < raw.len(), "compressed {} should be < raw {}", packed.len(), raw.len());

    // Decompressing restores the exact SQLite bytes, which reopen with all rows.
    let back = decompress_bytes(&packed).unwrap();
    assert_eq!(back, raw);

    // The legacy "CEGATC" compressed format is no longer supported: a file with
    // that magic is not recognized as a project and does not decompress.
    let mut legacy = packed.clone();
    legacy[..6].copy_from_slice(b"CEGATC");
    assert!(!is_compressed(&legacy), "legacy CEGATC magic must not be recognized");
    assert!(decompress_bytes(&legacy).is_err(), "legacy CEGATC must not decompress");
    let dest = dir.join("restored.cegat");
    std::fs::write(&dest, &back).unwrap();
    assert_eq!(Store::open(&dest).unwrap().summaries().unwrap().len(), 200);

    // A plain SQLite file is not treated as compressed.
    assert!(decompress_bytes(&raw).is_err());
}

// -----------------------------------------------------------------------------
// Live end-to-end: proxy + MCP driving the vulnerable target app. Ignored by
// default (needs test-target on 127.0.0.1:4000 + Postgres). Run with:
//   cargo test live_mcp_against_vuln_app -- --ignored --nocapture
// -----------------------------------------------------------------------------

#[cfg(test)]
async fn mcp_post(addr: SocketAddr, body: &str) -> Value {
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let resp = raw_exchange(addr, req.as_bytes()).await;
    let body = resp.splitn(2, "\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(Value::Null)
}

#[cfg(test)]
async fn mcp_tool(addr: SocketAddr, name: &str, args: Value) -> Value {
    let call = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
    let resp = mcp_post(addr, &call.to_string()).await;
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    serde_json::from_str(text).unwrap_or(serde_json::json!({ "raw": text, "isError": resp["result"]["isError"] }))
}

#[tokio::test]
#[ignore]
async fn live_mcp_against_vuln_app() {
    use http::Request;
    const APP: &str = "http://127.0.0.1:4000";

    if tokio::net::TcpStream::connect("127.0.0.1:4000").await.is_err() {
        eprintln!("SKIP: vuln target not running on 127.0.0.1:4000");
        return;
    }

    let dir = temp_dir("live-mcp");
    let ctx = std::sync::Arc::new(test_ctx(&dir));
    let proxy = spawn_proxy(ctx.clone()).await;
    let mcp = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(crate::mcp::serve(l, ctx.clone(), "/docs".to_string()));
        a
    };
    tokio::task::yield_now().await;
    eprintln!("\n== Ngintip live MCP test ==\nproxy=127.0.0.1:{}  mcp=127.0.0.1:{}\n", proxy.port(), mcp.port());

    // 0) Clean slate on the target (via an MCP repeater_send, which is recorded).
    let reset = mcp_tool(mcp, "repeater_send", serde_json::json!({ "target": APP, "raw": "POST /reset HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\r\n" })).await;
    eprintln!("[reset]        status={}", reset["status"]);

    // 1) Drive some traffic THROUGH the proxy so History fills.
    for path in ["/", "/users"] {
        let req = Request::get(format!("{APP}{path}")).body(body::empty()).unwrap();
        let res = client_via(proxy).request(req).await.expect("proxy request");
        assert!(res.status().is_success());
    }
    tokio::time::sleep(Duration::from_millis(400)).await; // let the batched writer commit

    // 2) initialize + tools/list
    let init = mcp_post(mcp, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await;
    assert!(init["result"]["protocolVersion"].as_str().unwrap_or("").starts_with("202"), "initialize: {init}");
    let tools = mcp_post(mcp, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
    let names: Vec<String> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    eprintln!("[tools/list]   {} tools: {}", names.len(), names.join(", "));

    // 3) history_list — flows recorded via the proxy are visible over MCP.
    let hist = mcp_tool(mcp, "history_list", serde_json::json!({})).await;
    let total = hist["total"].as_u64().unwrap_or(0);
    eprintln!("[history_list] total={total}");
    assert!(total >= 2, "expected proxied flows in history: {hist}");

    // 4) history_get on the first flow.
    let first_id = hist["flows"][0]["id"].as_u64().unwrap();
    let got = mcp_tool(mcp, "history_get", serde_json::json!({ "id": first_id })).await;
    assert!(got["request"].as_str().unwrap_or("").contains("HTTP/1.1"), "history_get: {got}");
    eprintln!("[history_get]  flow #{first_id} request/response fetched");

    // 5) THE RACE: fire the same registration 30x in parallel via MCP.
    let reg = "POST /register HTTP/1.1\r\nHost: 127.0.0.1:4000\r\nContent-Type: application/json\r\n\r\n{\"email\":\"race@nikko.id\",\"password\":\"x\"}";
    let batch = mcp_tool(mcp, "repeater_send_batch", serde_json::json!({ "target": APP, "raw": reg, "count": 30, "mode": "parallel" })).await;
    let created = batch["byStatus"]["201"].as_u64().unwrap_or(0);
    let conflicts = batch["byStatus"]["409"].as_u64().unwrap_or(0);
    eprintln!("[race batch]   sent={} 201(created)={created} 409(rejected)={conflicts}", batch["sent"]);

    // Confirm duplicates landed in the DB via /users.
    let users = mcp_tool(mcp, "repeater_send", serde_json::json!({ "target": APP, "raw": "GET /users HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\r\n" })).await;
    let resp_raw = users["response"].as_str().unwrap_or("");
    // Repeater responses use \n line endings, so the header/body split is \n\n.
    let ubody = resp_raw
        .find("\r\n\r\n")
        .map(|i| &resp_raw[i + 4..])
        .or_else(|| resp_raw.find("\n\n").map(|i| &resp_raw[i + 2..]))
        .unwrap_or("");
    let uj: Value = serde_json::from_str(ubody).unwrap_or(Value::Null);
    let dupes = uj["duplicates"].as_array().map(|a| a.len()).unwrap_or(0);
    eprintln!("[/users]       total_users={} duplicate_emails={dupes} -> {}", uj["total"], uj["duplicates"]);
    assert!(created >= 2, "race not won: only {created} registrations succeeded (byStatus={})", batch["byStatus"]);
    assert!(dupes >= 1, "expected duplicate emails in the DB after the race");

    // 5b) THE FIX: MCP-driven sends are recorded in History, not just proxy traffic.
    tokio::time::sleep(Duration::from_millis(400)).await; // let the batched flow writer commit
    let reg_hist = mcp_tool(mcp, "history_list", serde_json::json!({ "method": "POST", "contains": "/register", "limit": 1000 })).await;
    let reg_count = reg_hist["total"].as_u64().unwrap_or(0);
    eprintln!("[history POST /register] total={reg_count} MCP-sent flows recorded in History");
    assert!(reg_count >= 2, "MCP sends not recorded in History: expected >=2 POST /register flows, got {reg_count}: {reg_hist}");
    // And a recorded flow carries the real response (201/409), not a placeholder.
    let reg_id = reg_hist["flows"][0]["id"].as_u64().unwrap();
    let reg_flow = mcp_tool(mcp, "history_get", serde_json::json!({ "id": reg_id })).await;
    let reg_resp = reg_flow["response"].as_str().unwrap_or("");
    assert!(reg_resp.contains(" 201 ") || reg_resp.contains(" 409 "), "recorded flow missing real response: {reg_flow}");

    // 6) Sequential control: after reset, sequential sends must NOT dupe.
    mcp_tool(mcp, "repeater_send", serde_json::json!({ "target": APP, "raw": "POST /reset HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\r\n" })).await;
    let seq = mcp_tool(mcp, "repeater_send_batch", serde_json::json!({ "target": APP, "raw": reg, "count": 10, "mode": "sequential" })).await;
    let seq_created = seq["byStatus"]["201"].as_u64().unwrap_or(0);
    eprintln!("[seq control]  201(created)={seq_created} (should be exactly 1)");
    assert_eq!(seq_created, 1, "sequential must create exactly one: {}", seq["byStatus"]);

    // 7) Intruder: sniper over the id path (SQLi/IDOR-style probe) hitting /users/§n§.
    let intr = mcp_tool(mcp, "intruder_run", serde_json::json!({
        "target": APP,
        "raw": "GET /users/\u{a7}1\u{a7} HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\r\n",
        "type": "sniper",
        "payloads": ["1", "2", "999999"],
        "concurrency": 5
    })).await;
    eprintln!("[intruder]     sent={} byStatus={}", intr["sent"], intr["byStatus"]);
    assert_eq!(intr["sent"].as_u64().unwrap_or(0), 3);

    // 8) Passive scan over everything captured so far.
    let scan = mcp_tool(mcp, "passive_scan", serde_json::json!({})).await;
    eprintln!("[passive_scan] scannedFlows={} findings={} bySeverity={}", scan["scannedFlows"], scan["findings"], scan["bySeverity"]);
    let findings = mcp_tool(mcp, "passive_findings", serde_json::json!({})).await;
    eprintln!("[findings]     {} stored", findings["count"]);

    eprintln!("\n== ALL MCP TOOLS OK ==\n");
}
