//! Intercepting proxy: plain HTTP forwarding + CONNECT → TLS MITM (or plain
//! HTTP, e.g. `ws://` tunnelled by browsers), WebSocket pass-through.
//! HTTP/2 is negotiated with the client via ALPN and mirrored upstream.
//! Bodies stream through; History keeps the first `body::CAPTURE_LIMIT` bytes.

use std::{
    collections::HashMap,
    convert::Infallible,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, mpsc, Arc, Mutex, OnceLock, RwLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use http::{
    header,
    uri::{Authority, Scheme},
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri, Version,
};
use hyper::{
    body::Incoming,
    server::conn::{http1, http2},
    service::service_fn,
    upgrade::{OnUpgrade, Upgraded},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{LazyConfigAcceptor, TlsAcceptor};

use crate::{
    body::{self, SharedCapture, Tee, CAPTURE_LIMIT},
    brand,
    ca::CertAuthority,
    intercept::{Decision, Intercept, PendingItem, Phase, INTERCEPT_EVENT},
    rules::{Rules, Target},
    scope::Scope,
    raw,
    store::{header_vec, Flow, Store},
    upstream::{self, HttpClient, UpstreamProxy},
    ws,
};

pub use crate::body::Body;

/// Event name the UI listens on for each completed flow (and for previews of
/// flows still streaming, which a later event with the same id replaces).
pub const FLOW_EVENT: &str = "flow";

/// A response still streaming after this long shows up in History early.
const STREAMING_PREVIEW: Duration = Duration::from_secs(1);
/// How long a request may be out, with no response yet, before History shows it on its
/// own. Same reasoning as the streaming preview above: short enough that a request in
/// flight is visible, long enough that an ordinary fast exchange is still one write.
pub(crate) const PENDING_ROW: Duration = Duration::from_secs(1);

/// Said of a flow once Ngintip has stopped waiting for its answer and none was ever
/// filed. It states a fact about Ngintip's record and nothing about the target, which
/// may well have answered an operator who was no longer listening.
///
/// A request that is merely still in flight must NOT carry this, or anything else, in
/// `error`: that column names a failing party, and while the wait is still running
/// there is no party to name. An in-flight row is `status = 0` with `error` empty and
/// the session-only `pending` flag set, which is the whole difference between "no news
/// yet" and "no news ever". Rows written by builds before that distinction existed say
/// this from one second in, so reading code still has to treat it as either.
pub(crate) const NO_RESPONSE: &str = "no response recorded";

/// The error recorded when Ngintip stops waiting at a deadline the operator set.
///
/// Worded to name Ngintip rather than the target. The target was never observed to
/// fail: it was still being waited for when the configured patience ran out, and it may
/// well have answered a moment later. This is the one case where a clock may end a row,
/// because the clock did not merely pass, it was acted on: the request really was
/// dropped at that instant, so the row describes something that happened.
fn gave_up(after: Option<Duration>) -> anyhow::Error {
    anyhow::anyhow!(
        "Ngintip stopped waiting after {}s (Settings > Project > Performance). The target was not shown to have failed.",
        after.map_or(0, |d| d.as_secs())
    )
}

/// h2 request bodies up to this size are buffered so a peer stream reset can be
/// replayed over HTTP/1.1 (see `forward`). Larger bodies keep streaming and are
/// not retried, so big uploads aren't held in memory.
const RETRY_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Event name the UI listens on for each log line.
pub const LOG_EVENT: &str = "log";

/// Event name the UI listens on when the passive findings stored in the project were
/// replaced by something other than the window, which today means an MCP `passive_scan`.
///
/// The window keeps its own in-memory copy of `passiveFindings` and writes the whole
/// array back on any passive action, so without this the agent's findings were invisible
/// to the operator and erased by their next click.
pub const PASSIVE_EVENT: &str = "passive";

/// Event name the UI listens on when an MCP tool replaced a per-project setting the
/// window also caches and writes back wholesale. Carries the setting's key.
///
/// Without it the window shows the stale value AND overwrites the agent's write on the
/// operator's next edit, which for `scope` is worse than a display bug: the Delete
/// out-of-scope action decides what to delete from the window's cached copy.
pub const SETTING_EVENT: &str = "setting";
/// How many recent log lines the Logs view loads.
const LOG_LIMIT: usize = 2000;

pub use crate::store::LogEntry;

/// Where proxy events go: the Tauri window in the app, a recorder in tests.
pub trait Events: Send + Sync {
    fn emit_json(&self, event: &str, payload: serde_json::Value);
}

impl dyn Events {
    pub fn emit(&self, event: &str, payload: &impl Serialize) {
        if let Ok(value) = serde_json::to_value(payload) {
            self.emit_json(event, value);
        }
    }
}

/// How requests leave the proxy; replaced as a whole when the setting changes
/// so no pooled connection keeps using the old route.
struct Route {
    /// HTTP/1.1 only.
    h1: HttpClient,
    /// Offers HTTP/2 (ALPN), falls back to HTTP/1.1.
    auto: HttpClient,
    proxy: Option<Arc<UpstreamProxy>>,
}

impl Route {
    fn new(proxy: Option<Arc<UpstreamProxy>>, overrides: upstream::Overrides) -> Result<Self> {
        Ok(Self {
            h1: upstream::client(proxy.clone(), false, overrides.clone())?,
            auto: upstream::client(proxy.clone(), true, overrides)?,
            proxy,
        })
    }
}

/// Protocol to use towards the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Proto {
    Http1,
    /// HTTP/2 when the server agrees, else HTTP/1.1.
    PreferHttp2,
}

pub struct Ctx {
    pub events: Box<dyn Events>,
    pub store: Store,
    pub ca: Arc<CertAuthority>,
    route: RwLock<Route>,
    pub intercept: Intercept,
    /// Intercept queue for WebSocket data frames.
    pub ws_intercept: ws::WsIntercept,
    pub scope: Scope,
    pub rules: Rules,
    /// Python extensions that can transform requests/responses.
    pub extensions: crate::extensions::Extensions,
    /// Addresses our listeners are bound to, for spotting requests aimed at ourselves.
    /// A set because several listeners (different host:port) can run at once.
    listeners: RwLock<Vec<SocketAddr>>,
    /// Hostname -> IP overrides applied to direct connections (an app-scoped hosts
    /// file). Shared with every upstream connector so a change takes effect live.
    host_overrides: upstream::Overrides,
    /// `user:pass` clients must present, when proxy auth is on.
    auth: RwLock<Option<String>>,
    /// Invisible proxying: accept direct (non-proxy-aware) connections and use
    /// the SNI / Host header as the destination.
    invisible: AtomicBool,
    /// Milliseconds to wait for a response before giving up; 0 = wait forever.
    /// Live, so a change applies to the next request with no restart.
    give_up_ms: AtomicU64,
    /// Background writer that batches flow inserts; started on first use.
    /// Flows queued for the writer thread, each tagged with the store generation it
    /// was captured under (see Store::generation).
    writer: OnceLock<mpsc::Sender<(u64, Flow)>>,
    /// The waiting room for "please draw this" requests sent to the window (see
    /// render.rs). Not proxy business, but Ctx is what an MCP handler holds, and a
    /// Tauri command can reach it through `state.ctx` to deliver the answer.
    pub render: crate::render::Bridge,
    /// Sockets the Repeater has open. Behind an Arc because the task that owns a socket
    /// outlives the call that created it and has to forget itself when the socket ends.
    pub ws_repeater: std::sync::Arc<crate::wsrepeater::Sockets>,
    /// Hosts whose HTTP/2 upstream reset the stream (e.g. strict servers that send
    /// GOAWAY): we speak HTTP/1.1 to them directly for a while instead of failing
    /// an h2 attempt on every request. Re-probed after the TTL (self-healing).
    h2_broken: RwLock<HashMap<String, Instant>>,
}

/// How long a host stays on the HTTP/1.1-only list after an h2 stream reset.
const H2_BROKEN_TTL: Duration = Duration::from_secs(300);

impl Ctx {
    pub fn new(events: Box<dyn Events>, ca: Arc<CertAuthority>, store: Store) -> Result<Self> {
        let host_overrides: upstream::Overrides = Arc::new(RwLock::new(HashMap::new()));
        Ok(Self {
            events,
            store,
            ca,
            route: RwLock::new(Route::new(None, host_overrides.clone())?),
            intercept: Intercept::default(),
            ws_intercept: ws::WsIntercept::default(),
            scope: Scope::default(),
            rules: Rules::default(),
            extensions: crate::extensions::Extensions::default(),
            listeners: RwLock::new(Vec::new()),
            host_overrides,
            auth: RwLock::new(None),
            invisible: AtomicBool::new(false),
            give_up_ms: AtomicU64::new(0),
            writer: OnceLock::new(),
            render: crate::render::Bridge::default(),
            ws_repeater: std::sync::Arc::new(crate::wsrepeater::Sockets::default()),
            h2_broken: RwLock::new(HashMap::new()),
        })
    }

    /// Whether HTTP/2 to `host` recently reset and should be skipped for now.
    fn h2_is_broken(&self, host: &str) -> bool {
        self.h2_broken
            .read()
            .unwrap()
            .get(host)
            .is_some_and(|t| t.elapsed() < H2_BROKEN_TTL)
    }

    /// Remember that HTTP/2 to `host` reset, so later requests use HTTP/1.1 directly.
    fn mark_h2_broken(&self, host: &str) {
        if host.is_empty() {
            return;
        }
        self.h2_broken.write().unwrap().insert(host.to_string(), Instant::now());
    }

    /// Record a log line: persist it with the project, tell the UI, echo to stderr.
    pub fn log(&self, level: &'static str, source: &str, message: impl Into<String>) {
        let ts = crate::store::now_ms();
        let source = source.to_string();
        let message = message.into();
        let id = self.store.push_log(ts, level, &source, &message).unwrap_or(0);
        let entry = LogEntry { id, ts, level: level.to_string(), source, message };
        self.events.emit(LOG_EVENT, &entry);
        eprintln!("[{}] {}", entry.source, entry.message);
    }

    pub fn log_error(&self, source: &str, message: impl Into<String>) {
        self.log("error", source, message);
    }
    pub fn log_warn(&self, source: &str, message: impl Into<String>) {
        self.log("warn", source, message);
    }
    pub fn log_info(&self, source: &str, message: impl Into<String>) {
        self.log("info", source, message);
    }
    pub fn log_debug(&self, source: &str, message: impl Into<String>) {
        self.log("debug", source, message);
    }

    pub fn logs(&self) -> Vec<LogEntry> {
        self.store.recent_logs(LOG_LIMIT).unwrap_or_default()
    }

    pub fn clear_logs(&self) {
        let _ = self.store.clear_logs();
    }

    /// Route traffic through `proxy`, or directly when None.
    pub fn set_upstream(&self, proxy: Option<UpstreamProxy>) -> Result<()> {
        *self.route.write().unwrap() = Route::new(proxy.map(Arc::new), self.host_overrides.clone())?;
        Ok(())
    }

    /// Replace the hostname -> IP overrides for direct connections. Takes effect live:
    /// the map is shared with every connector, so no route rebuild is needed.
    pub fn set_host_overrides(&self, overrides: &[crate::config::HostOverride]) {
        let mut map = self.host_overrides.write().unwrap();
        map.clear();
        for o in overrides {
            let host = o.host.trim().to_ascii_lowercase();
            let ip = o.ip.trim().to_string();
            if o.enabled && !host.is_empty() && !ip.is_empty() {
                map.insert(host, ip);
            }
        }
    }

    /// Send a request to its target, through the upstream proxy if one is set.
    pub async fn send(&self, mut req: Request<Body>, proto: Proto) -> Result<Response<Incoming>> {
        // Every upstream request funnels through here (live proxy, Repeater,
        // Intruder, MCP), so fold split `cookie` fields into one `Cookie:` header
        // in one place. An HTTP/2 client (or a request replayed from History, which
        // shows the split fields) carries cookies across several `cookie` lines;
        // an HTTP/1.1 origin (e.g. PHP / CodeIgniter) reads only the first and
        // drops the rest, losing the session / CSRF cookies and bouncing to login.
        fold_cookies(req.headers_mut());
        let client = {
            let route = self.route.read().unwrap();
            // Absolute-form requests carry the upstream credential themselves;
            // CONNECT tunnels (https) already sent it.
            if req.uri().scheme() != Some(&Scheme::HTTPS) {
                if let Some(auth) = route
                    .proxy
                    .as_ref()
                    .filter(|p| p.kind == crate::upstream::Kind::Http)
                    .and_then(|p| p.http_auth())
                {
                    req.headers_mut().insert(header::PROXY_AUTHORIZATION, auth);
                }
            }
            match proto {
                Proto::Http1 => route.h1.clone(),
                Proto::PreferHttp2 => route.auto.clone(),
            }
        };
        // The negotiated protocol decides; an HTTP/2 connection ignores this.
        *req.version_mut() = Version::HTTP_11;
        Ok(client.request(req).await?)
    }

    pub fn set_auth(&self, credentials: Option<String>) {
        *self.auth.write().unwrap() = credentials;
    }

    pub fn set_invisible(&self, on: bool) {
        self.invisible.store(on, Ordering::Relaxed);
    }
    pub fn invisible(&self) -> bool {
        self.invisible.load(Ordering::Relaxed)
    }
    /// Seconds to wait for a response before giving up; 0 = wait as long as it takes.
    pub fn set_give_up_after(&self, secs: u64) {
        self.set_give_up_ms(secs.saturating_mul(1000));
    }
    /// The same deadline in milliseconds. The setting is in seconds because that is the
    /// unit an operator thinks in; tests need a deadline they do not have to wait out.
    pub(crate) fn set_give_up_ms(&self, ms: u64) {
        self.give_up_ms.store(ms, Ordering::Relaxed);
    }
    /// Seconds currently allowed for one exchange; 0 = no deadline. Exposed so the
    /// live value can be read back, because a setting that only the UI can see is a
    /// setting nobody can tell apart from one that never arrived.
    pub fn give_up_after(&self) -> u64 {
        self.give_up_ms.load(Ordering::Relaxed) / 1000
    }
    /// The deadline for one upstream exchange, or None when there is none.
    fn give_up(&self) -> Option<Duration> {
        match self.give_up_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    /// Checks `Proxy-Authorization: Basic …` when proxy auth is on.
    fn authorized(&self, headers: &HeaderMap) -> bool {
        let expected = self.auth.read().unwrap();
        let Some(expected) = expected.as_deref() else {
            return true;
        };
        headers
            .get(header::PROXY_AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(basic_credentials)
            .is_some_and(|given| constant_time_eq(given.as_bytes(), expected.as_bytes()))
    }

    /// Record a bound listener address (deduped), so `is_self` can spot loopbacks.
    pub fn add_listener(&self, addr: SocketAddr) {
        let mut v = self.listeners.write().unwrap();
        if !v.contains(&addr) {
            v.push(addr);
        }
    }

    /// The addresses listeners are actually bound to right now.
    ///
    /// Read-only on purpose. The MCP server is handed an `Arc<Ctx>` and nothing else, so
    /// this is the whole of what it can learn about the proxy's own sockets: it cannot
    /// bind, rebind or stop one, and the compiler keeps it that way. Reporting bound
    /// addresses rather than configured ones also means "nothing is bound" is answerable,
    /// which is the operationally useful statement.
    pub fn bound_listeners(&self) -> Vec<SocketAddr> {
        self.listeners.read().unwrap().clone()
    }

    /// Forget a listener address when its listener stops.
    pub fn remove_listener(&self, addr: &SocketAddr) {
        self.listeners.write().unwrap().retain(|a| a != addr);
    }

    /// True when `uri` points back at any of our own listeners, so forwarding it
    /// would just send the request to ourselves again.
    pub fn is_self(&self, uri: &Uri) -> bool {
        let listeners = self.listeners.read().unwrap();
        if listeners.is_empty() {
            return false;
        }
        let default_port = if uri.scheme() == Some(&Scheme::HTTPS) { 443 } else { 80 };
        let uri_port = uri.port_u16().unwrap_or(default_port);
        let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
        let ip = if host.eq_ignore_ascii_case("localhost") {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            match host.parse::<IpAddr>() {
                Ok(ip) => ip,
                Err(_) => return false,
            }
        };
        listeners.iter().any(|listener| {
            if uri_port != listener.port() {
                return false;
            }
            let bound = listener.ip();
            if bound.is_unspecified() {
                // 0.0.0.0 / :: accepts on loopback and every local interface.
                ip.is_loopback()
                    || ip.is_unspecified()
                    || if_addrs::get_if_addrs()
                        .map(|nics| nics.iter().any(|n| n.ip() == ip))
                        .unwrap_or(false)
            } else {
                ip == bound || (bound.is_loopback() && ip.is_loopback())
            }
        })
    }
}

pub async fn serve(listener: TcpListener, ctx: Arc<Ctx>) {
    if let Ok(addr) = listener.local_addr() {
        ctx.add_listener(addr);
        ctx.log_info("proxy", format!("listening on {addr}"));
    }
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                ctx.log_error("proxy", format!("accept failed: {e}"));
                continue;
            }
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            // Invisible proxying: a non-proxy-aware client connects straight to us.
            // A TLS ClientHello (0x16) is MITM'd using its SNI; plain HTTP is served
            // normally and routed by its Host header (see handle()).
            if ctx.invisible() {
                let mut peek = [0u8; 1];
                if matches!(stream.peek(&mut peek).await, Ok(1)) && peek[0] == 0x16 {
                    if let Err(e) = mitm_direct(stream, ctx.clone()).await {
                        ctx.log_error("tls", format!("direct TLS (invisible) failed: {e:#}"));
                    }
                    return;
                }
            }
            serve_http1(stream, ctx).await;
        });
    }
}

/// Serve plain HTTP/1.1 (proxy requests, or invisible plaintext routed by Host).
async fn serve_http1(stream: TcpStream, ctx: Arc<Ctx>) {
    let service = service_fn(move |req| {
        let ctx = ctx.clone();
        async move { Ok::<_, Infallible>(handle(req, ctx).await) }
    });
    let _ = http1::Builder::new()
        .preserve_header_case(true)
        .title_case_headers(true)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
}

/// A direct TLS connection (invisible proxy): read the SNI, forge a cert for it,
/// then serve the requests inside, forwarding to that host over HTTPS.
async fn mitm_direct(stream: TcpStream, ctx: Arc<Ctx>) -> Result<()> {
    let start = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), stream).await?;
    let host = start
        .client_hello()
        .server_name()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("direct TLS connection without SNI"))?;
    let config = ctx.ca.server_config(&host)?;
    let tls = start.into_stream(config).await?;
    let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
    let authority: Authority = format!("{host}:443").parse()?;
    serve_tunnel(TokioIo::new(tls), Scheme::HTTPS, authority, h2, ctx).await
}

async fn handle(mut req: Request<Incoming>, ctx: Arc<Ctx>) -> Response<Body> {
    // Anything we'd forward needs credentials; the landing page (origin-form) doesn't.
    let proxied = req.method() == Method::CONNECT || req.uri().authority().is_some();
    if proxied && !ctx.authorized(req.headers()) {
        return Response::builder()
            .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
            .header(header::PROXY_AUTHENTICATE, format!("Basic realm=\"{}\"", brand::NAME))
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(body::full(Bytes::from(format!("{}: proxy authentication required", brand::NAME))))
            .unwrap();
    }
    // Our own credential: never forwarded, never recorded in History.
    req.headers_mut().remove(header::PROXY_AUTHORIZATION);

    if req.method() == Method::CONNECT {
        let Some(authority) = req.uri().authority().cloned() else {
            return text(StatusCode::BAD_REQUEST, "invalid CONNECT target");
        };
        let log_ctx = ctx.clone();
        tokio::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    if let Err(e) = mitm(upgraded, authority.clone(), ctx).await {
                        log_ctx.log_error("tls", format!("TLS handshake with {authority} failed (the client may pin certificates): {e:#}"));
                    }
                }
                Err(e) => log_ctx.log_error("proxy", format!("CONNECT upgrade failed: {e}")),
            }
        });
        return Response::new(body::empty());
    }

    // Origin-form request (no authority): the proxy's own landing page, unless
    // invisible proxying is on and the Host header names another destination.
    if req.uri().authority().is_none() {
        if ctx.invisible() {
            if let Some(dest) = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).and_then(|h| h.parse::<Authority>().ok()) {
                let looks_self = format!("http://{dest}/").parse::<Uri>().map(|u| ctx.is_self(&u)).unwrap_or(false);
                if !looks_self {
                    return forward(req, Some((Scheme::HTTP, dest)), ctx).await;
                }
            }
        }
        return landing(req.uri(), &ctx);
    }
    // An absolute request naming our own address: answer here instead of looping.
    if ctx.is_self(req.uri()) {
        return landing(req.uri(), &ctx);
    }

    forward(req, None, ctx).await
}

/// Proxy the requests inside a CONNECT tunnel. TLS (the usual case) is
/// terminated with a forged cert; plain HTTP (browsers tunnel `ws://` this
/// way) is served as-is.
async fn mitm(upgraded: Upgraded, authority: Authority, ctx: Arc<Ctx>) -> Result<()> {
    let mut io = TokioIo::new(upgraded);
    let mut first = [0u8; 1];
    if io.read(&mut first).await? == 0 {
        return Ok(());
    }
    let io = Rewind {
        prefix: Some(Bytes::copy_from_slice(&first)),
        inner: io,
    };

    // 0x16 = TLS handshake record (ClientHello).
    if first[0] == 0x16 {
        let config = ctx.ca.server_config(authority.host())?;
        let tls = TlsAcceptor::from(config).accept(io).await?;
        let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
        serve_tunnel(TokioIo::new(tls), Scheme::HTTPS, authority, h2, ctx).await
    } else {
        serve_tunnel(TokioIo::new(io), Scheme::HTTP, authority, false, ctx).await
    }
}

async fn serve_tunnel<I>(io: I, scheme: Scheme, authority: Authority, h2: bool, ctx: Arc<Ctx>) -> Result<()>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let service = service_fn(move |req| {
        let (ctx, tunnel) = (ctx.clone(), (scheme.clone(), authority.clone()));
        async move { Ok::<_, Infallible>(forward(req, Some(tunnel), ctx).await) }
    });
    if h2 {
        http2::Builder::new(TokioExecutor::new()).serve_connection(io, service).await?;
    } else {
        http1::Builder::new()
            .preserve_header_case(true)
            .title_case_headers(true)
            .serve_connection(io, service)
            .with_upgrades()
            .await?;
    }
    Ok(())
}

/// Bytes already read from `inner` (to sniff the protocol), served again first.
struct Rewind<T> {
    prefix: Option<Bytes>,
    inner: T,
}

impl<T: AsyncRead + Unpin> AsyncRead for Rewind<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if let Some(mut prefix) = self.prefix.take() {
            let n = prefix.len().min(buf.remaining());
            buf.put_slice(&prefix.split_to(n));
            if !prefix.is_empty() {
                self.prefix = Some(prefix);
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Rewind<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A request body either read in full (the user may edit it) or passed through.
enum Payload {
    Full(Bytes),
    Stream(Body),
}

/// `tunnel` is the scheme and CONNECT target for requests arriving inside a tunnel.
async fn forward(
    req: Request<Incoming>,
    tunnel: Option<(Scheme, Authority)>,
    ctx: Arc<Ctx>,
) -> Response<Body> {
    let (mut parts, body) = req.into_parts();

    parts.uri = match absolute_uri(&parts.uri, tunnel.as_ref()) {
        Ok(uri) => uri,
        Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    // A tunnel opened to our own address (CONNECT 127.0.0.1:8080).
    if ctx.is_self(&parts.uri) {
        return landing(&parts.uri, &ctx);
    }
    // The generation is taken HERE, with the id, not when the flow is finally queued:
    // between the two sits the whole upstream round trip, and a project switch inside it
    // would otherwise make this request look like it belonged to the new project.
    let (id, gen) = ctx.store.next_id_at();
    // Upstream host, for the "h2 recently broke here" check below.
    let target_host = parts.uri.host().unwrap_or_default().to_string();

    // HTTP/2 carries the host in :authority; show it (and send it on HTTP/1.1) as Host.
    let client_h2 = parts.version == Version::HTTP_2;
    if client_h2 && !parts.headers.contains_key(header::HOST) {
        if let Some(auth) = parts.uri.authority() {
            // Match a real browser: the Host header omits the scheme's default port
            // (443 for https, 80 for http). The tunnel authority carries ":443" from
            // the CONNECT target, and a non-canonical `Host: site:443` can trip picky
            // vhosts / WAFs and make frameworks build wrong absolute URLs.
            let host = host_header_value(parts.uri.scheme(), auth);
            if let Ok(value) = HeaderValue::from_str(&host) {
                parts.headers.insert(header::HOST, value);
            }
        }
    }

    // WebSocket handshake: the client's side of the upgrade is ours to finish.
    let websocket = is_websocket(&parts.headers);
    let client_upgrade = if websocket { parts.extensions.remove::<OnUpgrade>() } else { None };
    if websocket {
        // No permessage-deflate, so frames stay readable in History.
        parts.headers.remove(header::SEC_WEBSOCKET_EXTENSIONS);
    }

    // Match & Replace runs first, so intercept shows what will actually be sent.
    ctx.rules.apply_request_line(&mut parts.method, &mut parts.uri);
    ctx.rules.apply_headers(Target::RequestHeader, &mut parts.headers);

    // Only in-scope messages are held; an empty scope holds everything. The advanced
    // filter (if any) narrows that further. The filter locks + allocates, so it is
    // only evaluated when a hold is actually possible (intercept on and in scope),
    // keeping the common "intercept off" case free on the hot path.
    let in_scope = ctx.scope.contains(parts.uri.host().unwrap_or_default());
    let intercept_on = ctx.intercept.is_enabled();
    let passes_filter = intercept_on
        && in_scope
        && {
            let host = parts.uri.host().unwrap_or_default();
            let path = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
            let scheme = parts.uri.scheme_str().unwrap_or("http");
            ctx.intercept.allows(parts.method.as_str(), host, path, &format!("{scheme}://{host}{path}"))
        };
    let hold = intercept_on && in_scope && passes_filter;
    // Extensions need the whole body to transform it, so an in-scope request also
    // gets buffered when an extension is active (cheap atomic check when off).
    let ext_active = in_scope && ctx.extensions.active();
    let mut wants_response = false;

    // The request will go upstream over h2 (mirroring the client) unless it's a
    // WebSocket upgrade (needs HTTP/1.1) or the host's h2 recently reset (then we
    // go straight to HTTP/1.1 instead of failing an h2 attempt every time).
    let will_prefer_h2 = client_h2 && !websocket && !ctx.h2_is_broken(&target_host);

    // Read the body up front when the user or a body rule needs all of it, or -
    // for h2 requests - so a peer stream reset can be replayed over HTTP/1.1.
    // Bodies past the limit keep streaming (and can't be retried).
    let mut payload = if hold || ext_active || ctx.rules.has(Target::RequestBody) {
        match body::buffer(body, CAPTURE_LIMIT).await {
            Ok(body::Buffered::Complete(bytes)) => Payload::Full(bytes),
            // Too large to edit or rewrite: let it through untouched.
            Ok(body::Buffered::TooLarge(rest)) => Payload::Stream(body::boxed(rest)),
            Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
        }
    } else if will_prefer_h2 {
        match body::buffer(body, RETRY_BODY_LIMIT).await {
            Ok(body::Buffered::Complete(bytes)) => Payload::Full(bytes),
            Ok(body::Buffered::TooLarge(rest)) => Payload::Stream(body::boxed(rest)),
            Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
        }
    } else {
        Payload::Stream(body::boxed(body))
    };

    if let Payload::Full(bytes) = &mut payload {
        *bytes = ctx.rules.apply_body(Target::RequestBody, &mut parts.headers, std::mem::take(bytes));
    }

    // The client's request before any intercept edit, kept as a History variant.
    let mut req_original: Option<String> = None;
    // Whether this request went through the intercept queue: if so, its extension
    // transform happens at the Interceptor surface (below) and the Proxy-surface
    // transform is skipped, so an extension scoped to both never runs twice.
    let mut intercepted_request = false;
    if let (true, Payload::Full(bytes)) = (hold, &mut payload) {
        intercepted_request = true;
        let held = Flow::request(id, &parts.method, &parts.uri, parts.version, &parts.headers, bytes.clone()).at(gen);
        let arrived = crate::raw::request_text(&held);
        let host = held.summary.host.clone();
        let path = held.summary.path.clone();
        // What the editor shows: process_* is a real wire change; decode_* makes the body
        // readable for editing WITHOUT changing the wire. The editor shows the decoded
        // view when there is one, keeping the arrived message as Original.
        let (processed, display) = if ext_active {
            let processing = crate::raw::request_text_for_processing(&held);
            let ext = ctx.clone();
            let (h, p) = (host.clone(), path.clone());
            tauri::async_runtime::spawn_blocking(move || {
                let processed = ext
                    .extensions
                    .transform(
                        crate::extensions::Phase::Request,
                        processing.as_bytes(),
                        crate::extensions::Surface::Interceptor,
                        &crate::extensions::HookCtx { host: h.clone(), path: p.clone(), flow_id: Some(id), ..Default::default() },
                    )
                    .map(|o| String::from_utf8_lossy(&o).replace("\r\n", "\n"));
                let base = processed.clone().unwrap_or(processing);
                let hc = crate::extensions::HookCtx { host: h, path: p, flow_id: Some(id), ..Default::default() };
                let display = ext
                    .extensions
                    .decode(crate::extensions::Phase::Request, base.as_bytes(), crate::extensions::Surface::Interceptor, &hc)
                    .map(|o| String::from_utf8_lossy(&o).replace("\r\n", "\n"));
                (processed, display)
            })
            .await
            .unwrap_or((None, None))
        } else {
            (None, None)
        };
        let shown = display.or(processed.clone());
        let notify = |item: &PendingItem| ctx.events.emit(INTERCEPT_EVENT, item);
        match ctx.intercept.hold(Phase::Request, &held, shown, notify).await {
            Decision::Forward { raw, hold_response } => {
                wants_response = hold_response;
                // The wire form: an edit is encoded back (e.g. re-encrypted); an unchanged
                // forward sends the process change if any, else the original bytes (never
                // the decoded display).
                let wire = match raw {
                    Some(user_edit) if ext_active => {
                        let ext = ctx.clone();
                        let (h, p) = (host.clone(), path.clone());
                        Some(
                            tauri::async_runtime::spawn_blocking(move || {
                                let hc = crate::extensions::HookCtx { host: h, path: p, flow_id: Some(id), edited: true, ..Default::default() };
                                ext.extensions
                                    .encode(crate::extensions::Phase::Request, user_edit.as_bytes(), crate::extensions::Surface::Interceptor, &hc)
                                    .map(|o| String::from_utf8_lossy(&o).replace("\r\n", "\n"))
                                    .unwrap_or(user_edit)
                            })
                            .await
                            .unwrap_or_default(),
                        )
                    }
                    Some(user_edit) => Some(user_edit),
                    None => processed,
                };
                if let Some(edited) = wire {
                    // Remember the pre-edit request so History can show Original vs Final.
                    req_original = Some(arrived);
                    match apply_edit(&mut parts, &edited, bytes) {
                        Ok(body) => *bytes = body,
                        // Forward was pressed, so as far as the operator is concerned the
                        // request left. It did not: Ngintip answered 400 itself. The row
                        // keeps the request as it arrived, which is the only version of
                        // it that exists, and says why nothing was sent.
                        Err(e) => {
                            let mut flow = held;
                            flow.set_error(format!("edited request invalid: {e:#}"));
                            ctx.record_flow(flow);
                            return text(StatusCode::BAD_REQUEST, &format!("{}: edited request invalid: {e:#}", brand::NAME));
                        }
                    }
                }
            }
            Decision::Drop => {
                let mut flow = held;
                flow.set_error("dropped (intercept)".into());
                ctx.record_flow(flow);
                return text(StatusCode::BAD_GATEWAY, &format!("Request dropped by {}", brand::NAME));
            }
        }
    }

    // Timing starts after any intercept pause.
    let started = Instant::now();

    // The row as the request arrived, and the guard that files it if this handler dies
    // before the exchange reaches an outcome. Both are set up HERE, in front of the
    // extension transform below, because that transform is a subprocess round trip that
    // this task awaits: a client that gives up inside that window used to take the whole
    // handler with it and leave no record that the request was ever made. The guard
    // knows the request has not gone upstream yet (`sent`), so the row it files says
    // that rather than implying the target received something it never did.
    let req_capture = SharedCapture::default();
    let recorded = Recorded::new();
    let mut flow = Flow::request(id, &parts.method, &parts.uri, parts.version, &parts.headers, Bytes::new()).at(gen);
    flow.variants.req_original = req_original.clone();
    let mut abandoned = AbandonGuard {
        ctx: ctx.clone(),
        recorded: recorded.clone(),
        capture: req_capture.clone(),
        flow: Some(flow.clone()),
        sent: false,
    };

    // Extension transform of the outgoing request (buffered bodies only, in scope):
    // e.g. encrypt an edited plaintext body before it goes upstream. Skipped for
    // intercepted requests, which were already transformed at the Interceptor surface.
    if ext_active && !intercepted_request {
        if let Payload::Full(bytes) = &mut payload {
            let held = Flow::request(id, &parts.method, &parts.uri, parts.version, &parts.headers, bytes.clone()).at(gen);
            // The extension needs the real payload bytes (decoded, uncapped, no hex
            // view), or a binary/large body would be corrupted or truncated.
            let current = crate::raw::request_text_for_processing(&held);
            let out = {
                let ext = ctx.clone();
                let cur = current.clone();
                let hc = crate::extensions::HookCtx {
                    host: held.summary.host.clone(),
                    path: held.summary.path.clone(),
                    flow_id: Some(id),
                    ..Default::default()
                };
                tauri::async_runtime::spawn_blocking(move || {
                    ext.extensions.transform(crate::extensions::Phase::Request, cur.as_bytes(), crate::extensions::Surface::Proxy, &hc)
                })
                    .await
                    .unwrap_or(None)
            };
            if let Some(out) = out {
                let edited = String::from_utf8_lossy(&out).into_owned();
                match apply_edit(&mut parts, &edited, bytes) {
                    Ok(body) => {
                        if req_original.is_none() {
                            req_original = Some(current);
                        }
                        *bytes = body;
                    }
                    Err(e) => ctx.log_info("extension", format!("request transform skipped (invalid): {e:#}")),
                }
            }
        }
    }

    // Both editors have had their turn and `apply_edit` rewrote `parts` in place, so the
    // record is re-read from them. Leaving it as built above would keep the request line
    // and headers as they arrived while something else went upstream, and then the
    // version that actually crossed the wire would exist nowhere: the row would show the
    // arrived message and call it Final.
    flow.set_request(&parts.method, &parts.uri, parts.version, &parts.headers);
    flow.variants.req_original = req_original;
    // The guard's copy is the one filed if this handler dies, so it has to be the same
    // request. Until this point it deliberately was not: nothing had been sent, and the
    // arrived message was the only version that existed.
    abandoned.flow = Some(flow.clone());

    // A body we can replay if the peer resets the h2 stream and we retry over
    // HTTP/1.1: buffered bodies replay verbatim, bodyless requests replay empty.
    // Streamed bodies can't be rewound, so those aren't retried.
    let retry_body: Option<Bytes> = match &payload {
        Payload::Full(bytes) => Some(bytes.clone()),
        Payload::Stream(_) if is_bodyless(&parts.method, &parts.headers) => Some(Bytes::new()),
        Payload::Stream(_) => None,
    };

    let upstream_body = match payload {
        Payload::Full(bytes) => {
            flow.req_body = bytes.clone();
            body::full(bytes)
        }
        Payload::Stream(stream) => body::boxed(Tee::new(stream, req_capture.clone(), None)),
    };

    strip_hop_headers(&mut parts.headers);
    if websocket {
        keep_upgrade(&mut parts.headers);
    }
    // Speak to the target the way the client spoke to us (Upgrade needs HTTP/1.1).
    let proto = if will_prefer_h2 { Proto::PreferHttp2 } else { Proto::Http1 };
    // Going out over h2: the host belongs in `:authority` only. We add a `Host` above
    // for History and the HTTP/1.1 fallback, so take it back off here (hyper re-adds it
    // if the connection ends up HTTP/1.1). The flow was already recorded with it.
    if will_prefer_h2 {
        drop_redundant_host(&mut parts.headers, &parts.uri);
    }
    // Snapshot the request so it can be replayed: an h2 stream reset retries over
    // HTTP/1.1, and a transient connect/TLS-handshake failure retries on a fresh
    // connection. Only replayable bodies (buffered or bodyless) get a snapshot.
    let retry = retry_body.map(|body| (parts.method.clone(), parts.uri.clone(), parts.headers.clone(), body));

    // The request is about to go on the wire, and everything that writes it to History
    // sits after the await below. That await is where the client's patience runs out: an
    // app at its read timeout closes the connection, hyper drops this whole future, and a
    // request that really was sent leaves no trace at all. So the request is also written
    // on its own, from a task that does not belong to the client's connection and is
    // therefore not dropped with it. For a response that is buffered rather than streamed
    // (an extension is active, or a body rule is set) this is the only interim write
    // there is, which is exactly the case where a slow endpoint used to vanish.
    //
    // Spawned here rather than beside the guard above, so the row it writes a second in
    // is the request as it goes upstream, not the one that arrived.
    tokio::spawn({
        let (ctx, recorded, req_capture) = (ctx.clone(), recorded.clone(), req_capture.clone());
        let mut pending = flow.clone();
        async move {
            tokio::time::sleep(PENDING_ROW).await;
            fill_request_body(&mut pending, &req_capture);
            // Deliberately no error. `error` is the column that names a failing party,
            // and a request still out at the target has not identified one; writing a
            // neutral sentence there anyway is what made a healthy request show up red.
            // Empty is the honest value and it keeps the row out of every failure
            // filter, out of an agent's failure heuristic, and out of the red.
            pending.summary.pending = true;
            // Not a round trip, so there is no duration to report, and any number here
            // would be read as one. This row used to claim 1002 ms for requests that
            // went on to take 60 and 100 seconds. The clock the UI should show is the
            // one still running, counted from `ts`.
            pending.summary.duration_ms = 0;
            recorded.write(&ctx, STAGE_REQUEST, pending);
        }
    });

    // The operator's own deadline, if they set one, read once so the response head and
    // the buffered body below are both measured against the same clock rather than
    // getting a fresh full deadline each. 0 (the default) means no deadline at all.
    let give_up = ctx.give_up();
    let give_up_at = give_up.map(|d| tokio::time::Instant::now() + d);
    // From here the target has the request, so an abandoned row stops saying it never
    // left. Set before the await, not after: the await is exactly where this task dies.
    abandoned.sent = true;
    let upstream = ctx.send(Request::from_parts(parts, upstream_body), proto);
    let mut sent = match give_up_at {
        None => upstream.await,
        // A give-up is not a connect error and not an h2 reset, so the retry below
        // leaves it alone: the point was to stop waiting, not to wait again.
        Some(at) => tokio::time::timeout_at(at, upstream).await.unwrap_or_else(|_| Err(gave_up(give_up))),
    };
    if let (Err(e), Some((method, uri, headers, body))) = (&sent, &retry) {
        let rebuild = || {
            let mut r = Request::new(body::full(body.clone()));
            *r.method_mut() = method.clone();
            *r.uri_mut() = uri.clone();
            *r.headers_mut() = headers.clone();
            r
        };
        if is_h2_stream_error(e) {
            // Some servers (or a stale pooled h2 connection) reset the stream with a
            // generic PROTOCOL_ERROR. The request wasn't served, so replay it over
            // HTTP/1.1 rather than surfacing "upstream error" to the client.
            let err_desc = short_error(e); // owned before `sent` is reassigned
            sent = ctx.send(rebuild(), Proto::Http1).await;
            // Remember the host so we skip the doomed h2 attempt next time (until
            // the TTL re-probes it), which also keeps this out of the log.
            if sent.is_ok() {
                ctx.mark_h2_broken(&target_host);
                ctx.log_info("upstream", format!("h2 to {target_host} reset; using HTTP/1.1 for {}m", H2_BROKEN_TTL.as_secs() / 60));
            } else {
                ctx.log_warn("upstream", format!("h2 to {target_host} reset ({err_desc}); HTTP/1.1 retry also failed"));
            }
        } else if is_connect_error(e) {
            // The connection never completed (e.g. "tls handshake eof": the server
            // closed mid-handshake), so nothing was sent - retry once on a fresh
            // connection with the same protocol before surfacing an error.
            let err_desc = short_error(e);
            sent = ctx.send(rebuild(), proto).await;
            if sent.is_ok() {
                ctx.log_info("upstream", format!("{target_host} retried after connect error ({err_desc})"));
            }
        }
    }
    let res = match sent {
        Ok(res) => res,
        Err(e) => {
            let msg = format!("{e:#}");
            ctx.log_warn("upstream", format!("{} {} failed: {msg}", flow.summary.method, flow.summary.host));
            fill_request_body(&mut flow, &req_capture);
            flow.set_error(msg.clone());
            flow.summary.duration_ms = started.elapsed().as_millis() as u64;
            recorded.write(&ctx, STAGE_FINAL, flow);
            return text(StatusCode::BAD_GATEWAY, &format!("{} upstream error: {msg}", brand::NAME));
        }
    };

    let (mut head, res_body) = res.into_parts();
    ctx.rules.apply_headers(Target::ResponseHeader, &mut head.headers);
    flow.set_response_head(head.status, head.version, &head.headers);

    if websocket && head.status == StatusCode::SWITCHING_PROTOCOLS {
        fill_request_body(&mut flow, &req_capture);
        flow.summary.duration_ms = started.elapsed().as_millis() as u64;
        let ws_host = flow.summary.host.clone();
        recorded.write(&ctx, STAGE_FINAL, flow);
        if let (Some(client), Some(server)) = (client_upgrade, head.extensions.remove::<OnUpgrade>()) {
            tokio::spawn(ws::relay(client, server, id, ws_host, ctx.clone()));
        }
        strip_hop_headers(&mut head.headers);
        keep_upgrade(&mut head.headers);
        return Response::from_parts(head, body::empty());
    }

    // Checked again: intercept may have been switched off while the request was out.
    // An explicitly requested response (Forward + response) always holds; a globally
    // held response also has to pass the advanced filter.
    let hold_response =
        ctx.intercept.is_enabled() && in_scope && (wants_response || (ctx.intercept.holds_responses() && passes_filter));
    // Also buffer when an extension is active, so respond() can transform the body.
    let res_body = if hold_response || ext_active || ctx.rules.has(Target::ResponseBody) {
        // Same deadline, still counting from before the request went out: a response
        // whose head arrives in time but whose body never ends is just as stuck.
        let buffered = match give_up_at {
            None => body::buffer(res_body, CAPTURE_LIMIT).await,
            // body::buffer reports a boxed std error, not anyhow's.
            Some(at) => tokio::time::timeout_at(at, body::buffer(res_body, CAPTURE_LIMIT)).await.unwrap_or_else(|_| Err(gave_up(give_up).into())),
        };
        match buffered {
            Ok(body::Buffered::Complete(bytes)) => {
                let bytes = ctx.rules.apply_body(Target::ResponseBody, &mut head.headers, bytes);
                let done = Buffered { head, bytes, flow, started, recorded };
                return done.respond(hold_response, &req_capture, &ctx).await;
            }
            // Too large to edit or rewrite: stream it like any other.
            Ok(body::Buffered::TooLarge(rest)) => body::boxed(rest),
            Err(e) => {
                let msg = format!("{e:#}");
                fill_request_body(&mut flow, &req_capture);
                flow.set_error(format!("response body: {msg}"));
                flow.summary.duration_ms = started.elapsed().as_millis() as u64;
                recorded.write(&ctx, STAGE_FINAL, flow);
                return text(StatusCode::BAD_GATEWAY, &format!("{} upstream error: {msg}", brand::NAME));
            }
        }
    } else {
        body::boxed(res_body)
    };

    strip_hop_headers(&mut head.headers);
    // From here the body is the Tee's to finish, and its own Drop reports a transfer cut
    // short with the part that did arrive. That is strictly better than this guard's
    // request-only row, so the guard stands down rather than racing it.
    abandoned.disarm();
    let body = stream_and_record(res_body, flow, req_capture, started, ctx, recorded);
    Response::from_parts(head, body)
}

/// A response read in full (to be held for the user and/or rewritten by rules).
struct Buffered {
    head: http::response::Parts,
    bytes: Bytes,
    flow: Flow,
    started: Instant,
    recorded: Recorded,
}

impl Buffered {
    /// Optionally show it in the intercept queue first (to forward, edit or
    /// drop), then record it and send it on.
    async fn respond(self, hold: bool, req_capture: &SharedCapture, ctx: &Arc<Ctx>) -> Response<Body> {
        let Buffered { mut head, bytes, mut flow, started, recorded } = self;
        fill_request_body(&mut flow, req_capture);
        flow.set_response(head.status, head.version, &head.headers, bytes.clone());
        // Time on the wire, not time spent waiting for the user.
        flow.summary.duration_ms = started.elapsed().as_millis() as u64;

        // Whether extensions may transform this response (active + host in scope).
        let ext_on = ctx.extensions.active() && ctx.scope.contains(&flow.summary.host);
        let bytes = if !hold {
            bytes
        } else {
            let host = flow.summary.host.clone();
            let path = flow.summary.path.clone();
            // process_* is a real wire change; decode_* makes the body readable for
            // editing without changing the wire. The editor shows the decoded view,
            // keeping the arrived response as Original. A response has no Host header,
            // so the host is passed in ctx for the extension's scope.
            // Every hook on this message names the same flow: process, decode and the
            // encode of an edit are three views of one recorded exchange.
            let fid = flow.summary.id;
            let (processed, display) = if ext_on {
                let processing = crate::raw::response_text_for_processing(&flow);
                let ext = ctx.clone();
                let (h, p) = (host.clone(), path.clone());
                tauri::async_runtime::spawn_blocking(move || {
                    let processed = ext
                        .extensions
                        .transform(
                            crate::extensions::Phase::Response,
                            processing.as_bytes(),
                            crate::extensions::Surface::Interceptor,
                            &crate::extensions::HookCtx { host: h.clone(), path: p.clone(), flow_id: Some(fid), ..Default::default() },
                        )
                        .map(|o| String::from_utf8_lossy(&o).replace("\r\n", "\n"));
                    let base = processed.clone().unwrap_or(processing);
                    let hc = crate::extensions::HookCtx { host: h, path: p, flow_id: Some(fid), ..Default::default() };
                    let display = ext
                        .extensions
                        .decode(crate::extensions::Phase::Response, base.as_bytes(), crate::extensions::Surface::Interceptor, &hc)
                        .map(|o| String::from_utf8_lossy(&o).replace("\r\n", "\n"));
                    (processed, display)
                })
                .await
                .unwrap_or((None, None))
            } else {
                (None, None)
            };
            let shown = display.or(processed.clone());
            let arrived = crate::raw::response_text(&flow);
            let notify = |item: &PendingItem| ctx.events.emit(INTERCEPT_EVENT, item);
            match ctx.intercept.hold(Phase::Response, &flow, shown, notify).await {
                Decision::Forward { raw, .. } => {
                    // An edit is encoded back (e.g. re-encrypted); an unchanged forward
                    // sends the process change if any, else the original bytes.
                    let wire = match raw {
                        Some(user_edit) if ext_on => {
                            let ext = ctx.clone();
                            let (h, p) = (host.clone(), path.clone());
                            Some(
                                tauri::async_runtime::spawn_blocking(move || {
                                    let hc = crate::extensions::HookCtx { host: h, path: p, flow_id: Some(fid), edited: true, ..Default::default() };
                                    ext.extensions
                                        .encode(crate::extensions::Phase::Response, user_edit.as_bytes(), crate::extensions::Surface::Interceptor, &hc)
                                        .map(|o| String::from_utf8_lossy(&o).replace("\r\n", "\n"))
                                        .unwrap_or(user_edit)
                                })
                                .await
                                .unwrap_or_default(),
                            )
                        }
                        Some(user_edit) => Some(user_edit),
                        None => processed,
                    };
                    match wire {
                        None => bytes,
                        Some(edited) => {
                            // Keep the pre-edit response as a History variant (Original vs Final).
                            flow.variants.res_original = Some(arrived);
                            match apply_response_edit(&mut head, &edited, &bytes) {
                                Ok(body) => {
                                    flow.set_response(head.status, head.version, &head.headers, body.clone());
                                    body
                                }
                                Err(e) => {
                                    flow.set_error(format!("edited response invalid: {e:#}"));
                                    // Through the guard, not around it. This row is final, and
                                    // the interim writer is still armed: a write that does not
                                    // claim a stage leaves that writer free to replace a
                                    // truthful row with "no response recorded" a moment later.
                                    recorded.write(ctx, STAGE_FINAL, flow);
                                    return text(StatusCode::BAD_GATEWAY, &format!("{}: edited response invalid: {e:#}", brand::NAME));
                                }
                            }
                        }
                    }
                }
                Decision::Drop => {
                    flow.set_error("response dropped (intercept)".into());
                    // Through the guard: see the note above. Dropping a held response is
                    // the operator's own act and the record has to keep saying so.
                    recorded.write(ctx, STAGE_FINAL, flow);
                    return text(StatusCode::BAD_GATEWAY, &format!("Response dropped by {}", brand::NAME));
                }
            }
        };

        // Extension transform of the response at the Proxy surface: e.g. decrypt an
        // encrypted body so it's readable in History, keeping the original as a variant.
        // Skipped for intercepted responses (already transformed at the Interceptor surface).
        let bytes = if ext_on && !hold {
            // The extension sees the real, decoded, uncapped response payload.
            let current = crate::raw::response_text_for_processing(&flow);
            let out = {
                let ext = ctx.clone();
                let cur = current.clone();
                let hc = crate::extensions::HookCtx {
                    host: flow.summary.host.clone(),
                    path: flow.summary.path.clone(),
                    flow_id: Some(flow.summary.id),
                    ..Default::default()
                };
                tauri::async_runtime::spawn_blocking(move || {
                    ext.extensions.transform(crate::extensions::Phase::Response, cur.as_bytes(), crate::extensions::Surface::Proxy, &hc)
                })
                    .await
                    .unwrap_or(None)
            };
            match out {
                Some(out) => match apply_response_edit(&mut head, &String::from_utf8_lossy(&out).into_owned(), &bytes) {
                    Ok(body) => {
                        if flow.variants.res_original.is_none() {
                            flow.variants.res_original = Some(current);
                        }
                        flow.set_response(head.status, head.version, &head.headers, body.clone());
                        body
                    }
                    Err(e) => {
                        ctx.log_info("extension", format!("response transform skipped (invalid): {e:#}"));
                        bytes
                    }
                },
                None => bytes,
            }
        } else {
            bytes
        };

        recorded.write(ctx, STAGE_FINAL, flow);
        strip_hop_headers(&mut head.headers);
        Response::from_parts(head, body::full(bytes))
    }
}

/// Guards the single History row a flow owns.
///
/// Several writers can produce a version of that row: the request on its own, a preview
/// of a response still streaming, and the finished exchange. They run on different tasks
/// and in no fixed order, so every write says how complete it is and a row is never
/// replaced by a less complete version of itself.
#[derive(Clone)]
struct Recorded(Arc<Mutex<u8>>);

/// Request only: it went upstream and no response for it has been recorded.
const STAGE_REQUEST: u8 = 1;
/// Response head known, body still arriving.
const STAGE_PREVIEW: u8 = 2;
/// The exchange as it ended. Nothing may follow it.
const STAGE_FINAL: u8 = 3;

impl Recorded {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(0)))
    }

    /// Write this version of the row unless one at least as complete is already in.
    /// The lock is held across the queueing, which is a channel send, so two writers
    /// cannot interleave and land out of order.
    fn write(&self, ctx: &Arc<Ctx>, stage: u8, flow: Flow) {
        let mut at = self.0.lock().unwrap();
        if *at >= stage {
            return;
        }
        *at = stage;
        ctx.record_flow(flow);
    }

    /// The stage already recorded. For a cheap look before doing any work.
    fn stage(&self) -> u8 {
        *self.0.lock().unwrap()
    }

    /// The exchange ended with no answer on file. Replaces a request-only row, and
    /// nothing above it: a preview or a finished exchange knows strictly more.
    ///
    /// Unlike `write` this is allowed to replace a row at its own stage, because it is
    /// not a competing version of the row. It is the same row saying something new
    /// about itself, that nothing is waiting for it any more.
    fn abandon(&self, ctx: &Arc<Ctx>, flow: Flow) {
        let mut at = self.0.lock().unwrap();
        if *at > STAGE_REQUEST {
            return;
        }
        *at = STAGE_REQUEST;
        ctx.record_flow(flow);
    }

    /// Hand a stage to another writer without writing anything. A write at that stage is
    /// then skipped, because the writer that claimed it produces a more complete version
    /// of the same row and would only be queued behind a worse one.
    fn supersede(&self, stage: u8) {
        let mut at = self.0.lock().unwrap();
        *at = (*at).max(stage);
    }
}

/// Pass the response body to the client, then record the flow with whatever
/// was captured. Slow streams get an early preview in History.
/// Writes a row off as unanswered if the request handler is dropped before the exchange
/// reaches an outcome of its own.
///
/// The handler future belongs to the client's connection. A client that gives up and
/// hangs up takes that future with it, at whatever await it was parked on, and nothing
/// below that await ever runs again. Without this the interim row written a second in
/// stays `pending` for as long as the window stays open: a badge counting upward for an
/// exchange that ended minutes ago. It is not a rare case either. Chrome's omnibox
/// abandons a suggestion request on every keystroke.
///
/// Rust cannot tell a cancellation from an ordinary return, so arming this means every
/// other way out has to out-rank it. They do, at no cost: each writes `STAGE_FINAL`,
/// which `abandon` refuses to overwrite. The streaming path is the one exception, since
/// it hands the exchange to a `Tee` that outlives this function and carries a `Drop` of
/// its own, so it disarms explicitly.
struct AbandonGuard {
    ctx: Arc<Ctx>,
    recorded: Recorded,
    capture: SharedCapture,
    flow: Option<Flow>,
    /// Whether the request had been handed to the upstream connection when this guard
    /// fired. The two outcomes read as completely different findings: a target that was
    /// asked and whose answer arrived with nobody left holding the question, versus a
    /// request that never left Ngintip at all.
    sent: bool,
}

impl AbandonGuard {
    /// Give up the outcome to a writer that takes over from here and would otherwise
    /// be pre-empted by this one.
    fn disarm(&mut self) {
        self.flow = None;
    }
}

impl Drop for AbandonGuard {
    fn drop(&mut self) {
        // The ordinary response lands here too, on its way out of the function, and it
        // must not pay for a request body copy only to discover it has nothing to do.
        if self.flow.is_none() || self.recorded.stage() > STAGE_REQUEST {
            return;
        }
        let Some(mut flow) = self.flow.take() else { return };
        fill_request_body(&mut flow, &self.capture);
        // `error` stays empty, deliberately. Nothing was observed about the target, and
        // this cannot tell a client hanging up from Ngintip itself being shut down. What
        // it does know is that nobody is waiting any more, and clearing `pending` says
        // exactly that: the row stops claiming to be waiting and starts reading as what
        // it is, a request no answer was ever filed for.
        flow.summary.pending = false;
        flow.summary.unsent = !self.sent;
        self.recorded.abandon(&self.ctx, flow);
    }
}

fn stream_and_record(
    body: Body,
    flow: Flow,
    req_capture: SharedCapture,
    started: Instant,
    ctx: Arc<Ctx>,
    recorded: Recorded,
) -> Body {
    // This response streams, so its interim row comes from the preview below, which
    // knows the status and is therefore strictly more complete than the request on its
    // own. The request-only write is handed over rather than landing just before it.
    recorded.supersede(STAGE_REQUEST);

    tokio::spawn({
        let (ctx, recorded, req_capture) = (ctx.clone(), recorded.clone(), req_capture.clone());
        let mut preview = flow.clone();
        async move {
            tokio::time::sleep(STREAMING_PREVIEW).await;
            fill_request_body(&mut preview, &req_capture);
            preview.summary.streaming = true;
            recorded.write(&ctx, STAGE_PREVIEW, preview);
        }
    });

    let capture = SharedCapture::default();
    let done = {
        let capture = capture.clone();
        move || {
            let mut flow = flow;
            fill_request_body(&mut flow, &req_capture);
            {
                let c = capture.lock().unwrap();
                flow.res_body = c.data.clone().freeze();
                flow.res_truncated = c.truncated();
                flow.summary.length = c.total as usize;
                if let Some(e) = &c.error {
                    flow.set_error(format!("response body: {e}"));
                }
            }
            flow.summary.duration_ms = started.elapsed().as_millis() as u64;
            recorded.write(&ctx, STAGE_FINAL, flow);
        }
    };
    body::boxed(Tee::new(body, capture, Some(Box::new(done))))
}

/// Copy a streamed request body into the flow (no-op for buffered ones).
fn fill_request_body(flow: &mut Flow, capture: &SharedCapture) {
    let c = capture.lock().unwrap();
    if c.total > 0 {
        flow.req_body = c.data.clone().freeze();
        flow.req_truncated = c.truncated();
    }
}

/// Largest number of flows committed in one transaction by the writer thread.
const WRITE_BATCH: usize = 512;

impl Ctx {
    /// Queue a completed flow to be persisted. A single background thread drains
    /// the queue, commits flows in batches (one transaction per burst), and
    /// emits the UI event, so SQLite never blocks the proxy's async tasks and a
    /// flood of traffic does not mean a commit per request.
    pub(crate) fn record_flow(self: &Arc<Self>, flow: Flow) {
        let tx = self.writer.get_or_init(|| {
            // Each flow travels with the store generation it was captured under.
            let (tx, rx) = mpsc::channel::<(u64, Flow)>();
            let ctx = self.clone();
            std::thread::spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut batch = vec![first];
                    while batch.len() < WRITE_BATCH {
                        match rx.try_recv() {
                            Ok(f) => batch.push(f),
                            Err(_) => break,
                        }
                    }
                    // A batch can straddle a project switch, so it is grouped by the
                    // generation each flow was captured under and each group is written
                    // against its own. Nothing is attributed to the wrong project, and
                    // nothing that could still be saved is thrown away.
                    let mut rest = batch;
                    while !rest.is_empty() {
                        let gen = rest[0].0;
                        let keep = rest.iter().take_while(|(g, _)| *g == gen).count();
                        let tail = rest.split_off(keep);
                        let group: Vec<Flow> = rest.into_iter().map(|(_, f)| f).collect();
                        let n = group.len();
                        match ctx.store.push_batch_at(group, Some(gen)) {
                            // Captured under a project that is no longer open. Dropping
                            // is the only safe answer, because the id came from that
                            // project's counter and INSERT OR REPLACE would overwrite a
                            // row here. Dropped capture is evidence lost, so say so.
                            Ok(None) => ctx.log_warn(
                                "store",
                                format!("{n} flow(s) captured in the previous project were not saved; it is no longer open"),
                            ),
                            Ok(Some(summaries)) => {
                                for s in &summaries {
                                    ctx.events.emit(FLOW_EVENT, s);
                                }
                            }
                            Err(e) => ctx.log_error("store", format!("{n} captured flow(s) were NOT saved: {e:#}")),
                        }
                        rest = tail;
                    }
                }
            });
            tx
        });
        if tx.send((flow.gen, flow)).is_err() {
            self.log_error("store", "the flow writer thread is gone; this flow was not saved");
        }
    }
}

/// Replace method/URI/headers with the user's edited raw request; returns the new body.
fn apply_edit(parts: &mut http::request::Parts, edited: &str, original: &Bytes) -> Result<Bytes> {
    let req = raw::parse_request(edited)?;
    parts.method = Method::from_bytes(req.method.as_bytes())?;
    parts.uri = if req.target.starts_with("http://") || req.target.starts_with("https://") {
        req.target.parse()?
    } else {
        let authority = parts
            .uri
            .authority()
            .cloned()
            .ok_or_else(|| anyhow!("missing host"))?;
        Uri::builder()
            .scheme(parts.uri.scheme().cloned().unwrap_or(Scheme::HTTP))
            .authority(authority)
            .path_and_query(req.target.as_str())
            .build()?
    };
    let (headers, body) = edited_message(&parts.headers, original, &req.headers, &req.body)?;
    parts.headers = headers;
    Ok(body)
}

/// Replace status/headers with the user's edited raw response; returns the new body.
fn apply_response_edit(head: &mut http::response::Parts, edited: &str, original: &Bytes) -> Result<Bytes> {
    let res = raw::parse_response(edited)?;
    head.status = StatusCode::from_u16(res.status)?;
    let (headers, body) = edited_message(&head.headers, original, &res.headers, &res.body)?;
    head.headers = headers;
    Ok(body)
}

/// Headers and body for an edited message. Editors show the body decoded (or
/// as a placeholder when binary), so an untouched body keeps its original
/// bytes; a changed one is sent as typed, minus the now-wrong Content-Encoding.
fn edited_message(
    orig_headers: &HeaderMap,
    orig_body: &Bytes,
    edited_headers: &[(String, String)],
    edited_body: &[u8],
) -> Result<(HeaderMap, Bytes)> {
    let orig_list = header_vec(orig_headers);
    let shown = raw::body_text(&orig_list, orig_body).replace("\r\n", "\n");
    let changed = shown.as_bytes() != edited_body;
    let body = if changed {
        raw::restore_crlf(edited_body, Some(orig_body), edited_headers)
    } else {
        orig_body.clone()
    };
    let drop_encoding = changed && orig_headers.contains_key(header::CONTENT_ENCODING);

    let mut headers = HeaderMap::new();
    let mut had_length = false;
    for (k, v) in edited_headers {
        if k.eq_ignore_ascii_case("content-length") {
            had_length = true;
            continue;
        }
        if drop_encoding && k.eq_ignore_ascii_case("content-encoding") {
            continue;
        }
        headers.append(HeaderName::from_bytes(k.as_bytes())?, HeaderValue::from_str(v)?);
    }
    // Recompute so body edits don't desync the length.
    if had_length || !body.is_empty() {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
    }
    Ok((headers, body))
}

fn absolute_uri(uri: &Uri, tunnel: Option<&(Scheme, Authority)>) -> Result<Uri> {
    let Some((scheme, authority)) = tunnel else {
        return Ok(uri.clone());
    };
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Ok(Uri::builder()
        .scheme(scheme.clone())
        .authority(authority.clone())
        .path_and_query(path)
        .build()?)
}

fn is_websocket(headers: &HeaderMap) -> bool {
    let has = |name, token: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    };
    has(header::UPGRADE, "websocket") && has(header::CONNECTION, "upgrade")
}

/// Hop-by-hop, but the protocol switch needs them on both legs.
fn keep_upgrade(headers: &mut HeaderMap) {
    headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
}

const HOP_HEADERS: [&str; 9] = [
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// A request with no body to send upstream, so it can be replayed on a retry: a
/// declared `content-length: 0`, or a bodyless method with no length/encoding.
fn is_bodyless(method: &Method, headers: &HeaderMap) -> bool {
    match headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(n) => n == 0,
        None => {
            !headers.contains_key(header::TRANSFER_ENCODING)
                && matches!(*method, Method::GET | Method::HEAD | Method::DELETE | Method::OPTIONS | Method::TRACE)
        }
    }
}

/// The peer reset the HTTP/2 stream (e.g. a generic PROTOCOL_ERROR) as the
/// request was being sent. Such a request wasn't served, so it's safe to replay
/// over HTTP/1.1.
fn is_h2_stream_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_ascii_lowercase();
    s.contains("http2") || s.contains("sendrequest")
}

/// A connection-establishment failure (TLS handshake / TCP connect) - the request
/// was never sent, so it is safe to retry once on a fresh connection. Covers the
/// common transient "client error (Connect): tls handshake eof".
fn is_connect_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_ascii_lowercase();
    s.contains("(connect)") || s.contains("tls handshake") || s.contains("handshake eof")
}

/// First line of an error chain, trimmed, for a compact log message.
fn short_error(e: &anyhow::Error) -> String {
    format!("{e:#}").lines().next().unwrap_or("").chars().take(160).collect()
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let listed: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    for name in HOP_HEADERS.iter().copied().chain(listed.iter().map(String::as_str)) {
        headers.remove(name);
    }
}

/// The `Host` value for an authority, dropping the scheme's default port (443 for
/// https, 80 for http) the way a browser does, so `site:443` becomes `site`.
/// HTTP/2 carries the host in `:authority`; a `Host` header that merely repeats it is
/// redundant, and a strict server treats the request as malformed (Microsoft's http.sys
/// answers `400 Bad Request`, which is what SharePoint Online does). Drop it ONLY when
/// it matches the authority, so a deliberately spoofed `Host` (Host-header injection
/// testing) still reaches the target. Safe for the HTTP/1.1 fallback too: hyper puts the
/// header back from the URI whenever a connection negotiates HTTP/1.1 (`set_host`).
fn drop_redundant_host(headers: &mut HeaderMap, uri: &Uri) {
    let Some(auth) = uri.authority() else { return };
    // Compare both sides in canonical form, so the scheme's default port (":443" from a
    // CONNECT authority) does not make an otherwise identical host look different.
    let canonical = host_header_value(uri.scheme(), auth);
    let redundant = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|sent| sent.parse::<Authority>().ok())
        .is_some_and(|sent| host_header_value(uri.scheme(), &sent).eq_ignore_ascii_case(&canonical));
    if redundant {
        headers.remove(header::HOST);
    }
}

fn host_header_value(scheme: Option<&Scheme>, auth: &Authority) -> String {
    let default_port = match scheme.map(Scheme::as_str) {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    };
    match auth.port_u16() {
        Some(port) if Some(port) == default_port => auth.host().to_string(),
        _ => auth.as_str().to_string(),
    }
}

/// Concatenate multiple request `Cookie` header fields into a single one joined by
/// "; ". HTTP/2 clients split their cookies across several `cookie` fields for
/// header compression; forwarding those to an HTTP/1.1 origin as separate `Cookie:`
/// lines makes many servers read only the first and drop the rest. A single folded
/// `Cookie` header is valid over HTTP/2 as well (hyper re-splits it if needed).
fn fold_cookies(headers: &mut HeaderMap) {
    // Collect first so the immutable borrow ends before we mutate the map.
    let values: Vec<String> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if values.len() <= 1 {
        return; // 0 or 1 cookie field: nothing to fold
    }
    // Only replace once the joined value is known valid, so a bad join never drops
    // cookies entirely.
    if let Ok(value) = HeaderValue::from_str(&values.join("; ")) {
        headers.remove(header::COOKIE);
        headers.insert(header::COOKIE, value);
    }
}

/// `Basic dXNlcjpwYXNz` -> `user:pass`
fn basic_credentials(value: &str) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let (scheme, encoded) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    String::from_utf8(STANDARD.decode(encoded.trim()).ok()?).ok()
}

/// Comparison time doesn't depend on where the inputs first differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn text(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body::full(msg.to_string()))
        .unwrap()
}

fn landing(uri: &Uri, ctx: &Ctx) -> Response<Body> {
    let download = |body: Vec<u8>, name: &str| {
        Response::builder()
            .header(header::CONTENT_TYPE, "application/x-x509-ca-cert")
            .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
            .body(body::full(body))
            .unwrap()
    };
    match uri.path() {
        "/ca.pem" | "/cert" => return download(ctx.ca.pem().into_bytes(), &format!("{}-ca.pem", brand::ID)),
        // DER: what Android, iOS and Windows expect when installing from a browser.
        "/ca.der" | "/ca.cer" | "/ca.crt" => return download(ctx.ca.der(), &format!("{}-ca.cer", brand::ID)),
        _ => {}
    }
    let name = brand::NAME;
    let html = format!(r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>{name}</title>
<body style="font:15px/1.5 system-ui;max-width:560px;margin:10vh auto;padding:0 16px;color:#222">
<h1>{name} proxy</h1>
<p>Download the root CA and trust it to intercept HTTPS.</p>
<p><a href="/ca.der">Download CA (.cer, for Android / iOS / Windows)</a><br>
<a href="/ca.pem">Download CA (.pem, for macOS / Linux / Firefox)</a></p>
<ul>
<li><b>Android</b>: Settings &rarr; Security &rarr; Encryption &amp; credentials &rarr; Install a certificate &rarr; CA certificate. Apps only trust it if they opt in to user CAs.</li>
<li><b>iOS</b>: open this page in Safari, install the profile (Settings &rarr; Profile Downloaded), then enable it in Settings &rarr; General &rarr; About &rarr; Certificate Trust Settings.</li>
<li><b>Firefox</b>: Settings &rarr; Privacy &amp; Security &rarr; Certificates &rarr; View Certificates &rarr; Authorities &rarr; Import.</li>
</ul></body>"#);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(body::full(Bytes::from(html)))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_headers(uri: &str, sent: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(sent).unwrap());
        drop_redundant_host(&mut h, &uri.parse::<Uri>().unwrap());
        h
    }

    #[test]
    fn h2_drops_a_host_that_only_repeats_the_authority() {
        // Sending both `:authority` and a matching `Host` over h2 is malformed; strict
        // servers (Microsoft http.sys / SharePoint Online) answer 400 Bad Request.
        assert!(!host_headers("https://example.com/x", "example.com").contains_key(header::HOST));
        // A CONNECT tunnel's authority keeps ":443" while the browser-style Host omits
        // it; that is still the same host, so it must go.
        assert!(!host_headers("https://example.com:443/x", "example.com").contains_key(header::HOST));
        // ...and the other way round (authority without the port, Host with it).
        assert!(!host_headers("https://example.com/x", "example.com:443").contains_key(header::HOST));
        // Case and surrounding space do not make it a different host.
        assert!(!host_headers("https://example.com/x", "  Example.COM  ").contains_key(header::HOST));
        // A non-default port is part of the host and must match.
        assert!(!host_headers("https://example.com:8443/x", "example.com:8443").contains_key(header::HOST));
    }

    #[test]
    fn h2_keeps_a_host_that_does_not_match_the_authority() {
        // Host-header injection is a thing we test for, so a deliberately spoofed Host
        // has to survive and travel alongside :authority.
        assert_eq!(host_headers("https://example.com/x", "evil.test").get(header::HOST).unwrap(), "evil.test");
        // A different port is a different origin as far as the target is concerned.
        assert_eq!(host_headers("https://example.com/x", "example.com:8443").get(header::HOST).unwrap(), "example.com:8443");
    }

    #[test]
    fn detects_requests_to_self() {
        let dir = std::env::temp_dir().join(format!("intip-self-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::e2e::test_ctx(&dir);
        let is_self = |uri: &str| ctx.is_self(&uri.parse().unwrap());

        assert!(!is_self("http://127.0.0.1:8080/"), "no listener yet");
        ctx.add_listener("127.0.0.1:8080".parse().unwrap());
        assert!(is_self("http://127.0.0.1:8080/"));
        assert!(is_self("http://localhost:8080/x"));
        assert!(!is_self("http://127.0.0.1:8081/"));
        assert!(!is_self("http://example.com:8080/"));
        assert!(!is_self("http://10.9.8.7:8080/"));

        // A second listener on every interface also makes its port count as self.
        ctx.add_listener("0.0.0.0:9090".parse().unwrap());
        assert!(is_self("http://[::1]:9090/"));
        assert!(is_self("https://127.0.0.1:9090/"));
        assert!(is_self("http://127.0.0.1:8080/"), "first listener still matches");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bodyless_requests_are_replayable() {
        let mut h = HeaderMap::new();
        assert!(is_bodyless(&Method::GET, &h));
        assert!(is_bodyless(&Method::HEAD, &h));
        // A POST with no declared body is not treated as bodyless...
        assert!(!is_bodyless(&Method::POST, &h));
        // ...unless it explicitly declares content-length: 0.
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        assert!(is_bodyless(&Method::POST, &h));
        // A non-zero length is never bodyless, even for GET.
        let mut h2 = HeaderMap::new();
        h2.insert(header::CONTENT_LENGTH, HeaderValue::from_static("12"));
        assert!(!is_bodyless(&Method::GET, &h2));
    }

    #[test]
    fn folds_multiple_cookie_headers_into_one() {
        // An HTTP/2 client sends several separate `cookie` fields.
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_static("PHPSESSID=abc"));
        h.append(header::COOKIE, HeaderValue::from_static("csrf_cookie_name=def"));
        h.append(header::COOKIE, HeaderValue::from_static("ci_session=ghi"));
        fold_cookies(&mut h);
        let all: Vec<_> = h.get_all(header::COOKIE).iter().collect();
        assert_eq!(all.len(), 1, "the cookies must collapse to a single header");
        assert_eq!(all[0], "PHPSESSID=abc; csrf_cookie_name=def; ci_session=ghi");
    }

    #[test]
    fn host_header_drops_default_port_only() {
        let host = |scheme: &str, auth: &str| {
            host_header_value(Some(&scheme.parse::<Scheme>().unwrap()), &auth.parse::<Authority>().unwrap())
        };
        // Default port for the scheme is dropped, matching a browser's Host header.
        assert_eq!(host("https", "site.example:443"), "site.example");
        assert_eq!(host("http", "site.example:80"), "site.example");
        // A non-default port is kept.
        assert_eq!(host("https", "site.example:8443"), "site.example:8443");
        assert_eq!(host("http", "site.example:8080"), "site.example:8080");
        // No port, or a cross-scheme default, is left as-is.
        assert_eq!(host("https", "site.example"), "site.example");
        assert_eq!(host("https", "site.example:80"), "site.example:80");
    }

    #[test]
    fn fold_cookies_leaves_a_single_header_untouched() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; b=2"));
        fold_cookies(&mut h);
        let all: Vec<_> = h.get_all(header::COOKIE).iter().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], "a=1; b=2");
        // No cookie header at all is a no-op too.
        let mut empty = HeaderMap::new();
        fold_cookies(&mut empty);
        assert!(empty.get(header::COOKIE).is_none());
    }

    #[test]
    fn h2_broken_hosts_switch_to_http1() {
        let dir = std::env::temp_dir().join(format!("intip-h2broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = crate::e2e::test_ctx(&dir);
        assert!(!ctx.h2_is_broken("telemetry.example"), "unknown host isn't broken");
        ctx.mark_h2_broken("telemetry.example");
        assert!(ctx.h2_is_broken("telemetry.example"), "marked host is skipped for h2");
        assert!(!ctx.h2_is_broken("other.example"), "only the marked host is affected");
        ctx.mark_h2_broken(""); // an empty host is ignored
        assert!(!ctx.h2_is_broken(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn h2_stream_errors_are_recognized() {
        let e = anyhow!("client error (SendRequest): http2 error: stream error received: unspecific protocol error detected");
        assert!(is_h2_stream_error(&e));
        assert!(short_error(&e).starts_with("client error (SendRequest)"));
        // A plain connection failure is not an h2 stream error (don't retry those blindly).
        assert!(!is_h2_stream_error(&anyhow!("tcp connect error: Connection refused")));
    }

    #[test]
    fn apply_edit_rewrites_request() {
        let (mut parts, _) = Request::builder()
            .method("GET")
            .uri("https://example.com/old")
            .body(())
            .unwrap()
            .into_parts();

        let body = apply_edit(
            &mut parts,
            "POST /new?x=1 HTTP/1.1\nHost: example.com\nContent-Length: 999\nX-Test: yes\n\nhello",
            &Bytes::new(),
        )
        .unwrap();

        assert_eq!(parts.method, Method::POST);
        assert_eq!(parts.uri, "https://example.com/new?x=1");
        assert_eq!(parts.headers["x-test"], "yes");
        assert_eq!(parts.headers["content-length"], "5", "length recomputed");
        assert_eq!(&body[..], b"hello");
    }

    fn gzip(data: &[u8]) -> Bytes {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(data).unwrap();
        Bytes::from(gz.finish().unwrap())
    }

    fn post_parts(headers: &[(&str, &str)]) -> http::request::Parts {
        let mut b = Request::builder().method("POST").uri("https://example.com/api");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(()).unwrap().into_parts().0
    }

    #[test]
    fn edit_keeps_encoded_body_when_only_headers_change() {
        let original = gzip(b"{\"a\":1}");
        let mut parts = post_parts(&[("content-encoding", "gzip"), ("content-type", "application/json")]);
        // The editor showed the body decoded; only a header was added.
        let body = apply_edit(
            &mut parts,
            "POST /api HTTP/1.1\nContent-Encoding: gzip\nContent-Type: application/json\nX-New: 1\n\n{\"a\":1}",
            &original,
        )
        .unwrap();
        assert_eq!(body, original, "original compressed bytes are sent");
        assert_eq!(parts.headers["content-encoding"], "gzip");
        assert_eq!(parts.headers["x-new"], "1");
        assert_eq!(parts.headers["content-length"], original.len().to_string().as_str());
    }

    #[test]
    fn edited_body_drops_stale_content_encoding() {
        let original = gzip(b"{\"a\":1}");
        let mut parts = post_parts(&[("content-encoding", "gzip")]);
        let body = apply_edit(&mut parts, "POST /api HTTP/1.1\nContent-Encoding: gzip\n\n{\"a\":2}", &original).unwrap();
        assert_eq!(&body[..], b"{\"a\":2}");
        assert!(parts.headers.get("content-encoding").is_none(), "plain body, so no gzip header");
    }

    #[test]
    fn binary_placeholder_is_not_sent_as_the_body() {
        let original = Bytes::from_static(b"\x00\x01\x02binary");
        let mut parts = post_parts(&[("content-type", "application/octet-stream")]);
        let shown = crate::raw::body_text(&header_vec(&parts.headers), &original);
        let edited = format!("POST /api HTTP/1.1\nContent-Type: application/octet-stream\nX-Tag: t\n\n{shown}");
        let body = apply_edit(&mut parts, &edited, &original).unwrap();
        assert_eq!(body, original);
    }

    #[test]
    fn edited_multipart_body_keeps_crlf() {
        let original = Bytes::from_static(b"--x\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--x--\r\n");
        let mut parts = post_parts(&[("content-type", "multipart/form-data; boundary=x")]);
        let edited = "POST /api HTTP/1.1\nContent-Type: multipart/form-data; boundary=x\n\n--x\nContent-Disposition: form-data; name=\"a\"\n\n2\n--x--\n";
        let body = apply_edit(&mut parts, edited, &original).unwrap();
        assert_eq!(&body[..], &b"--x\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n2\r\n--x--\r\n"[..]);
    }
}
