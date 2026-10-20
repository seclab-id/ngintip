//! WebSocket relay. After the 101 handshake, frames are relayed between the two
//! sides while a copy is parsed into messages for History. When WebSocket intercept
//! is on for the connection, each data message is instead held until the UI forwards
//! it (optionally edited) or drops it, then re-encoded and sent on.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use bytes::Bytes;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

use crate::{
    proxy::Ctx,
    store::now_ms,
};

/// Event name the UI listens on for each WebSocket message.
pub const WS_EVENT: &str = "ws";

/// Event name the UI listens on for each held (intercepted) WebSocket message.
pub const WS_INTERCEPT_EVENT: &str = "ws-intercept";

/// Payload bytes kept per message; bigger messages keep their start.
pub const WS_CAPTURE_LIMIT: usize = 1024 * 1024;

/// Largest message the interceptor will hold/edit. A frame bigger than this makes the
/// connection fall back to a raw relay (too large to edit meaningfully, and holding it
/// would pin that much memory).
pub const WS_HOLD_CAP: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Client → server.
    Client,
    /// Server → client.
    Server,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Client => "client",
            Direction::Server => "server",
        }
    }
}

/// One complete message (data messages reassembled from fragments).
#[derive(Debug, PartialEq)]
pub struct Parsed {
    pub opcode: u8,
    pub data: Vec<u8>,
    /// Full payload size, even when `data` was cut at the capture limit.
    pub length: u64,
}

struct Frame {
    opcode: u8,
    fin: bool,
    mask: Option<[u8; 4]>,
    remaining: u64,
    /// Payload bytes seen so far, for the mask offset.
    pos: u64,
}

/// Incremental frame parser (RFC 6455 §5.2). Never buffers more than
/// [`WS_CAPTURE_LIMIT`] per message, however large the frames are.
#[derive(Default)]
pub struct Parser {
    head: Vec<u8>,
    frame: Option<Frame>,
    /// Data message being assembled across continuation frames.
    message: Option<Parsed>,
    /// Payload of the current control frame (always ≤ 125 bytes).
    control: Vec<u8>,
}

impl Parser {
    pub fn feed(&mut self, mut bytes: &[u8]) -> Vec<Parsed> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let Some(frame) = self.frame.as_mut() else {
                self.head.push(bytes[0]);
                bytes = &bytes[1..];
                if let Some((frame, _)) = parse_header(&self.head) {
                    self.head.clear();
                    self.start(frame);
                    self.maybe_finish(&mut out);
                }
                continue;
            };

            let take = (frame.remaining.min(bytes.len() as u64)) as usize;
            let (chunk, rest) = bytes.split_at(take);
            bytes = rest;
            let mut chunk = chunk.to_vec();
            if let Some(mask) = frame.mask {
                for (i, b) in chunk.iter_mut().enumerate() {
                    *b ^= mask[((frame.pos + i as u64) % 4) as usize];
                }
            }
            frame.pos += take as u64;
            frame.remaining -= take as u64;

            if is_control(frame.opcode) {
                self.control.extend_from_slice(&chunk);
            } else if let Some(msg) = self.message.as_mut() {
                msg.length += take as u64;
                let room = WS_CAPTURE_LIMIT.saturating_sub(msg.data.len());
                msg.data.extend_from_slice(&chunk[..take.min(room)]);
            }
            self.maybe_finish(&mut out);
        }
        out
    }

    fn start(&mut self, frame: Frame) {
        if is_control(frame.opcode) {
            self.control.clear();
        } else if frame.opcode != 0 || self.message.is_none() {
            // A new data message (or a stray continuation: keep it anyway).
            self.message = Some(Parsed {
                opcode: frame.opcode,
                data: Vec::new(),
                length: 0,
            });
        }
        self.frame = Some(frame);
    }

    fn maybe_finish(&mut self, out: &mut Vec<Parsed>) {
        let Some(frame) = self.frame.as_ref().filter(|f| f.remaining == 0) else {
            return;
        };
        let (opcode, fin) = (frame.opcode, frame.fin);
        self.frame = None;
        if is_control(opcode) {
            let data = std::mem::take(&mut self.control);
            out.push(Parsed {
                opcode,
                length: data.len() as u64,
                data,
            });
        } else if fin {
            out.extend(self.message.take());
        }
    }
}

fn is_control(opcode: u8) -> bool {
    opcode & 0x8 != 0
}

/// Parse a frame header from the front of `head`. Returns the frame plus the header
/// length in bytes (so the caller knows where the payload starts), or None if `head`
/// does not yet hold a complete header.
fn parse_header(head: &[u8]) -> Option<(Frame, usize)> {
    if head.len() < 2 {
        return None;
    }
    let masked = head[1] & 0x80 != 0;
    let ext = match head[1] & 0x7f {
        126 => 2,
        127 => 8,
        _ => 0,
    };
    let need = 2 + ext + if masked { 4 } else { 0 };
    if head.len() < need {
        return None;
    }
    let len = match ext {
        2 => u16::from_be_bytes([head[2], head[3]]) as u64,
        8 => u64::from_be_bytes(head[2..10].try_into().unwrap()),
        _ => (head[1] & 0x7f) as u64,
    };
    let mask = masked.then(|| head[2 + ext..6 + ext].try_into().unwrap());
    Some((
        Frame {
            opcode: head[0] & 0x0f,
            fin: head[0] & 0x80 != 0,
            mask,
            remaining: len,
            pos: 0,
        },
        need,
    ))
}

/// Encode one final (fin=true) WebSocket frame. Frames sent to a server (client->server)
/// must be masked; frames sent to a client must not be.
pub fn encode_frame(opcode: u8, payload: &[u8], mask: bool) -> Vec<u8> {
    let mut out = vec![0x80 | (opcode & 0x0f)];
    let flag = if mask { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(flag | n as u8),
        n if n <= 0xffff => {
            out.push(flag | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(flag | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    if mask {
        let key = mask_key();
        out.extend_from_slice(&key);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
    } else {
        out.extend_from_slice(payload);
    }
    out
}

// A masking key for client->server frames. Masking is a framing requirement (RFC 6455),
// not a security boundary in a MITM proxy, so a per-frame non-crypto key is fine.
fn mask_key() -> [u8; 4] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1).wrapping_add(1);
    n.to_le_bytes()
}

/// One held WebSocket message awaiting a decision from the UI.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsPendingItem {
    /// Unique per held message.
    pub key: u64,
    /// History flow id of the owning WebSocket connection.
    pub flow_id: u64,
    pub ts: u64,
    /// "client" (client->server) or "server" (server->client).
    pub direction: String,
    pub opcode: u8,
    pub host: String,
    /// Decoded payload for display/editing (lossy for binary).
    pub text: String,
    /// The payload is not valid UTF-8 text (editing it as text would corrupt it).
    pub binary: bool,
}

/// What to do with a held WebSocket message.
pub enum WsDecision {
    /// Send it on: `data: None` unchanged, `Some(bytes)` as edited.
    Forward { data: Option<Vec<u8>> },
    /// Drop it (never reaches the peer).
    Drop,
}

struct WsPending {
    item: WsPendingItem,
    tx: oneshot::Sender<WsDecision>,
}

/// Intercept queue for WebSocket data messages (mirrors [`crate::intercept::Intercept`]).
/// Enabling is a snapshot per connection: it applies to WebSocket connections opened
/// while it is on (reconnect to intercept an existing socket).
#[derive(Default)]
pub struct WsIntercept {
    enabled: AtomicBool,
    next_key: AtomicU64,
    queue: Mutex<Vec<WsPending>>,
}

impl WsIntercept {
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Turning it off releases everything still held, unchanged.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if !on {
            for p in self.queue.lock().unwrap().drain(..) {
                let _ = p.tx.send(WsDecision::Forward { data: None });
            }
        }
    }

    pub fn pending(&self) -> Vec<WsPendingItem> {
        self.queue.lock().unwrap().iter().map(|p| p.item.clone()).collect()
    }

    /// Returns false if `key` is no longer held.
    pub fn resolve(&self, key: u64, decision: WsDecision) -> bool {
        let mut queue = self.queue.lock().unwrap();
        let Some(i) = queue.iter().position(|p| p.item.key == key) else {
            return false;
        };
        let _ = queue.remove(i).tx.send(decision);
        true
    }

    /// Park a message until the UI decides. `base` carries everything but the key.
    async fn hold(&self, mut base: WsPendingItem, notify: impl FnOnce(&WsPendingItem)) -> WsDecision {
        let (tx, rx) = oneshot::channel();
        base.key = self.next_key.fetch_add(1, Ordering::Relaxed) + 1;
        let item = base;
        self.queue.lock().unwrap().push(WsPending { item: item.clone(), tx });
        // Intercept may have been switched off between the caller's check and the push.
        if !self.is_enabled() {
            self.resolve(item.key, WsDecision::Forward { data: None });
        } else {
            notify(&item);
        }
        rx.await.unwrap_or(WsDecision::Forward { data: None })
    }
}

/// A single parsed frame with its exact on-wire bytes, for the intercept path.
struct RawFrame {
    opcode: u8,
    fin: bool,
    /// Unmasked payload.
    payload: Vec<u8>,
    /// Exact bytes as received (header + masked payload), for forwarding unchanged.
    raw: Vec<u8>,
    /// The declared frame was larger than [`WS_HOLD_CAP`]; the connection should fall
    /// back to a raw relay rather than buffer it.
    oversized: bool,
}

/// Read one complete frame from `from`, buffering into `buf`. Returns None on EOF.
async fn read_frame(mut from: impl AsyncRead + Unpin, buf: &mut Vec<u8>) -> Option<RawFrame> {
    loop {
        if let Some((frame, hlen)) = parse_header(buf) {
            if frame.remaining as usize > WS_HOLD_CAP {
                return Some(RawFrame { opcode: frame.opcode, fin: frame.fin, payload: Vec::new(), raw: Vec::new(), oversized: true });
            }
            let need = hlen + frame.remaining as usize;
            if buf.len() >= need {
                let raw = buf[..need].to_vec();
                let mut payload = buf[hlen..need].to_vec();
                if let Some(mask) = frame.mask {
                    for (i, b) in payload.iter_mut().enumerate() {
                        *b ^= mask[i % 4];
                    }
                }
                buf.drain(..need);
                return Some(RawFrame { opcode: frame.opcode, fin: frame.fin, payload, raw, oversized: false });
            }
        }
        let mut chunk = [0u8; 16 * 1024];
        match from.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Wait for both sides to switch protocols, then relay until either closes.
pub async fn relay(client: OnUpgrade, server: OnUpgrade, flow_id: u64, host: String, ctx: Arc<Ctx>) {
    let (client, server) = match tokio::try_join!(client, server) {
        Ok(pair) => pair,
        Err(e) => return ctx.log_error("ws", format!("WebSocket upgrade failed: {e}")),
    };

    // One writer per connection keeps messages in the order they were seen.
    let (tx, mut rx) = mpsc::unbounded_channel::<(Direction, Parsed, u64)>();
    let writer = tokio::spawn({
        let ctx = ctx.clone();
        async move {
            while let Some((dir, msg, ts)) = rx.recv().await {
                let store_ctx = ctx.clone();
                let saved = tokio::task::spawn_blocking(move || {
                    store_ctx.store.push_ws(flow_id, ts, dir.as_str(), msg.opcode, msg.length, &msg.data)
                })
                .await;
                match saved {
                    Ok(Ok(m)) => ctx.events.emit(WS_EVENT, &m),
                    Ok(Err(e)) => ctx.log_error("ws", format!("store: {e:#}")),
                    Err(e) => ctx.log_error("ws", format!("store task: {e}")),
                }
            }
        }
    });

    let (cr, cw) = tokio::io::split(TokioIo::new(client));
    let (sr, sw) = tokio::io::split(TokioIo::new(server));
    // Snapshot intercept at connection start: hold frames only when it is on and this
    // host is in scope. Otherwise use the zero-copy raw relay (unchanged fast path).
    if ctx.ws_intercept.is_enabled() && ctx.scope.contains(&host) {
        tokio::join!(
            pump_intercept(cr, sw, Direction::Client, flow_id, host.clone(), ctx.clone(), tx.clone()),
            pump_intercept(sr, cw, Direction::Server, flow_id, host, ctx.clone(), tx),
        );
    } else {
        tokio::join!(
            pump(cr, sw, Direction::Client, tx.clone()),
            pump(sr, cw, Direction::Server, tx),
        );
    }
    let _ = writer.await;
}

/// Intercept variant of [`pump`]: assembles each data message, holds it for a UI
/// decision, then forwards it (as-is or edited) or drops it. Control frames always pass
/// through immediately. A message past [`WS_HOLD_CAP`] falls back to a raw relay.
#[allow(clippy::too_many_arguments)]
async fn pump_intercept(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    dir: Direction,
    flow_id: u64,
    host: String,
    ctx: Arc<Ctx>,
    tx: mpsc::UnboundedSender<(Direction, Parsed, u64)>,
) {
    // Frames we send to a server must be masked; frames we send to a client must not.
    let mask = dir == Direction::Client;
    let mut buf: Vec<u8> = Vec::new();
    // The data message currently being assembled: (opcode, decoded payload, raw frames).
    let mut msg: Option<(u8, Vec<u8>, Vec<u8>)> = None;
    loop {
        let Some(frame) = read_frame(&mut from, &mut buf).await else { break };
        if frame.oversized {
            // Too big to hold: flush what we buffered, then relay the rest raw.
            if to.write_all(&buf).await.is_err() {
                break;
            }
            buf.clear();
            let _ = tokio::io::copy(&mut from, &mut to).await;
            break;
        }
        if is_control(frame.opcode) {
            if to.write_all(&encode_frame(frame.opcode, &frame.payload, mask)).await.is_err() {
                break;
            }
            let _ = tx.send((dir, Parsed { opcode: frame.opcode, length: frame.payload.len() as u64, data: frame.payload }, now_ms()));
            continue;
        }
        match &mut msg {
            None => msg = Some((frame.opcode, frame.payload.clone(), frame.raw.clone())),
            Some((_, dec, raw)) => {
                dec.extend_from_slice(&frame.payload);
                raw.extend_from_slice(&frame.raw);
            }
        }
        if !frame.fin {
            continue;
        }
        let (opcode, decoded, raw) = msg.take().unwrap();
        if raw.len() > WS_HOLD_CAP {
            if to.write_all(&raw).await.is_err() {
                break;
            }
            let _ = tx.send((dir, Parsed { opcode, length: decoded.len() as u64, data: decoded }, now_ms()));
            continue;
        }
        let binary = std::str::from_utf8(&decoded).is_err() || decoded.contains(&0);
        let item = WsPendingItem {
            key: 0,
            flow_id,
            ts: now_ms(),
            direction: dir.as_str().to_string(),
            opcode,
            host: host.clone(),
            text: String::from_utf8_lossy(&decoded).into_owned(),
            binary,
        };
        let decision = ctx.ws_intercept.hold(item, |it| ctx.events.emit(WS_INTERCEPT_EVENT, it)).await;
        match decision {
            WsDecision::Drop => {} // not forwarded, not recorded
            WsDecision::Forward { data } => {
                let out = data.unwrap_or(decoded);
                if to.write_all(&encode_frame(opcode, &out, mask)).await.is_err() {
                    break;
                }
                let _ = tx.send((dir, Parsed { opcode, length: out.len() as u64, data: out }, now_ms()));
            }
        }
    }
    let _ = to.shutdown().await;
}

async fn pump(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    dir: Direction,
    tx: mpsc::UnboundedSender<(Direction, Parsed, u64)>,
) {
    let mut parser = Parser::default();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Forward first; parsing is only for the record.
        if to.write_all(&buf[..n]).await.is_err() {
            break;
        }
        let ts = now_ms();
        for msg in parser.feed(&buf[..n]) {
            let _ = tx.send((dir, msg, ts));
        }
    }
    let _ = to.shutdown().await;
}

/// How a message is shown in the UI.
pub fn display(opcode: u8, data: &Bytes) -> String {
    const MAX: usize = 256 * 1024;
    match opcode {
        0x8 => match data.len() {
            0 => "close".into(),
            1 => format!("close (invalid payload {data:?})"),
            _ => {
                let code = u16::from_be_bytes([data[0], data[1]]);
                let reason = String::from_utf8_lossy(&data[2..]);
                format!("close {code}{}{reason}", if reason.is_empty() { "" } else { " " })
            }
        },
        0x2 if std::str::from_utf8(data).is_err() || data.contains(&0) => hex_dump(&data[..data.len().min(4096)]),
        _ => {
            let text = String::from_utf8_lossy(&data[..data.len().min(MAX)]);
            text.into_owned()
        }
    }
}

fn hex_dump(data: &[u8]) -> String {
    data.chunks(16)
        .enumerate()
        .map(|(i, row)| {
            let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = row
                .iter()
                .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
                .collect();
            format!("{:08x}  {:<47}  {ascii}", i * 16, hex.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(opcode: u8, fin: bool, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
        let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
        let m = if mask.is_some() { 0x80 } else { 0 };
        match payload.len() {
            n if n < 126 => out.push(m | n as u8),
            n if n <= 0xffff => {
                out.push(m | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(m | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        match mask {
            Some(key) => {
                out.extend_from_slice(&key);
                out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
            }
            None => out.extend_from_slice(payload),
        }
        out
    }

    #[test]
    fn parses_masked_fragmented_and_split_input() {
        let mut bytes = frame(0x1, false, b"hel", Some([1, 2, 3, 4]));
        bytes.extend(frame(0x9, true, b"hi", Some([9, 9, 9, 9]))); // ping between fragments
        bytes.extend(frame(0x0, true, b"lo", Some([5, 6, 7, 8])));
        bytes.extend(frame(0x2, true, &[0u8; 300], None));

        // Feed one byte at a time: frame boundaries must not matter.
        let mut p = Parser::default();
        let msgs: Vec<Parsed> = bytes.iter().flat_map(|b| p.feed(std::slice::from_ref(b))).collect();
        assert_eq!(msgs.len(), 3);
        assert_eq!((msgs[0].opcode, &msgs[0].data[..]), (0x9, &b"hi"[..]));
        assert_eq!((msgs[1].opcode, &msgs[1].data[..], msgs[1].length), (0x1, &b"hello"[..], 5));
        assert_eq!((msgs[2].opcode, msgs[2].data.len()), (0x2, 300));
    }

    #[test]
    fn huge_message_is_capped_but_counted() {
        let big = vec![b'x'; WS_CAPTURE_LIMIT + 10];
        let mut p = Parser::default();
        let msgs = p.feed(&frame(0x1, true, &big, Some([1, 1, 1, 1])));
        assert_eq!((msgs[0].data.len(), msgs[0].length), (WS_CAPTURE_LIMIT, big.len() as u64));
    }

    #[test]
    fn displays_close_and_binary() {
        assert_eq!(display(0x8, &Bytes::from_static(b"\x03\xe8bye")), "close 1000 bye");
        assert!(display(0x2, &Bytes::from_static(b"\x00\x01ab")).starts_with("00000000  00 01 61 62"));
    }

    #[test]
    fn encode_frame_round_trips_via_parser() {
        // A re-encoded frame (masked for client->server, unmasked for server->client)
        // must parse back to the same message.
        for mask in [false, true] {
            let enc = encode_frame(0x1, b"hello world", mask);
            let mut p = Parser::default();
            let msgs = p.feed(&enc);
            assert_eq!(msgs.len(), 1);
            assert_eq!((msgs[0].opcode, &msgs[0].data[..]), (0x1, &b"hello world"[..]));
        }
    }

    fn held_key(ic: &WsIntercept) -> u64 {
        ic.pending().pop().map(|i| i.key).unwrap()
    }

    fn item(text: &str) -> WsPendingItem {
        WsPendingItem { key: 0, flow_id: 1, ts: 0, direction: "client".into(), opcode: 0x1, host: "x".into(), text: text.into(), binary: false }
    }

    #[tokio::test]
    async fn ws_intercept_forward_edit_and_drop() {
        let ic = Arc::new(WsIntercept::default());
        ic.set_enabled(true);

        let held = tokio::spawn({ let ic = ic.clone(); async move { ic.hold(item("hi"), |_| {}).await } });
        let key = loop {
            if !ic.pending().is_empty() { break held_key(&ic); }
            tokio::task::yield_now().await;
        };
        assert!(ic.resolve(key, WsDecision::Forward { data: Some(b"edited".to_vec()) }));
        assert!(matches!(held.await.unwrap(), WsDecision::Forward { data: Some(d) } if d == b"edited"));

        let held = tokio::spawn({ let ic = ic.clone(); async move { ic.hold(item("bye"), |_| {}).await } });
        let key = loop {
            if !ic.pending().is_empty() { break held_key(&ic); }
            tokio::task::yield_now().await;
        };
        assert!(ic.resolve(key, WsDecision::Drop));
        assert!(matches!(held.await.unwrap(), WsDecision::Drop));
        assert!(!ic.resolve(key, WsDecision::Drop), "already resolved");
    }

    #[tokio::test]
    async fn ws_intercept_disable_releases_queue() {
        let ic = Arc::new(WsIntercept::default());
        ic.set_enabled(true);
        let held = tokio::spawn({ let ic = ic.clone(); async move { ic.hold(item("m"), |_| {}).await } });
        loop {
            if !ic.pending().is_empty() { break; }
            tokio::task::yield_now().await;
        }
        ic.set_enabled(false);
        assert!(matches!(held.await.unwrap(), WsDecision::Forward { data: None }));
        assert!(ic.pending().is_empty());
    }
}
