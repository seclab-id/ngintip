//! Replaying a WebSocket: open a socket by hand, send frames into it, watch what comes
//! back.
//!
//! The mechanism, and the reason it is this small: **a Repeater socket is an ordinary
//! flow**. Opening one records a 101 flow in the store, and every frame in either
//! direction is pushed as a ws message and emitted on the same event the proxy uses. So
//! History shows it (and the HTTP / WS switch finds it), the message viewer renders it
//! live with no new component, saving it to the Organizer keeps its frames, and the MCP
//! server can read it. None of those had to learn anything about the Repeater.
//!
//! The framing is this crate's own ([`crate::ws::encode_frame`] and [`crate::ws::Parser`]),
//! not a second WebSocket library: the proxy already speaks RFC 6455 both ways, and one
//! definition of a frame is worth more than the convenience of another dependency in a
//! binary whose whole point is being small.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri, Version};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::proxy::Ctx;
use crate::store::Flow;
use crate::ws::{encode_frame, Parser, WS_EVENT};

/// How a frame is described across the API: the opcode, in words.
pub fn opcode_of(kind: &str) -> Result<u8> {
    match kind {
        "text" => Ok(0x1),
        "binary" => Ok(0x2),
        "ping" => Ok(0x9),
        "pong" => Ok(0xa),
        other => bail!("unknown frame kind '{other}' (text, binary, ping, pong)"),
    }
}

/// One open socket: the id the UI holds, the flow it is recorded as, and the channel
/// its frames leave by.
struct Session {
    flow_id: u64,
    out: mpsc::UnboundedSender<Vec<u8>>,
}

/// Every socket the Repeater has open. Lives on `Ctx`, like the other registries.
#[derive(Default)]
pub struct Sockets {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Session>>,
}

/// What `open` tells the UI.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    pub conn_id: u64,
    pub flow_id: u64,
    pub status: u16,
    /// The handshake response as text, for the viewer's Handshake tab.
    pub handshake: String,
}

impl Sockets {
    /// Open `url` ("ws://" or "wss://"), record it as a flow, and start reading.
    pub async fn open(&self, ctx: &Arc<Ctx>, url: &str) -> Result<Opened> {
        let (uri, host, port, tls, path) = parse_url(url)?;

        // Only the key the server has to echo; everything else about the handshake is
        // fixed, because a Repeater socket is ours, not a replay of someone else's.
        let key = STANDARD.encode(rand16());
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: {}/repeater\r\n\r\n",
            if (tls && port == 443) || (!tls && port == 80) { String::new() } else { format!(":{port}") },
            crate::brand::NAME,
        );

        let tcp = TcpStream::connect((host.as_str(), port)).await.with_context(|| format!("connect {host}:{port}"))?;
        let _ = tcp.set_nodelay(true);
        let mut stream: Box<dyn Stream> = if tls {
            let cfg = crate::upstream::insecure_tls_config()?;
            let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
            let name = rustls::pki_types::ServerName::try_from(host.clone())?;
            Box::new(connector.connect(name, tcp).await.context("TLS handshake")?)
        } else {
            Box::new(tcp)
        };

        stream.write_all(request.as_bytes()).await.context("send handshake")?;
        let (head, rest) = read_head(&mut stream).await?;
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        if status != 101 {
            bail!("the server answered {status}, not 101: it did not accept the upgrade");
        }

        // The flow. From here on every other view in the app knows what to do with it.
        //
        // The generation is taken with the id and carried into the reading task. Every
        // project mints ids from 1, so a socket left open across a project swap would
        // otherwise keep writing frames under an id that now belongs to someone else's
        // engagement. The task stops instead.
        let (flow_id, gen) = ctx.store.next_id_at();
        let mut req_headers = HeaderMap::new();
        for line in request.lines().skip(1).filter(|l| !l.is_empty()) {
            if let Some((k, v)) = line.split_once(": ") {
                if let (Ok(name), Ok(value)) = (k.parse::<http::HeaderName>(), v.parse()) {
                    req_headers.insert(name, value);
                }
            }
        }
        let mut flow = Flow::request(flow_id, &Method::GET, &uri, Version::HTTP_11, &req_headers, Bytes::new()).at(gen);
        let mut res_headers = HeaderMap::new();
        for line in head.lines().skip(1).filter(|l| !l.is_empty()) {
            if let Some((k, v)) = line.split_once(": ") {
                if let (Ok(name), Ok(value)) = (k.parse::<http::HeaderName>(), v.parse()) {
                    res_headers.insert(name, value);
                }
            }
        }
        flow.set_response(StatusCode::SWITCHING_PROTOCOLS, Version::HTTP_11, &res_headers, Bytes::new());
        let recorded = ctx.store.push(flow).map_err(|e| anyhow!("{e}"))?;
        ctx.events.emit(crate::proxy::FLOW_EVENT, &recorded);

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let conn_id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.open.lock().unwrap().insert(conn_id, Session { flow_id, out: out_tx });

        // One task owns the socket. Writes arrive on the channel, reads are pumped into
        // the store; when either side ends, the socket is dropped and the id forgotten.
        let ctx2 = ctx.clone();
        let sockets = ctx.ws_repeater.clone();
        tokio::spawn(async move {
            let mut parser = Parser::default();
            let mut buf = vec![0u8; 16 * 1024];
            if !rest.is_empty() {
                record(&ctx2, flow_id, "server", parser.feed(&rest));
            }
            loop {
                // Another project is open: this socket belongs to the last one, and its
                // frames must not land in this one.
                if ctx2.store.generation() != gen {
                    break;
                }
                tokio::select! {
                    frame = out_rx.recv() => {
                        let Some(frame) = frame else { break };
                        if stream.write_all(&frame).await.is_err() {
                            break;
                        }
                    }
                    read = stream.read(&mut buf) => {
                        match read {
                            Ok(0) | Err(_) => break,
                            Ok(n) => record(&ctx2, flow_id, "server", parser.feed(&buf[..n])),
                        }
                    }
                }
            }
            sockets.open.lock().unwrap().remove(&conn_id);
            ctx2.events.emit(EVENT, &serde_json::json!({ "connId": conn_id, "flowId": flow_id, "state": "closed" }));
        });

        Ok(Opened { conn_id, flow_id, status, handshake: head })
    }

    /// Send one frame. Client frames are masked, as RFC 6455 requires.
    pub fn send(&self, ctx: &Arc<Ctx>, conn_id: u64, kind: &str, payload: &[u8]) -> Result<u64> {
        let opcode = opcode_of(kind)?;
        let open = self.open.lock().unwrap();
        let s = open.get(&conn_id).ok_or_else(|| anyhow!("socket {conn_id} is not open"))?;
        let flow_id = s.flow_id;
        s.out.send(encode_frame(opcode, payload, true)).map_err(|_| anyhow!("socket {conn_id} has closed"))?;
        drop(open);
        // Recorded as if the proxy had seen it, because as far as the capture is
        // concerned it did: this is traffic the tool sent.
        record_one(ctx, flow_id, "client", opcode, payload);
        Ok(flow_id)
    }

    /// Close politely: a close frame with a code, then the writer ends.
    pub fn close(&self, ctx: &Arc<Ctx>, conn_id: u64, code: u16, reason: &str) -> Result<()> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        let flow_id = {
            let open = self.open.lock().unwrap();
            let s = open.get(&conn_id).ok_or_else(|| anyhow!("socket {conn_id} is not open"))?;
            let _ = s.out.send(encode_frame(0x8, &payload, true));
            s.flow_id
        };
        record_one(ctx, flow_id, "client", 0x8, &payload);
        self.open.lock().unwrap().remove(&conn_id);
        Ok(())
    }

    /// Open sockets, as (connId, flowId), for a UI that reconnects to its tabs.
    pub fn list(&self) -> Vec<(u64, u64)> {
        self.open.lock().unwrap().iter().map(|(c, s)| (*c, s.flow_id)).collect()
    }

    #[cfg(test)]
    pub fn is_open(&self, conn_id: u64) -> bool {
        self.open.lock().unwrap().contains_key(&conn_id)
    }
}

/// Event the UI listens to for a socket's own lifecycle (frames come on [`WS_EVENT`]).
pub const EVENT: &str = "ws-repeater";

fn record(ctx: &Arc<Ctx>, flow_id: u64, direction: &str, parsed: Vec<crate::ws::Parsed>) {
    for m in parsed {
        record_one(ctx, flow_id, direction, m.opcode, &m.data);
    }
}

fn record_one(ctx: &Arc<Ctx>, flow_id: u64, direction: &str, opcode: u8, data: &[u8]) {
    let ts = crate::store::now_ms();
    if let Ok(msg) = ctx.store.push_ws(flow_id, ts, direction, opcode, data.len() as u64, data) {
        ctx.events.emit(WS_EVENT, &msg);
    }
}

/// `ws://host[:port]/path` -> (uri, host, port, tls, path+query).
fn parse_url(url: &str) -> Result<(Uri, String, u16, bool, String)> {
    let trimmed = url.trim();
    let (tls, rest) = match trimmed.split_once("://") {
        Some(("ws", rest)) => (false, rest),
        Some(("wss", rest)) => (true, rest),
        // http(s) is accepted and read as its socket equivalent: a handshake is an
        // ordinary GET, so this is what people paste out of History.
        Some(("http", rest)) => (false, rest),
        Some(("https", rest)) => (true, rest),
        _ => bail!("a socket address starts with ws:// or wss:// (got '{trimmed}')"),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(if tls { 443 } else { 80 })),
        None => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        bail!("no host in '{trimmed}'");
    }
    let scheme = if tls { "wss" } else { "ws" };
    let uri: Uri = format!("{scheme}://{authority}{path}").parse().context("bad socket address")?;
    Ok((uri, host, port, tls, path.to_string()))
}

/// Read until the end of the response head, returning it plus whatever bytes of the
/// socket arrived in the same read (a server may greet you before you ask anything).
async fn read_head(stream: &mut Box<dyn Stream>) -> Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            bail!("the server closed the connection during the handshake");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find_double_crlf(&buf) {
            let head = String::from_utf8_lossy(&buf[..i]).into_owned();
            return Ok((head, buf[i + 4..].to_vec()));
        }
        if buf.len() > 64 * 1024 {
            bail!("handshake response head is unreasonably large");
        }
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn rand16() -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
        *b = (n >> (i % 4 * 8)) as u8 ^ (i as u8).wrapping_mul(31);
    }
    out
}

/// Either half of "plain or TLS", so the rest of the module does not care which.
pub trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Stream for T {}
