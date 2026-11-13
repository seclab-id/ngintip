//! MCP (Model Context Protocol) server over HTTP, so an LLM can drive Ngintip:
//! read/search history, send to Repeater (recorded), run parallel batches and
//! Intruder attacks, view/trigger/configure the passive scan, organize saved
//! requests in the Organizer, and read the project Vault.
//!
//! Transport: MCP "Streamable HTTP". Clients POST JSON-RPC to the endpoint and
//! get a JSON response; GET <docs_path> returns human/LLM docs. Bound to
//! 127.0.0.1 only: the tools are powerful, so it's local-only by design.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use http::{header, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::brand;
use crate::organizer::{self, NewItem, Op, Organizer, SavedRequest};
use crate::proxy::Ctx;
use crate::raw;
use crate::vault;

/// MCP protocol revisions this server speaks (dates are the spec's revision, not a
/// build date). The first is the default we advertise; per the spec's version
/// negotiation we echo the client's requested revision when it is one of these,
/// otherwise we answer with the default. Our transport is a plain JSON-RPC
/// request/response, which is compatible across these revisions.
const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Pick the protocol revision to answer `initialize` with.
fn negotiate_protocol(params: &Value) -> &'static str {
    params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .and_then(|r| SUPPORTED_PROTOCOLS.iter().copied().find(|&s| s == r))
        .unwrap_or(PROTOCOL_VERSION)
}
const MAX_BATCH: usize = 500;
const MAX_INTRUDER: usize = 5000;
const RESP_PREVIEW: usize = 8 * 1024;

pub async fn serve(listener: TcpListener, ctx: Arc<Ctx>, docs_path: String) {
    let docs_path = Arc::new(if docs_path.is_empty() { "/docs".to_string() } else { docs_path });
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let (ctx, docs_path) = (ctx.clone(), docs_path.clone());
        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let (ctx, docs_path) = (ctx.clone(), docs_path.clone());
                async move { Ok::<_, Infallible>(handle(req, ctx, docs_path).await) }
            });
            let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

fn cors(mut res: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS".parse().unwrap());
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, "content-type, mcp-session-id".parse().unwrap());
    res
}

fn json_res(status: StatusCode, body: Value) -> Response<Full<Bytes>> {
    cors(
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap_or_default())))
            .unwrap(),
    )
}

fn text_res(status: StatusCode, ctype: &str, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    cors(Response::builder().status(status).header(header::CONTENT_TYPE, ctype).body(Full::new(body.into())).unwrap())
}

async fn handle(req: Request<Incoming>, ctx: Arc<Ctx>, docs_path: Arc<String>) -> Response<Full<Bytes>> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if method == Method::OPTIONS {
        return text_res(StatusCode::NO_CONTENT, "text/plain", Bytes::new());
    }
    if method == Method::GET && path == *docs_path {
        return text_res(StatusCode::OK, "text/markdown; charset=utf-8", docs());
    }
    if method == Method::GET && (path == "/" || path == "/mcp") {
        return json_res(
            StatusCode::OK,
            json!({ "name": format!("{}-mcp", brand::ID), "protocol": PROTOCOL_VERSION, "endpoint": "POST /mcp", "docs": *docs_path }),
        );
    }
    if method == Method::POST && (path == "/mcp" || path == "/") {
        let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
        let Ok(value) = serde_json::from_slice::<Value>(&body) else {
            return json_res(StatusCode::OK, rpc_error(Value::Null, -32700, "Parse error"));
        };
        return match handle_rpc(&ctx, value).await {
            Some(response) => json_res(StatusCode::OK, response),
            None => text_res(StatusCode::ACCEPTED, "text/plain", Bytes::new()),
        };
    }
    text_res(StatusCode::NOT_FOUND, "text/plain", Bytes::from_static(b"not found"))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Dispatch one JSON-RPC message or a batch. Returns None for notification-only
/// input (nothing to send back).
async fn handle_rpc(ctx: &Arc<Ctx>, value: Value) -> Option<Value> {
    if let Value::Array(items) = value {
        let mut out = Vec::new();
        for item in items {
            if let Some(resp) = handle_one(ctx, item).await {
                out.push(resp);
            }
        }
        return if out.is_empty() { None } else { Some(Value::Array(out)) };
    }
    handle_one(ctx, value).await
}

async fn handle_one(ctx: &Arc<Ctx>, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // Notifications (no id) get no response.
    let is_notification = id.is_none();

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiate_protocol(&params),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": brand::ID, "version": env!("CARGO_PKG_VERSION") },
        })),
        "notifications/initialized" | "notifications/cancelled" => return None,
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => call_tool_rpc(ctx, &params).await,
        other => Err((-32601, format!("Method not found: {other}"))),
    };

    if is_notification {
        return None;
    }
    let id = id.unwrap_or(Value::Null);
    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err((code, message)) => rpc_error(id, code, &message),
    })
}

/// A tools/call result: wrap tool output as MCP text content (JSON as text).
async fn call_tool_rpc(ctx: &Arc<Ctx>, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match call_tool(ctx, name, &args).await {
        Ok(text) => Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false })),
        Err(e) => Ok(json!({ "content": [{ "type": "text", "text": e }], "isError": true })),
    }
}

fn arg_str(a: &Value, k: &str) -> String {
    a.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
}
fn arg_u64(a: &Value, k: &str, d: u64) -> u64 {
    a.get(k).and_then(|v| v.as_u64()).unwrap_or(d)
}

/// An integer argument, strictly.
///
/// `arg_u64` falls back to its default on anything it cannot read, which is fine for a
/// limit but wrong for a paging cursor: `{"beforeId": "4071"}`, which models emit
/// constantly, would silently mean "start from the newest again" and the caller would
/// re-read page one forever. That is an infinite loop, not a wrong answer. An all-digit
/// string is accepted, because that is a near miss rather than a different intent.
fn arg_int(a: &Value, k: &str, d: u64) -> Result<u64, String> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(d),
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| format!("{k} must be a whole number, zero or more")),
        Some(Value::String(t)) if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
            t.parse().map_err(|_| format!("{k} is too large"))
        }
        Some(v) => Err(format!("{k} must be a whole number, not {v}")),
    }
}

/// A string argument restricted to a fixed set. An unknown value is refused rather than
/// quietly replaced by the default, so the caller is told instead of being misled.
fn arg_enum(a: &Value, k: &str, allowed: &[&str], d: &str) -> Result<String, String> {
    let v = arg_str(a, k);
    if v.trim().is_empty() {
        return Ok(d.to_string());
    }
    let v = v.trim().to_ascii_lowercase();
    if allowed.contains(&v.as_str()) {
        Ok(v)
    } else {
        Err(format!("invalid {k} '{v}'; use one of: {}", allowed.join(", ")))
    }
}

/// Refuse an argument this tool deliberately does not have, naming the one to use
/// instead. A silently ignored parameter is the bug class this whole surface is trying
/// to avoid, and `offset` on a live list is the one a model reaches for by habit.
fn reject_arg(a: &Value, k: &str, instead: &str) -> Result<(), String> {
    if a.get(k).is_some_and(|v| !v.is_null()) {
        return Err(format!("{k} is not how this tool pages; {instead}"));
    }
    Ok(())
}

/// First `max` bytes of `s`, cut on a character boundary, plus whether it was cut.
/// Bytes, not characters, because every other size in this API is bytes.
fn clip(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

async fn call_tool(ctx: &Arc<Ctx>, name: &str, a: &Value) -> Result<String, String> {
    match name {
        "history_list" => history_list(ctx, a),
        "history_get" => history_get(ctx, a),
        "history_search" => history_search(ctx, a),
        "history_set_note" => history_set_note(ctx, a),
        "history_set_color" => history_set_color(ctx, a),
        "history_get_color" => history_get_color(ctx, a),
        "ws_messages" => ws_messages(ctx, a),
        "scope_get" => scope_get(ctx),
        "scope_set" => scope_set(ctx, a),
        "scope_check" => scope_check(ctx, a),
        "repeater_send" => repeater_send(ctx, a).await,
        "repeater_send_batch" => repeater_send_batch(ctx, a).await,
        "repeater_groups" => repeater_groups(ctx),
        "repeater_send_group" => repeater_send_group(ctx, a).await,
        "intruder_run" => intruder_run(ctx, a).await,
        "passive_findings" => passive_findings(ctx, a),
        "passive_rules_get" => passive_rules_get(ctx),
        "passive_rules_set" => passive_rules_set(ctx, a),
        "passive_scan" => passive_scan(ctx),
        "organizer_list" => organizer_list(ctx, a),
        "organizer_get" => organizer_get(ctx, a),
        "organizer_save" => organizer_save(ctx, a),
        "findings_add" => findings_add(ctx, a),
        "organizer_set" => organizer_set(ctx, a),
        "vault_list" => vault_list(ctx, a),
        "vault_get" => vault_get(ctx, a),
        "extensions_status" => ok_json(serde_json::to_value(ctx.extensions.status()).unwrap_or_default()),
        "extensions_enable" => {
            let on = a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            ctx.extensions.set_enabled(on);
            ok_json(serde_json::to_value(ctx.extensions.status()).unwrap_or_default())
        }
        "extension_toggle" => {
            let id = arg_str(a, "id");
            let on = a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            if ctx.extensions.set_extension_enabled(&id, on) {
                ok_json(serde_json::to_value(ctx.extensions.status()).unwrap_or_default())
            } else {
                Err(format!("no extension with id {id}"))
            }
        }
        "extensions_logs" => ok_json(json!({ "logs": ctx.extensions.logs() })),
        "proxy_status" => proxy_status(ctx),
        other => Err(format!("unknown tool: {other}")),
    }
}

/// Tool output. Compact, not pretty-printed: indentation is about 19% of a flow
/// listing and no reader of this API needs it.
fn ok_json(v: Value) -> Result<String, String> {
    Ok(serde_json::to_string(&v).unwrap_or_else(|_| "{}".into()))
}

// ---- history ----------------------------------------------------------------

fn history_list(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let method = arg_str(a, "method").to_ascii_uppercase();
    let host = arg_str(a, "host").to_ascii_lowercase();
    let contains = arg_str(a, "contains").to_ascii_lowercase();
    let status = a.get("status").and_then(|v| v.as_u64()).map(|s| s as u16);
    // Restrict to hosts in the target scope; default off, so nothing is hidden
    // unless asked. An empty scope means everything is in scope.
    let in_scope = a.get("inScope").and_then(|v| v.as_bool()).unwrap_or(false);
    let limit = arg_int(a, "limit", 100)?.clamp(1, 1000) as usize;
    // Paging by index over a live, newest-first capture is wrong by construction: flows
    // arrive at the newest end, so every page boundary shifts and the caller silently
    // re-reads rows it has seen and silently skips rows it has not. Ids are monotonic
    // and are already the sort key, so a keyset cursor is exact. Refuse `offset` out
    // loud rather than ignoring it.
    reject_arg(a, "offset", "pass beforeId, set to the nextBeforeId from your last page; ids stay stable while capture runs, an index does not")?;
    let before = arg_int(a, "beforeId", 0)?;

    let mut all = ctx.store.summaries().map_err(|e| e.to_string())?;
    all.retain(|f| {
        (method.is_empty() || f.method.eq_ignore_ascii_case(&method))
            && (host.is_empty() || f.host.to_ascii_lowercase().contains(&host))
            && (status.is_none() || Some(f.status) == status)
            && (!in_scope || ctx.scope.contains(&f.host))
            && (contains.is_empty()
                || format!("{} {} {}", f.method, f.host, f.path).to_ascii_lowercase().contains(&contains))
    });
    let total = all.len();
    let newest = all.last().map(|f| f.id).unwrap_or(0);
    let (items, has_more, next) = page_flows(&all, before, limit);
    let mut out = json!({
        // Rows matching the filters right now. Recomputed per call, so it drifts while
        // capture runs; `newestId` is how the caller sees that it moved.
        "total": total,
        "newestId": newest,
        "returned": items.len(),
        "hasMore": has_more,
        "flows": items,
    });
    if let Some(n) = next {
        out["nextBeforeId"] = json!(n);
    }
    ok_json(out)
}

/// One page of summaries, newest first, with ids below `before` (0 means start at the
/// newest). Returns the rows, whether more follow, and the cursor for the next page.
///
/// The cursor is only reported when more actually follow: a cursor that is still there
/// once the list is exhausted is an invitation to page forever.
fn page_flows(all: &[crate::store::FlowSummary], before: u64, limit: usize) -> (Vec<Value>, bool, Option<u64>) {
    let mut items: Vec<Value> = Vec::with_capacity(limit.min(256));
    let mut last = None;
    let mut has_more = false;
    for f in all.iter().rev().filter(|f| before == 0 || f.id < before) {
        if items.len() == limit {
            has_more = true;
            break;
        }
        last = Some(f.id);
        items.push(serde_json::to_value(f).unwrap_or(Value::Null));
    }
    (items, has_more, if has_more { last } else { None })
}

/// Which versions of a flow's message `history_get` can return. The wire bytes are
/// canonical, so they are always what `request`/`response` mean under the default view;
/// the other versions are additions, never replacements.
const VIEWS: [&str; 3] = ["auto", "wire", "extension"];

/// Which message a read is about. Naming one side is how a body is paged, and it halves
/// the reply for free when only one side matters.
const SIDES: [&str; 3] = ["both", "request", "response"];

/// Which part of a message to return. `headers` is the cheapest useful read by a wide
/// margin: it answers most header questions for a few hundred bytes.
const PARTS: [&str; 3] = ["all", "headers", "body"];

/// How a body comes back. `text` is the decoded body as lossy UTF-8; `base64` is the
/// bytes themselves, for a body that is not text.
const ENCODINGS: [&str; 2] = ["text", "base64"];

/// Default body bytes per side for a deliberate read.
///
/// Deliberately larger than [`RESP_PREVIEW`], which is a glance inside a send reply that
/// also hands back a `flowId` to fetch the rest with. This is the size of a read someone
/// asked for, and 8 KB cuts most JSON and HTML mid-document.
const BODY_PREVIEW: usize = 32 * 1024;

/// Most body bytes one call will return as text.
const BODY_MAX: usize = 1024 * 1024;

/// Base64 carries a third more characters than the bytes inside it and tokenizes badly,
/// so it gets a tighter ceiling of its own.
const B64_MAX: usize = 8 * 1024;

/// Explains the extra keys to whatever model is reading them, once, only when they are
/// actually present. Without this an agent sees two texts for one message and has no way
/// to know which one the server really sent.
const VERSIONS_NOTE: &str = "`request`/`response` are the bytes on the wire. `requestExtension`/`responseExtension` are the same message after a decode_* extension made it readable (e.g. decrypted) - display only, never sent. `requestOriginal`/`responseOriginal` are what the message was before an intercept edit or an encode_*/process_* extension changed it.";

/// Why a read can be complete and still not be the whole message.
const CAPTURE_NOTE: &str = "Capture stored only the start of this body, so even a complete read of it is less than what crossed the wire.";

/// One side of a flow, rendered: the text, plus the receipt that says what the text is
/// and, more usefully, what it is not.
struct Side {
    text: String,
    receipt: Option<Value>,
}

/// Split a rendered message into its head (through the blank line) and its body. A head
/// ends at the first blank line by definition, which is what parsing one relies on too.
fn split_head_body(text: &str) -> (&str, &str) {
    match text.find("\n\n") {
        Some(i) => text.split_at(i + 2),
        None => (text, ""),
    }
}

/// A byte range of an already-rendered body, snapped to character boundaries. Returns
/// the text, the offset the next page starts at, and whether more follows.
fn str_slice(body: &str, offset: usize, max: usize) -> (String, usize, bool) {
    if offset >= body.len() {
        return (String::new(), body.len(), false);
    }
    let mut start = offset;
    while start < body.len() && !body.is_char_boundary(start) {
        start += 1;
    }
    let (text, cut) = clip(&body[start..], max);
    let next = start + text.len();
    (text, next, cut)
}

/// A flow's request or response as `history_get` returns it, honouring `part`, the byte
/// range and the encoding.
fn wire_side(f: &crate::store::Flow, is_req: bool, part: &str, offset: usize, max: usize, b64: bool) -> Side {
    let headers = if is_req { &f.req_headers } else { &f.res_headers };
    let body: &[u8] = if is_req { &f.req_body } else { &f.res_body };
    let captured_cut = if is_req { f.req_truncated } else { f.res_truncated };

    // A request that never got a response keeps the text it has always had.
    if !is_req && f.res_version.is_empty() {
        return Side {
            text: raw::response_text_full(f),
            receipt: Some(json!({ "noResponse": true, "complete": true })),
        };
    }

    let head = if is_req { raw::request_head(f) } else { raw::response_head(f).unwrap_or_default() };
    let body_bytes = raw::decoded_len(headers, body);

    if part == "headers" {
        let mut r = json!({ "part": "headers", "complete": true, "bodyIncluded": false });
        if !body.is_empty() {
            if let Some(n) = body_bytes {
                r["bodyBytes"] = json!(n);
            }
            r["note"] = json!("Headers only; the body was not read. Ask with part=body to page it.");
        }
        return Side { text: head, receipt: Some(r) };
    }

    // A binary body as lossy UTF-8 is unusable: each invalid byte becomes a three-byte
    // replacement character, so the reply can be larger than the body and carries no
    // information at all. Hand back the head and say how to ask for the bytes.
    if raw::body_is_binary(headers, body) && !b64 {
        let mut r = json!({
            "part": part,
            "binary": true,
            "mime": f.summary.mime,
            "returned": 0,
            "complete": false,
            "bodyIncluded": false,
            "note": format!("Binary body not returned as text, which would be replacement characters. Ask with part=body and encoding=base64 for up to {B64_MAX} bytes of it, or stay with part=headers."),
        });
        if let Some(n) = body_bytes {
            r["bodyBytes"] = json!(n);
        }
        if captured_cut {
            r["capturedTruncated"] = json!(true);
            r["captureNote"] = json!(CAPTURE_NOTE);
        }
        return Side { text: if part == "body" { String::new() } else { head }, receipt: Some(r) };
    }

    if b64 {
        let (bytes, more) = raw::body_range(headers, body, offset, max);
        let n = bytes.len();
        let mut r = json!({
            "part": "body",
            "encoding": "base64",
            "unit": "bytes of the decoded body (Content-Encoding undone)",
            "offset": offset,
            "returned": n,
            "complete": offset == 0 && !more,
            "hasMore": more,
        });
        if more {
            r["nextOffset"] = json!(offset + n);
        }
        if let Some(bb) = body_bytes {
            r["bodyBytes"] = json!(bb);
        }
        if captured_cut {
            r["capturedTruncated"] = json!(true);
            r["captureNote"] = json!(CAPTURE_NOTE);
        }
        return Side { text: STANDARD.encode(&bytes), receipt: Some(r) };
    }

    let sl = raw::body_slice(headers, body, offset, max);
    if part == "all" && offset == 0 && !sl.more {
        // Unchanged from before this tool could slice: the whole message, including the
        // inline truncation and streaming notes the viewer has always appended.
        let text = if is_req { raw::request_text_full(f) } else { raw::response_text_full(f) };
        let receipt = captured_cut
            .then(|| json!({ "part": "all", "complete": true, "capturedTruncated": true, "captureNote": CAPTURE_NOTE }));
        return Side { text, receipt };
    }

    let complete = offset == 0 && !sl.more;
    let mut r = json!({
        "part": part,
        "unit": "bytes of the decoded body (Content-Encoding undone), rendered as lossy UTF-8",
        "offset": sl.start,
        "returned": sl.text.len(),
        "complete": complete,
        "hasMore": sl.more,
    });
    if sl.more {
        r["nextOffset"] = json!(sl.next);
    }
    if let Some(bb) = body_bytes {
        r["bodyBytes"] = json!(bb);
    }
    if captured_cut {
        r["capturedTruncated"] = json!(true);
        r["captureNote"] = json!(CAPTURE_NOTE);
    }
    // Nothing is appended to the text itself: a caller may be diffing or hashing it, and
    // an English note inside the bytes would corrupt that. The receipt carries it all.
    let text = if part == "body" { sl.text } else { format!("{head}{}", sl.text) };
    Side { text, receipt: Some(r) }
}

/// A variant (an extension-decoded or pre-edit message) cut the same way as the wire, so
/// a body cap is not quietly defeated by asking for another version of the same body.
fn variant_side(text: &str, part: &str, offset: usize, max: usize) -> Side {
    let (head, body) = split_head_body(text);
    if part == "headers" {
        return Side { text: head.to_string(), receipt: None };
    }
    let (slice, next, more) = str_slice(body, offset, max);
    let complete = offset == 0 && !more;
    let out = if part == "body" { slice } else { format!("{head}{slice}") };
    if complete {
        return Side { text: out, receipt: None };
    }
    let mut r = json!({ "offset": offset, "returned": next.saturating_sub(offset), "complete": false, "hasMore": more });
    if more {
        r["nextOffset"] = json!(next);
    }
    Side { text: out, receipt: Some(r) }
}

fn history_get(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let id = arg_int(a, "id", 0)?;
    let view = arg_enum(a, "view", &VIEWS, "auto")?;
    let side = arg_enum(a, "side", &SIDES, "both")?;
    let part = arg_enum(a, "part", &PARTS, "all")?;
    let b64 = arg_enum(a, "encoding", &ENCODINGS, "text")? == "base64";
    if b64 && part != "body" {
        return Err("encoding=base64 returns body bytes; pass part=body with it".into());
    }
    let ceiling = if b64 { B64_MAX } else { BODY_MAX };
    let max = arg_int(a, "maxBytes", if b64 { B64_MAX } else { BODY_PREVIEW } as u64)?.clamp(1, ceiling as u64) as usize;
    let offset = arg_int(a, "offset", 0)? as usize;
    if offset > 0 && side == "both" {
        return Err("offset addresses one body; pass side=request or side=response with it".into());
    }
    if offset > 0 && part == "headers" {
        return Err("offset addresses a body; part=headers has none".into());
    }

    let flow = match ctx.store.get(id).map_err(|e| e.to_string())? {
        Some(f) => f,
        // An id a send handed back can be read a moment before its row lands, because
        // recording happens on a background thread. Saying "no flow" there would read as
        // "the proxy did not record my request", which invites sending it again.
        None if id > 0 && id <= ctx.store.peek_id() => {
            return Err(format!(
                "flow {id} has been allocated but is not in the store yet: recording happens on a background thread. Retry once. If it never appears, it was not recorded."
            ))
        }
        None => return Err(format!("no flow {id}")),
    };

    let want_req = side != "response";
    let want_res = side != "request";
    let mut out = json!({
        "summary": serde_json::to_value(&flow.summary).unwrap_or(Value::Null),
        "view": if view == "extension" { "extension" } else { "wire" },
        "side": side,
        "part": part,
    });
    if b64 {
        out["encoding"] = json!("base64");
    }
    // Was only ever English appended to the end of the text before, which a sliced read
    // would have dropped silently.
    if flow.summary.streaming {
        out["streaming"] = json!(true);
        out["streamingNote"] = json!("This response is a provisional preview; a final version will replace it.");
    }
    // WebSocket messages are not part of the handshake, so a flow that carried them
    // looks empty here unless it says so. Omitted entirely when there are none.
    if let Ok(ws) = ctx.store.ws_stats(id) {
        if ws.count > 0 {
            out["wsMessages"] = json!(ws.count);
            out["wsNote"] = json!(format!(
                "This flow carried {} WebSocket messages, which are not part of the handshake above. Call ws_messages with flowId {id} to read them.",
                ws.count
            ));
        }
    }

    // Computing a decoded version runs an extension hook, a synchronous Python round
    // trip, so a headers-only read must not pay for one.
    let variants = if part == "headers" || view == "wire" {
        if part == "headers" && view != "wire" && ctx.extensions.active() {
            out["versionsNote"] = json!("part=headers never includes other versions: producing one runs an extension hook, which is wasted when only headers were asked for. Ask for the body to see them.");
        }
        None
    } else {
        Some(crate::commands::flow_variants(ctx, &flow))
    };

    let add = |out: &mut Value, key: &str, s: Side| {
        out[key] = json!(s.text);
        if let Some(r) = s.receipt {
            out[format!("{key}Slice")] = r;
        }
    };

    // Only count a version that actually DIFFERS from the wire. A hook that changed
    // nothing can still come back as "changed": the extension runner re-serializes with
    // CRLF and `flow_variants` normalises that back, so the bytes differed while the
    // text did not. Rendering the wire text to compare against is not free, so it only
    // happens when there is a variant to compare at all, which usually there is not.
    let (mut req_ext, mut res_ext, mut req_orig, mut res_orig) = (None, None, None, None);
    if let Some(v) = &variants {
        if !v.is_empty() {
            let wire_req = raw::request_text_full(&flow);
            let wire_res = raw::response_text_full(&flow);
            let keep = |alt: &Option<String>, wire: &str| alt.as_deref().filter(|t| *t != wire).map(str::to_string);
            req_ext = keep(&v.req_extension, &wire_req);
            res_ext = keep(&v.res_extension, &wire_res);
            req_orig = keep(&v.req_original, &wire_req);
            res_orig = keep(&v.res_original, &wire_res);
        }
    }

    if view == "extension" {
        // The readable form in the usual place, for a caller that only wants plaintext.
        // Falling back to the wire keeps the shape predictable: there is always a message
        // here, and the flags say whether a decode produced it.
        if want_req {
            add(&mut out, "request", match &req_ext {
                Some(t) => variant_side(t, &part, offset, max),
                None => wire_side(&flow, true, &part, offset, max, b64),
            });
            out["requestDecoded"] = json!(req_ext.is_some());
        }
        if want_res {
            add(&mut out, "response", match &res_ext {
                Some(t) => variant_side(t, &part, offset, max),
                None => wire_side(&flow, false, &part, offset, max, b64),
            });
            out["responseDecoded"] = json!(res_ext.is_some());
        }
        return ok_json(out);
    }

    if want_req {
        add(&mut out, "request", wire_side(&flow, true, &part, offset, max, b64));
    }
    if want_res {
        add(&mut out, "response", wire_side(&flow, false, &part, offset, max, b64));
    }

    {
        // Identical text would double what the caller has to read for nothing, and this
        // mirrors the window, where the switcher appears only when a version differs.
        let candidates = [
            ("requestExtension", req_ext, true),
            ("responseExtension", res_ext, false),
            ("requestOriginal", req_orig, true),
            ("responseOriginal", res_orig, false),
        ];
        let mut versions = serde_json::Map::new();
        for (key, text, is_req) in candidates {
            if (is_req && !want_req) || (!is_req && !want_res) {
                continue;
            }
            if let Some(t) = text {
                let s = variant_side(&t, &part, offset, max);
                versions.insert(key.into(), Value::String(s.text));
                if let Some(r) = s.receipt {
                    versions.insert(format!("{key}Slice"), r);
                }
            }
        }
        if !versions.is_empty() {
            out["versions"] = Value::Object(versions);
            out["versionsNote"] = json!(VERSIONS_NOTE);
        }
    }
    ok_json(out)
}

/// Why a search that found nothing is not evidence that nothing is there. Emitted only
/// on an empty result, which is the case where silence gets read as absence and then
/// written into a report.
const SEARCH_EMPTY_NOTE: &str = "No match is not proof of absence here. The index holds only the first 256 KB of each decoded body, skips binary bodies, and matches whole tokens for queries of 3 characters or more, so a needle can be present and unfound. Confirm with history_get on the specific flow before reporting that something does not occur.";

/// How a match inside a WebSocket message is made actionable. Without this, a search can
/// return a flow whose only match is in a frame, history_get shows the bare handshake,
/// and the caller concludes the search was wrong.
const SEARCH_WS_NOTE: &str = "The flows in `wsMatches` matched inside a WebSocket message, not in the HTTP request or response. Call ws_messages with that flowId and the same text as `contains` to read the matching messages.";

fn history_search(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let query = arg_str(a, "query");
    if query.trim().is_empty() {
        return Err("query is required".into());
    }
    let limit = arg_int(a, "limit", 100)?.clamp(1, 1000) as usize;
    reject_arg(a, "offset", "pass beforeId, set to the nextBeforeId from your last page")?;
    let before = arg_int(a, "beforeId", 0)?;
    let ids: std::collections::HashSet<u64> = ctx.store.search(&query).map_err(|e| e.to_string())?.into_iter().collect();
    // Which of the hits came from a WebSocket message rather than the exchange itself.
    // One extra indexed query for the whole call, not one per row.
    let ws_hits: Vec<u64> = ctx.store.ws_search(&query).map_err(|e| e.to_string())?;
    let mut all = ctx.store.summaries().map_err(|e| e.to_string())?;
    all.retain(|f| ids.contains(&f.id));
    let (items, has_more, next) = page_flows(&all, before, limit);
    let mut out = json!({ "matches": ids.len(), "returned": items.len(), "hasMore": has_more, "flows": items });
    if let Some(n) = next {
        out["nextBeforeId"] = json!(n);
    }
    if !ws_hits.is_empty() {
        out["wsMatches"] = json!(ws_hits);
        out["wsNote"] = json!(SEARCH_WS_NOTE);
    }
    if ids.is_empty() {
        out["note"] = json!(SEARCH_EMPTY_NOTE);
    }
    ok_json(out)
}

/// Highlight colours a flow or saved request can be tagged with (mirrors HIGHLIGHTS in
/// src/lib/colors.js); an empty string clears the highlight.
pub(crate) const HIGHLIGHT_COLORS: [&str; 9] = ["red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink", "gray"];

/// Set (or clear) the note on a flow, preserving its highlight colour. Emits a flow
/// event so open History views update live.
fn history_set_note(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let id = arg_u64(a, "id", 0);
    let note = arg_str(a, "note");
    let note = note.trim();
    let mut flow = ctx.store.get(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {id}"))?;
    ctx.store.set_meta(id, note, &flow.summary.color).map_err(|e| e.to_string())?;
    flow.summary.note = note.to_string();
    ctx.events.emit(crate::proxy::FLOW_EVENT, &flow.summary);
    ok_json(json!({ "id": id, "note": note, "color": flow.summary.color }))
}

/// Set (or clear) the highlight colour on a flow, preserving its note. Emits a flow
/// event so open History views update live.
fn history_set_color(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let id = arg_u64(a, "id", 0);
    let color = arg_str(a, "color");
    let color = color.trim();
    if !color.is_empty() && !HIGHLIGHT_COLORS.contains(&color) {
        return Err(format!("invalid color '{color}'; use one of: {} (or \"\" to clear)", HIGHLIGHT_COLORS.join(", ")));
    }
    let mut flow = ctx.store.get(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {id}"))?;
    ctx.store.set_meta(id, &flow.summary.note, color).map_err(|e| e.to_string())?;
    flow.summary.color = color.to_string();
    ctx.events.emit(crate::proxy::FLOW_EVENT, &flow.summary);
    ok_json(json!({ "id": id, "note": flow.summary.note, "color": color }))
}

/// List the highlight colours that history_set_color accepts. With an optional `id`, also
/// returns that flow's current colour ("" when unhighlighted).
fn history_get_color(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let colors = HIGHLIGHT_COLORS.to_vec();
    let id = arg_u64(a, "id", 0);
    if id != 0 {
        let flow = ctx.store.get(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {id}"))?;
        return ok_json(json!({ "id": id, "color": flow.summary.color, "colors": colors }));
    }
    ok_json(json!({ "colors": colors }))
}

// ---- websocket --------------------------------------------------------------

/// Defaults and ceilings for reading a WebSocket conversation. Every one of these is
/// load bearing: capture keeps up to `ws::WS_CAPTURE_LIMIT` (1 MB) PER MESSAGE, so a
/// socket of a thousand messages can hold a gigabyte, and this tool can be called in a
/// loop.
const WS_LIMIT_DEFAULT: u64 = 50;
const WS_LIMIT_MAX: u64 = 500;
const WS_BYTES_DEFAULT: usize = 2 * 1024;
const WS_BYTES_MAX: usize = 64 * 1024;

/// A hex dump runs to about five characters per byte, so a payload that renders as hex
/// is cut harder. 256 bytes is enough to recognise a magic number, a length prefix or a
/// protobuf tag, which is what a hex dump is actually for here.
const WS_HEX_BYTES: usize = 256;

/// Total payload text one call will return. This overrides `limit` x `maxBytes`, which
/// at their ceilings would otherwise permit a 32 MB reply.
const WS_BUDGET: usize = 64 * 1024;

/// Why the payload in a message row is not simply "the frame".
const WS_NOTE: &str = "`payload` is a rendering, not the bytes: a binary message is shown as a hex dump of its first 256 bytes, and a close message as its code and reason. `length` is what the peer sent, `stored` is what capture kept, `returned` is what this page read. Only trust `payload` as the whole payload when `complete` is true, and cite `length` rather than the size of the text.";

/// The opcode's name, for a reader that should not have to know the numbers.
fn ws_opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x0 => "continuation",
        0x1 => "text",
        0x2 => "binary",
        0x8 => "close",
        0x9 => "ping",
        0xa => "pong",
        _ => "reserved",
    }
}

/// How `ws::display` will render this payload. Mirrors its own condition, so the label
/// cannot disagree with the string it describes.
fn ws_rendered_as(opcode: u8, data: &[u8]) -> &'static str {
    match opcode {
        0x8 => "close",
        0x2 if std::str::from_utf8(data).is_err() || data.contains(&0) => "hex",
        _ => "text",
    }
}

fn ws_messages(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let flow_id = arg_int(a, "flowId", 0)?;
    if flow_id == 0 {
        return Err("flowId is required".into());
    }
    reject_arg(a, "offset", "pass afterId, set to the nextAfterId from your last page; message ids stay stable while the socket is still open, an index does not")?;
    let after = arg_int(a, "afterId", 0)?;
    let dir = arg_enum(a, "direction", &["both", "client", "server"], "both")?;
    let contains = arg_str(a, "contains").trim().to_string();
    let limit = arg_int(a, "limit", WS_LIMIT_DEFAULT)?.clamp(1, WS_LIMIT_MAX) as usize;
    let max = arg_int(a, "maxBytes", WS_BYTES_DEFAULT as u64)?.clamp(64, WS_BYTES_MAX as u64) as usize;

    let stats = ctx.store.ws_stats(flow_id).map_err(|e| e.to_string())?;
    if stats.count == 0 {
        // Only here is the whole flow loaded, to tell "no such flow" (an error, like
        // every other tool) from "that flow carried no messages" (a fact).
        if ctx.store.get(flow_id).map_err(|e| e.to_string())?.is_none() {
            return Err(format!("no flow {flow_id}"));
        }
        return ok_json(json!({
            "flowId": flow_id, "total": 0, "returned": 0, "hasMore": false, "messages": [],
            "note": "That flow carried no WebSocket messages. Only a flow whose response was 101 Switching Protocols has any.",
        }));
    }

    // One row more than asked for, so "more follow" is known rather than guessed.
    let rows = ctx
        .store
        .ws_page(
            flow_id,
            after,
            (dir != "both").then_some(dir.as_str()),
            (!contains.is_empty()).then_some(contains.as_str()),
            limit + 1,
            max,
        )
        .map_err(|e| e.to_string())?;
    let more_rows = rows.len() > limit;

    let mut messages: Vec<Value> = Vec::with_capacity(rows.len().min(limit));
    let (mut spent, mut last, mut budget_hit, mut any_cut) = (0usize, after, false, false);
    for row in rows.into_iter().take(limit) {
        let m = row.msg;
        let rendered_as = ws_rendered_as(m.opcode, &m.data);
        let shown = if rendered_as == "hex" { m.data.slice(..m.data.len().min(WS_HEX_BYTES)) } else { m.data.clone() };
        let payload = crate::ws::display(m.opcode, &shown);
        // The budget wins over `limit`, but never over making progress: a single message
        // larger than the whole budget still comes back, alone.
        if !messages.is_empty() && spent + payload.len() > WS_BUDGET {
            budget_hit = true;
            break;
        }
        spent += payload.len();
        last = m.id;
        let complete = shown.len() as u64 == m.length;
        any_cut |= !complete;
        messages.push(json!({
            "id": m.id,
            "ts": m.ts,
            "direction": m.direction,
            "opcode": m.opcode,
            "type": ws_opcode_name(m.opcode),
            "length": m.length,
            "stored": row.stored,
            "returned": shown.len(),
            "renderedAs": rendered_as,
            "complete": complete,
            "payload": payload,
        }));
    }

    let has_more = budget_hit || more_rows;
    let mut out = json!({
        "flowId": flow_id,
        "total": stats.count,
        "client": stats.client,
        "server": stats.server,
        "totalBytes": stats.bytes,
        "returned": messages.len(),
        "hasMore": has_more,
        "messages": messages,
    });
    if has_more {
        out["nextAfterId"] = json!(last);
    }
    if budget_hit {
        out["budgetReached"] = json!(true);
    }
    if any_cut || budget_hit {
        out["note"] = json!(WS_NOTE);
    }
    if !contains.is_empty() {
        // `ws_fts` indexes only text and binary messages, and only their start, so a
        // filtered page is not the same thing as "these are all the matches".
        out["containsNote"] = json!(
            "`contains` is answered from the message index, which covers only text and binary messages and only the first 256 KB of each. A ping payload, or anything past that, cannot be found this way."
        );
    }
    ok_json(out)
}

// ---- scope ------------------------------------------------------------------

fn scope_get(ctx: &Arc<Ctx>) -> Result<String, String> {
    let patterns = ctx.scope.get();
    ok_json(json!({ "patterns": patterns, "allInScope": patterns.is_empty() }))
}

/// Replace the target scope patterns. Applies immediately (proxy/intercept/passive
/// in-scope checks) and is saved with the project. An empty list means everything.
fn scope_set(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let patterns: Vec<String> = a
        .get("patterns")
        .and_then(|v| v.as_array())
        .ok_or("patterns (array of host strings) is required")?
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    // Taken before the live scope is touched: this both stores the scope AND sets the
    // running proxy's, so applying it to a project the operator has since left would
    // hand the new project the previous engagement's scope, which is what "delete out of
    // scope" then uses to decide what to destroy.
    let gen = ctx.store.generation();
    ctx.scope.set(patterns);
    let normalized = ctx.scope.get(); // trimmed, lowercased, deduped
    let value = serde_json::to_value(&normalized).map_err(|e| e.to_string())?;
    if !ctx.store.put_setting_at(crate::commands::SCOPE_KEY, &value, gen).map_err(|e| e.to_string())? {
        // Put the live scope back to the one belonging to the project that IS open.
        if let Ok(list) = ctx.store.get_list(crate::commands::SCOPE_KEY) {
            ctx.scope.set(list);
        }
        return Err("the project changed while this scope was being applied; nothing was saved".into());
    }
    ctx.events.emit(crate::proxy::SETTING_EVENT, &json!({ "key": crate::commands::SCOPE_KEY }));
    ok_json(json!({ "patterns": normalized, "allInScope": normalized.is_empty() }))
}

fn scope_check(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let host = arg_str(a, "host");
    let host = host.trim();
    if host.is_empty() {
        return Err("host is required".into());
    }
    let patterns = ctx.scope.get();
    let matched: Vec<String> = patterns.iter().filter(|p| crate::scope::host_matches(p, host)).cloned().collect();
    ok_json(json!({ "host": host, "inScope": ctx.scope.contains(host), "matched": matched, "allInScope": patterns.is_empty() }))
}

// ---- repeater ---------------------------------------------------------------

/// Resolve the request text + target: either a raw request (+ target) or by
/// cloning an existing flow id.
fn resolve_request(ctx: &Arc<Ctx>, a: &Value) -> Result<(String, String), String> {
    if let Some(id) = a.get("flowId").and_then(|v| v.as_u64()) {
        let flow = ctx.store.get(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {id}"))?;
        let target = format!("{}://{}", flow.summary.scheme, flow.summary.host);
        return Ok((target, raw::request_text_full(&flow)));
    }
    let raw = arg_str(a, "raw");
    if raw.trim().is_empty() {
        return Err("provide `raw` (+ `target`) or `flowId`".into());
    }
    Ok((arg_str(a, "target"), raw))
}

async fn repeater_send(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let (target, raw) = resolve_request(ctx, a)?;
    let tab = arg_u64(a, "tab", 9001);
    let res = crate::commands::send_recorded(ctx, Some(tab), &target, &raw, true).await?;
    let (preview, cut) = clip(&res.response_raw, RESP_PREVIEW);
    let mut out = json!({
        "tab": tab,
        "status": res.status,
        "length": res.length,
        "durationMs": res.duration_ms,
        "response": preview,
    });
    // The id of the History row this send became, so the caller can read its own
    // exchange back instead of guessing which row was its own.
    if let Some(id) = res.flow_id {
        out["flowId"] = json!(id);
        out["flowTs"] = json!(res.flow_ts);
        // Say the preview was cut AND how to get the rest. A silently cut response is
        // how a caller comes to report a truncated body as the whole body.
        if cut {
            out["responseTruncated"] = json!(true);
            out["responseNote"] = json!(format!(
                "`response` is the first {RESP_PREVIEW} bytes. For the rest: history_get {{\"id\":{id},\"side\":\"response\",\"part\":\"body\"}} and page with offset."
            ));
        }
    } else if cut {
        out["responseTruncated"] = json!(true);
    }
    // A repeater-scope extension may have rewritten the request on its way out (e.g.
    // re-encrypted it) and made the reply readable on its way in. Both versions go back,
    // or the caller cannot tell what it really sent and what really arrived.
    if let Some(sent) = &res.request_final {
        out["requestSent"] = json!(sent);
    }
    if let Some(original) = &res.response_original {
        out["responseWire"] = json!(clip(original, RESP_PREVIEW).0);
    }
    if res.request_final.is_some() || res.response_original.is_some() {
        out["extensionNote"] =
            json!("An extension transformed this exchange. `requestSent` is what left the proxy (vs the request you passed) and `responseWire` is what arrived before the extension made `response` readable.");
    }
    ok_json(out)
}

async fn send_many(ctx: &Arc<Ctx>, tab: u64, target: &str, raw: &str, count: usize, parallel: bool) -> Vec<Value> {
    let one = |i: usize| {
        let (ctx, target, raw) = (ctx.clone(), target.to_string(), raw.to_string());
        async move {
            let started = Instant::now();
            match crate::commands::send_recorded(&ctx, Some(tab), &target, &raw, true).await {
                Ok(r) => json!({ "i": i, "flowId": r.flow_id, "status": r.status, "length": r.length, "durationMs": r.duration_ms }),
                Err(e) => json!({ "i": i, "error": e, "elapsedMs": started.elapsed().as_millis() as u64 }),
            }
        }
    };
    if parallel {
        let mut set = tokio::task::JoinSet::new();
        for i in 0..count {
            set.spawn(one(i));
        }
        let mut out = Vec::with_capacity(count);
        while let Some(r) = set.join_next().await {
            if let Ok(v) = r {
                out.push(v);
            }
        }
        out.sort_by_key(|v| v.get("i").and_then(|x| x.as_u64()).unwrap_or(0));
        out
    } else {
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push(one(i).await);
        }
        out
    }
}

async fn repeater_send_batch(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let (target, raw) = resolve_request(ctx, a)?;
    let count = arg_u64(a, "count", 10).clamp(1, MAX_BATCH as u64) as usize;
    let parallel = arg_str(a, "mode") != "sequential"; // default parallel
    let tab = arg_u64(a, "tab", 9001);
    let results = send_many(ctx, tab, &target, &raw, count, parallel).await;

    let mut by_status: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in &results {
        let key = r.get("status").and_then(|s| s.as_u64()).map(|s| s.to_string()).unwrap_or_else(|| "error".into());
        *by_status.entry(key).or_insert(0) += 1;
    }
    ok_json(json!({ "sent": results.len(), "mode": if parallel {"parallel"} else {"sequential"}, "byStatus": by_status, "results": results }))
}

fn repeater_groups(ctx: &Arc<Ctx>) -> Result<String, String> {
    let saved = ctx.store.get_setting("repeater").map_err(|e| e.to_string())?.unwrap_or(json!({}));
    let tabs = saved.get("tabs").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    let mut groups: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
    for t in &tabs {
        let group = t.get("group").and_then(|g| g.as_str()).unwrap_or("").to_string();
        groups.entry(group).or_default().push(json!({
            "id": t.get("id"), "name": t.get("name"), "target": t.get("target"),
        }));
    }
    let out: Vec<Value> = groups.into_iter().map(|(name, tabs)| json!({ "group": name, "tabs": tabs })).collect();
    ok_json(json!({ "groups": out }))
}

async fn repeater_send_group(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let group = arg_str(a, "group");
    let parallel = arg_str(a, "mode") != "sequential";
    let saved = ctx.store.get_setting("repeater").map_err(|e| e.to_string())?.unwrap_or(json!({}));
    let tabs = saved.get("tabs").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    let members: Vec<&Value> = tabs.iter().filter(|t| t.get("group").and_then(|g| g.as_str()).unwrap_or("") == group).collect();
    if members.is_empty() {
        return Err(format!("no tabs in group {group:?}"));
    }
    let jobs: Vec<(u64, String, String)> = members
        .iter()
        .map(|t| {
            (
                t.get("id").and_then(|i| i.as_u64()).unwrap_or(0),
                t.get("target").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                t.get("raw").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            )
        })
        .collect();

    let run = |(id, target, raw): (u64, String, String)| {
        let ctx = ctx.clone();
        async move {
            match crate::commands::send_recorded(&ctx, Some(id), &target, &raw, true).await {
                Ok(r) => json!({ "tab": id, "flowId": r.flow_id, "status": r.status, "length": r.length, "durationMs": r.duration_ms }),
                Err(e) => json!({ "tab": id, "error": e }),
            }
        }
    };
    let results = if parallel {
        let mut set = tokio::task::JoinSet::new();
        for job in jobs {
            set.spawn(run(job));
        }
        let mut out = Vec::new();
        while let Some(r) = set.join_next().await {
            if let Ok(v) = r {
                out.push(v);
            }
        }
        out
    } else {
        let mut out = Vec::new();
        for job in jobs {
            out.push(run(job).await);
        }
        out
    };
    ok_json(json!({ "group": group, "sent": results.len(), "results": results }))
}

// ---- intruder ---------------------------------------------------------------

const MARK: char = '\u{a7}';

/// Split a template into alternating literal/position segments (even = literal).
fn template_segments(raw: &str) -> Vec<String> {
    raw.split(MARK).map(|s| s.to_string()).collect()
}
fn position_count(segs: &[String]) -> usize {
    segs.len() / 2
}
fn render(segs: &[String], values: &[String]) -> String {
    let mut out = String::new();
    let mut vi = 0;
    for (i, s) in segs.iter().enumerate() {
        if i % 2 == 0 {
            out.push_str(s);
        } else {
            out.push_str(values.get(vi).map(|s| s.as_str()).unwrap_or(""));
            vi += 1;
        }
    }
    out
}

fn payload_sets(a: &Value, k: usize) -> Vec<Vec<String>> {
    let lines = |v: &Value| -> Vec<String> {
        match v {
            Value::String(s) => s.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect(),
            Value::Array(arr) => arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
            _ => vec![],
        }
    };
    let raw = a.get("payloads").cloned().unwrap_or(json!([]));
    // Accept a flat array/string (one set) or an array of sets.
    let sets: Vec<Vec<String>> = match &raw {
        Value::Array(arr) if arr.iter().any(|x| x.is_array() || x.is_string() && arr.len() > 1 && arr.iter().all(|y| y.is_array())) => {
            arr.iter().map(lines).collect()
        }
        Value::Array(arr) if arr.iter().all(|x| x.is_array()) => arr.iter().map(lines).collect(),
        other => vec![lines(other)],
    };
    // Pad to k sets by reusing the first.
    let mut out = sets;
    if out.is_empty() {
        out.push(vec![]);
    }
    while out.len() < k {
        out.push(out[0].clone());
    }
    out
}

fn build_intruder(a: &Value, target: &str, raw: &str) -> Result<Vec<(Vec<String>, String, String)>, String> {
    let segs = template_segments(raw);
    let k = position_count(&segs);
    if k == 0 {
        return Err("no positions: wrap text in § markers".into());
    }
    let attack = arg_str(a, "type");
    let attack = if attack.is_empty() { "sniper".to_string() } else { attack };
    let sets = payload_sets(a, k);
    let defaults: Vec<String> = (0..k).map(|i| segs[i * 2 + 1].clone()).collect();

    let mut reqs: Vec<(Vec<String>, String)> = Vec::new();
    match attack.as_str() {
        "sniper" => {
            let set = &sets[0];
            for pos in 0..k {
                for p in set {
                    let mut vals = defaults.clone();
                    vals[pos] = p.clone();
                    reqs.push((vec![p.clone()], render(&segs, &vals)));
                }
            }
        }
        "ram" | "battering" | "batteringram" => {
            for p in &sets[0] {
                let vals = vec![p.clone(); k];
                reqs.push((vec![p.clone()], render(&segs, &vals)));
            }
        }
        "pitchfork" => {
            let n = sets.iter().map(|s| s.len()).min().unwrap_or(0);
            for i in 0..n {
                let vals: Vec<String> = (0..k).map(|j| sets[j][i].clone()).collect();
                reqs.push((vals.clone(), render(&segs, &vals)));
            }
        }
        "cluster" | "clusterbomb" => {
            let mut acc: Vec<Vec<String>> = vec![vec![]];
            for set in sets.iter().take(k) {
                let mut next = Vec::new();
                for combo in &acc {
                    for p in set {
                        let mut c = combo.clone();
                        c.push(p.clone());
                        next.push(c);
                    }
                }
                acc = next;
            }
            for vals in acc {
                reqs.push((vals.clone(), render(&segs, &vals)));
            }
        }
        other => return Err(format!("unknown attack type {other:?}")),
    }
    if reqs.len() > MAX_INTRUDER {
        return Err(format!("{} requests exceeds the {MAX_INTRUDER} cap", reqs.len()));
    }
    Ok(reqs.into_iter().map(|(p, r)| (p, target.to_string(), r)).collect())
}

async fn intruder_run(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let (target, raw) = resolve_request(ctx, a)?;
    let reqs = build_intruder(a, &target, &raw)?;
    let concurrency = arg_u64(a, "concurrency", 10).clamp(1, 50) as usize;
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));

    let mut set = tokio::task::JoinSet::new();
    for (n, (payloads, target, raw)) in reqs.into_iter().enumerate() {
        let (ctx, sem) = (ctx.clone(), sem.clone());
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            let started = Instant::now();
            match crate::commands::send_recorded(&ctx, None, &target, &raw, true).await {
                Ok(r) => json!({ "n": n, "payloads": payloads, "flowId": r.flow_id, "status": r.status, "length": r.length, "timeMs": r.duration_ms }),
                Err(e) => json!({ "n": n, "payloads": payloads, "error": e, "timeMs": started.elapsed().as_millis() as u64 }),
            }
        });
    }
    let mut rows = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(v) = r {
            rows.push(v);
        }
    }
    rows.sort_by_key(|v| v.get("n").and_then(|x| x.as_u64()).unwrap_or(0));
    let mut by_status: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in &rows {
        let key = r.get("status").and_then(|s| s.as_u64()).map(|s| s.to_string()).unwrap_or_else(|| "error".into());
        *by_status.entry(key).or_insert(0) += 1;
    }
    ok_json(json!({ "sent": rows.len(), "byStatus": by_status, "results": rows }))
}

// ---- passive ----------------------------------------------------------------

fn passive_findings(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let sev = arg_str(a, "severity");
    let findings = ctx.store.get_setting("passiveFindings").map_err(|e| e.to_string())?.unwrap_or(json!([]));
    let arr = findings.as_array().cloned().unwrap_or_default();
    let filtered: Vec<Value> = arr
        .into_iter()
        .filter(|f| sev.is_empty() || f.get("severity").and_then(|s| s.as_str()) == Some(sev.as_str()))
        .collect();
    // Findings are a frozen array rebuilt by passive_scan, with no per-finding id, so a
    // keyset cursor is impossible here and an index is stable: nothing is inserted into
    // the middle of this list between two calls. Hence `offset`, unlike the flow lists.
    let offset = arg_int(a, "offset", 0)? as usize;
    let limit = arg_int(a, "limit", 200)?.clamp(1, 2000) as usize;
    let page: Vec<Value> = filtered.iter().skip(offset).take(limit).cloned().collect();
    let has_more = offset + page.len() < filtered.len();
    let mut out = json!({
        "count": filtered.len(),
        "offset": offset,
        "returned": page.len(),
        "hasMore": has_more,
        "findings": page,
    });
    if has_more {
        out["nextOffset"] = json!(offset + page.len());
    }
    ok_json(out)
}

fn passive_rules_get(ctx: &Arc<Ctx>) -> Result<String, String> {
    let custom = ctx.store.get_setting("passiveRules").map_err(|e| e.to_string())?.unwrap_or(json!([]));
    let disabled = ctx.store.get_setting("passiveDisabled").map_err(|e| e.to_string())?.unwrap_or(json!([]));
    let builtin: Vec<Value> = BUILTIN_RULES
        .iter()
        .map(|r| json!({ "id": r.0, "title": r.1, "severity": r.2, "pattern": r.3 }))
        .collect();
    ok_json(json!({ "builtin": builtin, "custom": custom, "disabled": disabled }))
}

fn passive_rules_set(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let rules = a.get("rules").cloned().ok_or("provide `rules` (array)")?;
    if !rules.is_array() {
        return Err("`rules` must be an array".into());
    }
    let gen = ctx.store.generation();
    if !ctx.store.put_setting_at("passiveRules", &rules, gen).map_err(|e| e.to_string())? {
        return Err("the project changed; the rules were not saved".into());
    }
    ctx.events.emit(crate::proxy::SETTING_EVENT, &json!({ "key": "passiveRules" }));
    ok_json(json!({ "ok": true, "count": rules.as_array().map(|a| a.len()).unwrap_or(0) }))
}

// Built-in passive rules (mirror src/lib/passive-rules.js). (id, title, severity,
// pattern, case_insensitive, group). Rust's regex crate has no lookaround, so
// `(?i)` is embedded in the pattern and the `email` rule is matched broadly and
// then refined by `plausible_email` (emulating the JS lookbehind/lookahead).
const BUILTIN_RULES: &[(&str, &str, &str, &str, bool, usize)] = &[
    ("jwt", "JSON Web Token", "medium", r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}", false, 0),
    ("aws-key", "AWS access key ID", "high", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", false, 0),
    ("google-key", "Google API key", "high", r"\bAIza[0-9A-Za-z_-]{35}\b", false, 0),
    ("slack-token", "Slack token", "high", r"\bxox[baprs]-[0-9A-Za-z-]{10,}", false, 0),
    ("github-token", "GitHub token", "high", r"\bgh[posru]_[0-9A-Za-z]{36}\b", false, 0),
    ("stripe-key", "Stripe secret key", "high", r"\b(?:sk|rk)_live_[0-9A-Za-z]{10,}\b", false, 0),
    ("private-key", "Private key block", "high", r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----", false, 0),
    ("secret-assign", "Hardcoded secret", "medium", r#"(?i)\b(?:api[_-]?key|secret|passwd|password|access[_-]?token|client[_-]?secret|authorization)\b["']?\s*[:=]\s*["']([^"'\s]{6,})["']"#, true, 1),
    ("authorization-basic", "HTTP Basic credentials", "high", r"(?i)Authorization:\s*Basic\s+[A-Za-z0-9+/=]{8,}", true, 0),
    ("bearer-token", "Bearer token", "medium", r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9._~+/-]{10,}", true, 0),
    ("creds-in-url", "Credentials in URL", "high", r"(?i)\b[a-z][a-z0-9+.-]*://[^/\s:@]+:[^/\s:@]+@", true, 0),
    ("password-field", "Password in query/body", "medium", r#"(?i)(?:password|passwd|pwd)=[^&\s"']{3,}"#, true, 0),
    ("openai-key", "OpenAI API key", "high", r"\bsk-[A-Za-z0-9]{20,}", false, 0),
    ("google-oauth", "Google OAuth access token", "high", r"\bya29\.[A-Za-z0-9_-]{20,}", false, 0),
    ("slack-webhook", "Slack webhook URL", "high", r"https://hooks\.slack\.com/services/[A-Za-z0-9/]+", false, 0),
    ("sendgrid-key", "SendGrid API key", "high", r"\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}", false, 0),
    ("twilio-sid", "Twilio account SID", "medium", r"\bAC[0-9a-fA-F]{32}\b", false, 0),
    ("npm-token", "npm token", "high", r"\bnpm_[A-Za-z0-9]{36}\b", false, 0),
    ("vite-exposed-secret", "Bundler-exposed secret (VITE_/NEXT_PUBLIC_/…)", "high", r#"\b(?:VITE|NEXT_PUBLIC|REACT_APP|VUE_APP|EXPO_PUBLIC|NG_APP|PUBLIC)_[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASS(?:WORD)?|API|AUTH|PRIVATE|CRED(?:ENTIAL)?)[A-Z0-9_]*["']?\s*[:=]\s*["']([^"'\s]{6,})["']"#, false, 1),
    ("azure-storage-key", "Azure Storage account key", "high", r"(?i)AccountKey=[A-Za-z0-9+/]{86}==", true, 0),
    ("azure-storage-conn", "Azure Storage connection string", "high", r"(?i)DefaultEndpointsProtocol=https?;AccountName=[a-z0-9]+;AccountKey=[A-Za-z0-9+/=]{80,}", true, 0),
    ("azure-ad-secret", "Azure AD client secret", "high", r"\b[A-Za-z0-9._~-]{2,5}Q~[A-Za-z0-9._~-]{28,36}\b", false, 0),
    ("gcp-service-account", "GCP service-account key file", "high", r#""type"\s*:\s*"service_account""#, false, 0),
    ("google-oauth-client-secret", "Google OAuth client secret", "high", r"\bGOCSPX-[A-Za-z0-9_-]{28}\b", false, 0),
    ("github-fine-grained-pat", "GitHub fine-grained PAT", "high", r"\bgithub_pat_[0-9A-Za-z_]{82}\b", false, 0),
    ("gitlab-pat", "GitLab personal access token", "high", r"\bglpat-[0-9A-Za-z_-]{20}\b", false, 0),
    ("shopify-token", "Shopify access token", "high", r"\bshp(?:at|ss|ca|pa)_[a-fA-F0-9]{32}\b", false, 0),
    ("digitalocean-token", "DigitalOcean access token", "high", r"\bdop_v1_[a-f0-9]{64}\b", false, 0),
    ("firebase-fcm-key", "Firebase Cloud Messaging server key", "high", r"\bAAAA[A-Za-z0-9_-]{7}:[A-Za-z0-9_-]{140}\b", false, 0),
    ("twilio-api-key", "Twilio API key", "medium", r"\bSK[0-9a-fA-F]{32}\b", false, 0),
    ("mailchimp-key", "Mailchimp API key", "high", r"\b[0-9a-f]{32}-us[0-9]{1,2}\b", false, 0),
    ("slack-app-token", "Slack app-level token", "high", r"\bxapp-\d-[A-Z0-9]+-\d+-[a-f0-9]+\b", false, 0),
    ("telegram-bot-token", "Telegram bot token", "high", r"\b\d{8,10}:AA[A-Za-z0-9_-]{33}\b", false, 0),
    ("discord-bot-token", "Discord bot token", "high", r"\b[MNO][A-Za-z0-9_-]{23}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27}\b", false, 0),
    ("square-access-token", "Square access token", "high", r"\bsq0atp-[0-9A-Za-z_-]{22}\b", false, 0),
    ("square-oauth-secret", "Square OAuth secret", "high", r"\bsq0csp-[0-9A-Za-z_-]{43}\b", false, 0),
    ("pypi-token", "PyPI upload token", "high", r"\bpypi-AgEIcHlwaS5vcmc[A-Za-z0-9_-]{50,}", false, 0),
    ("postman-key", "Postman API key", "high", r"\bPMAK-[0-9a-fA-F]{24}-[0-9a-fA-F]{34}\b", false, 0),
    ("linear-key", "Linear API key", "high", r"\blin_api_[0-9A-Za-z]{40}\b", false, 0),
    ("databricks-token", "Databricks personal access token", "high", r"\bdapi[0-9a-f]{32}\b", false, 0),
    ("figma-token", "Figma personal access token", "high", r"\bfigd_[0-9A-Za-z_-]{40,}", false, 0),
    ("dynatrace-token", "Dynatrace API token", "high", r"\bdt0[a-z]0[0-9]\.[A-Z0-9]{24}\.[A-Z0-9]{64}\b", false, 0),
    ("vault-token", "HashiCorp Vault token", "high", r"\bhvs\.[A-Za-z0-9_-]{24,}", false, 0),
    ("cloudinary-url", "Cloudinary credentials URL", "high", r"\bcloudinary://\d+:[A-Za-z0-9_-]+@[a-z0-9-]+", false, 0),
    ("age-secret-key", "age secret key", "high", r"\bAGE-SECRET-KEY-1[0-9A-Z]{58}\b", false, 0),
    ("pem-cert", "Certificate block", "low", r"-----BEGIN CERTIFICATE-----", false, 0),
    ("private-ip", "Private IP address", "info", r"\b(?:10\.(?:\d{1,3}\.){2}\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3})\b", false, 0),
    ("email", "Email address", "info", r"(?i)\b[a-z0-9]+(?:[._%+-][a-z0-9]+)*@[a-z][a-z0-9-]*(?:\.[a-z0-9-]+)*\.[a-z]{2,24}\b", true, 0),
];

/// File/asset/code extensions that look like a TLD but aren't (mirror of
/// EMAIL_TLD_DENYLIST in src/lib/passive-rules.js).
const EMAIL_TLD_DENYLIST: &[&str] = &[
    "js", "jsx", "ts", "tsx", "mjs", "cjs", "css", "scss", "sass", "less", "map", "json", "html",
    "htm", "xml", "vue", "svelte", "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "woff",
    "ttf", "eot", "otf", "wasm", "lock", "min", "txt", "yaml", "yml", "toml", "ini", "cfg", "conf",
    "class", "jar", "war", "dll", "exe", "node", "bin", "php", "aspx", "jsp", "go", "rb", "java",
    "cpp", "hpp",
];

/// Emulates the JS `email` rule's lookbehind/lookahead (unavailable in Rust's
/// regex crate): reject a match preceded by a word/URL char (userinfo, longer
/// token) or ending in a code/asset extension that only looks like a TLD.
fn plausible_email(text: &str, start: usize, m: &str) -> bool {
    if let Some(prev) = text[..start].chars().next_back() {
        if prev.is_alphanumeric() || matches!(prev, '_' | '.' | '/' | '@' | '-') {
            return false;
        }
    }
    let tld = m.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    !EMAIL_TLD_DENYLIST.contains(&tld.as_str())
}

/// A tech-disclosure header value that reveals a version (nginx/1.25.3, PHP/8.2,
/// 4.0.30319) rather than just a product name (cloudflare, Varnish).
fn has_version(v: &str) -> bool {
    let b = v.as_bytes();
    for i in 0..b.len() {
        // `product/1` — a slash then (optional spaces then) a digit.
        if b[i] == b'/' {
            let mut j = i + 1;
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                return true;
            }
        }
        // A dotted version like `1.25` or `4.0`.
        if b[i] == b'.' && i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            return true;
        }
    }
    false
}

fn trunc(s: &str) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 90 {
        format!("{}…", t.chars().take(90).collect::<String>())
    } else {
        t
    }
}

/// A content-type essence (as the store's short mime keeps it) → canonical ext.
/// Mirrors MIME_EXT in src/lib/passive.js.
fn mime_to_ext(mime: &str) -> String {
    let mut m = mime.to_ascii_lowercase();
    if let Some(sc) = m.find(';') {
        m.truncate(sc);
    }
    let m = m.trim();
    let m = m.rsplit('/').next().unwrap_or(m);
    let m = m.strip_prefix("x-").unwrap_or(m);
    match m {
        "javascript" | "ecmascript" => "js".to_string(),
        "typescript" => "ts".to_string(),
        "htm" => "html".to_string(),
        "svg+xml" => "svg".to_string(),
        "plain" => "txt".to_string(),
        other => other.to_string(),
    }
}

/// A flow's path extension, if any (before the query, in the last path segment).
fn path_ext(path: &str) -> Option<String> {
    let p = path.split(['?', '#']).next().unwrap_or(path);
    let seg = p.rsplit('/').next().unwrap_or(p);
    let dot = seg.rfind('.')?;
    if dot > 0 && dot < seg.len() - 1 {
        Some(seg[dot + 1..].to_ascii_lowercase())
    } else {
        None
    }
}

/// Whether a flow is in the passive-scan scope (mirror of flowInScope in
/// src/lib/passive.js). `None` (setting absent/empty) or `*` means scan all.
fn flow_in_scope(path: &str, mime: &str, scope: &Option<std::collections::HashSet<String>>) -> bool {
    let Some(set) = scope else { return true };
    if set.contains("*") {
        return true;
    }
    if let Some(ext) = path_ext(path) {
        if set.contains(&ext) {
            return true;
        }
    }
    let mext = mime_to_ext(mime);
    !mext.is_empty() && set.contains(&mext)
}

fn passive_scan(ctx: &Arc<Ctx>) -> Result<String, String> {
    // The widest window of the three by far: this walks every captured flow, so the
    // operator has every reason to be doing something else (including opening another
    // project) while an agent runs it.
    let gen = ctx.store.generation();
    // Compile builtin rules + custom rules from settings (skip any that fail).
    let mut rules: Vec<(String, String, String, regex::Regex, usize)> = Vec::new();
    // Built-ins the operator switched off in the window stay off here. Without this an
    // agent's scan re-raises findings the operator deliberately dismissed, and writes
    // them into the project as though that decision had never been made.
    let disabled: std::collections::HashSet<String> = match ctx.store.get_setting("passiveDisabled") {
        Ok(Some(Value::Array(ids))) => ids.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => std::collections::HashSet::new(),
    };
    for (id, title, sev, pat, _ci, group) in BUILTIN_RULES {
        if disabled.contains(*id) {
            continue;
        }
        if let Ok(re) = regex::Regex::new(pat) {
            rules.push((id.to_string(), title.to_string(), sev.to_string(), re, *group));
        }
    }
    if let Ok(Some(Value::Array(custom))) = ctx.store.get_setting("passiveRules") {
        for r in custom {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("custom").to_string();
            let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("Custom rule").to_string();
            let sev = r.get("severity").and_then(|v| v.as_str()).unwrap_or("medium").to_string();
            let flags = r.get("flags").and_then(|v| v.as_str()).unwrap_or("");
            let pat = r.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let group = r.get("group").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            // The window writes `enabled: false` when the operator switches a custom
            // rule off (`toggleCustomRule`), and `effectiveRules()` filters on it. The
            // same rule firing over MCP would re-raise findings the operator dismissed.
            if r.get("enabled").is_some_and(|v| v == &Value::Bool(false)) {
                continue;
            }
            let full = if flags.contains('i') { format!("(?i){pat}") } else { pat.to_string() };
            if let Ok(re) = regex::Regex::new(&full) {
                rules.push((id, title, sev, re, group));
            }
        }
    }

    // Scan scope: which resources to look at, by extension / content-type. An
    // absent or empty setting (and '*') means scan everything.
    let scope: Option<std::collections::HashSet<String>> = match ctx.store.get_setting("passiveScope") {
        Ok(Some(Value::Array(a))) => {
            let set: std::collections::HashSet<String> =
                a.iter().filter_map(|v| v.as_str()).map(|s| s.to_ascii_lowercase()).collect();
            if set.is_empty() { None } else { Some(set) }
        }
        _ => None,
    };
    // "In scope only": restrict to hosts in the project Scope (empty scope allows all).
    let in_scope_only = matches!(ctx.store.get_setting("passiveInScope"), Ok(Some(Value::Bool(true))));

    let flows = ctx.store.all().map_err(|e| e.to_string())?;
    let mut findings: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut scanned: Vec<u64> = Vec::new();

    for flow in &flows {
        let s = &flow.summary;
        if in_scope_only && !ctx.scope.contains(&s.host) {
            continue;
        }
        if !flow_in_scope(&s.path, &s.mime, &scope) {
            continue;
        }
        scanned.push(s.id);
        let req = String::from_utf8_lossy(&raw::decode_body(&flow.req_headers, &flow.req_body)).into_owned();
        let res = String::from_utf8_lossy(&raw::decode_body(&flow.res_headers, &flow.res_body)).into_owned();
        let push = |ty: &str, title: &str, sev: &str, where_: &str, m: &str, findings: &mut Vec<Value>, seen: &mut std::collections::HashSet<String>| {
            let key = format!("{}|{ty}|{where_}|{m}", s.id);
            if seen.insert(key.clone()) {
                findings.push(json!({
                    "type": ty, "title": title, "severity": sev, "where": where_, "match": m,
                    "flowId": s.id, "method": s.method, "host": s.host, "path": s.path, "status": s.status, "key": key,
                }));
            }
        };
        for (where_, text) in [("request", &req), ("response", &res)] {
            for (id, title, sev, re, group) in &rules {
                let mut count = 0;
                for cap in re.captures_iter(text) {
                    if count >= 20 {
                        break;
                    }
                    let Some(whole) = cap.get(0) else { continue };
                    let m = cap.get(*group).or_else(|| cap.get(0)).map(|x| x.as_str()).unwrap_or("");
                    if m.is_empty() {
                        continue;
                    }
                    if id == "email" && !plausible_email(text, whole.start(), m) {
                        continue;
                    }
                    push(id, title, sev, where_, &trunc(m), &mut findings, &mut seen);
                    count += 1;
                }
            }
        }
        // Response header checks (CORS / cookie flags / tech disclosure).
        for (k, v) in &flow.res_headers {
            let name = k.to_ascii_lowercase();
            let val = v.trim();
            if name == "access-control-allow-origin" && val == "*" {
                push("cors", "Permissive CORS (Access-Control-Allow-Origin: *)", "medium", "response", &trunc(&format!("{k}: {v}")), &mut findings, &mut seen);
            } else if name == "set-cookie" {
                let low = val.to_ascii_lowercase();
                let mut missing = Vec::new();
                if !low.contains("httponly") {
                    missing.push("HttpOnly");
                }
                if !low.contains("secure") {
                    missing.push("Secure");
                }
                if !missing.is_empty() {
                    push("cookie", &format!("Cookie missing {}", missing.join(" & ")), "low", "response", &trunc(val), &mut findings, &mut seen);
                }
            } else if (name == "server" || name == "x-powered-by" || name == "x-aspnet-version") && !val.is_empty() {
                // A disclosed version outranks a bare product name.
                let sev = if has_version(val) { "low" } else { "info" };
                push("tech", &format!("Tech: {k}: {v}"), sev, "response", &trunc(&format!("{k}: {v}")), &mut findings, &mut seen);
            }
        }
    }

    let order = |s: &str| match s { "high" => 0, "medium" => 1, "low" => 2, _ => 3 };
    findings.sort_by_key(|f| order(f.get("severity").and_then(|x| x.as_str()).unwrap_or("info")));
    let wrote = ctx
        .store
        .put_setting_at("passiveFindings", &Value::Array(findings.clone()), gen)
        .unwrap_or(false);
    if !wrote {
        return Err("the project changed during the scan; no findings were saved".into());
    }
    let _ = ctx
        .store
        .put_setting_at("passiveScanned", &serde_json::to_value(&scanned).unwrap_or(json!([])), gen);
    // Tell an open window, or it will keep showing its stale copy and overwrite this one
    // on the operator's next passive action.
    ctx.events
        .emit(crate::proxy::PASSIVE_EVENT, &json!({ "findings": findings.len(), "scanned": scanned.len() }));

    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in &findings {
        *counts.entry(f.get("severity").and_then(|x| x.as_str()).unwrap_or("info").to_string()).or_insert(0) += 1;
    }
    ok_json(json!({ "scannedFlows": scanned.len(), "findings": findings.len(), "bySeverity": counts }))
}

// ---- organizer ----------------------------------------------------------------

/// A saved request without its request/response text, for listings. `n` is its 1-based
/// position in the collection (the # column in the UI).
fn saved_summary(it: &SavedRequest, n: usize) -> Value {
    json!({
        "id": it.id, "n": n, "name": it.name, "method": it.method, "host": it.host, "path": it.path,
        "status": it.status, "label": it.label, "note": it.note, "color": it.color,
        "source": it.source, "savedAt": it.saved_at,
    })
}

/// A collection by id or (case-insensitive) name.
fn find_collection<'a>(org: &'a Organizer, key: &str) -> Option<&'a organizer::Collection> {
    let key = key.trim();
    org.collections.iter().find(|c| c.id == key || c.name.eq_ignore_ascii_case(key))
}

fn collection_ref(org: &Organizer, c: &organizer::Collection) -> Value {
    json!({ "id": c.id, "name": c.name, "active": org.active_id.as_deref() == Some(c.id.as_str()) })
}

/// Tell an open Organizer about an edit made here (it skips revisions it already has).
fn organizer_changed(ctx: &Arc<Ctx>, org: &Organizer) {
    ctx.events.emit(organizer::EVENT, &json!({ "rev": org.rev }));
}

/// The saved request an edit touched, with its collection.
fn applied_item(applied: &organizer::Applied) -> Result<Value, String> {
    let org = &applied.state;
    let id = applied.item_id.as_deref().ok_or("the edit returned no item")?;
    for c in &org.collections {
        if let Some((i, it)) = c.items.iter().enumerate().find(|(_, it)| it.id == id) {
            return Ok(json!({ "collection": collection_ref(org, c), "item": saved_summary(it, i + 1) }));
        }
    }
    Err(format!("no saved request {id}"))
}

fn organizer_list(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let org = organizer::load(&ctx.store)?;
    let wanted = arg_str(a, "collection");
    let cols: Vec<&organizer::Collection> = if wanted.trim().is_empty() {
        org.collections.iter().collect()
    } else {
        vec![find_collection(&org, &wanted).ok_or_else(|| format!("no collection '{wanted}'"))?]
    };
    let active = org.active_id.as_deref().and_then(|id| org.collections.iter().find(|c| c.id == id));
    let collections: Vec<Value> = cols
        .iter()
        .map(|c| {
            let mut v = collection_ref(&org, c);
            v["count"] = json!(c.items.len());
            v["items"] = json!(c.items.iter().enumerate().map(|(i, it)| saved_summary(it, i + 1)).collect::<Vec<_>>());
            v
        })
        .collect();
    ok_json(json!({
        "activeCollection": active.map(|c| json!({ "id": c.id, "name": c.name })),
        "colors": HIGHLIGHT_COLORS,
        "collections": collections,
    }))
}

fn organizer_get(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let id = arg_str(a, "id");
    let org = organizer::load(&ctx.store)?;
    for c in &org.collections {
        if let Some((i, it)) = c.items.iter().enumerate().find(|(_, it)| it.id == id) {
            let mut v = saved_summary(it, i + 1);
            v["target"] = json!(it.target);
            v["request"] = json!(it.request);
            v["response"] = json!(it.response);
            v["collection"] = collection_ref(&org, c);
            return ok_json(v);
        }
    }
    Err(format!("no saved request {id}"))
}

/// Save a captured flow (`flowId`) or a raw request into a collection; see the docs.
fn organizer_save(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let opt = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
    let mut item = match a.get("flowId").and_then(Value::as_u64) {
        Some(id) => {
            let flow = ctx.store.get(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {id}"))?;
            let s = &flow.summary;
            NewItem {
                name: format!("{}{}", s.host, s.path),
                target: format!("{}://{}", s.scheme, s.host),
                request: raw::request_text(&flow),
                response: raw::response_text(&flow),
                status: (s.status != 0).then_some(s.status),
                note: s.note.clone(),
                color: s.color.clone(),
                // Not "history": that is what the window writes when the operator saves
                // a flow by hand, and an agent's save must not look like one.
                source: "mcp".into(),
                ..Default::default()
            }
        }
        None => {
            let (request, target) = (arg_str(a, "request"), arg_str(a, "target"));
            if request.trim().is_empty() || target.trim().is_empty() {
                return Err("give flowId, or request + target".into());
            }
            NewItem {
                request,
                target,
                response: arg_str(a, "response"),
                status: a.get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok()),
                source: "mcp".into(),
                ..Default::default()
            }
        }
    };
    if let Some(v) = opt("name") {
        item.name = v;
    }
    if let Some(v) = opt("label") {
        item.label = v;
    }
    if let Some(v) = opt("note") {
        item.note = v;
    }
    if let Some(v) = opt("color") {
        item.color = v;
    }
    let collection = opt("collection").filter(|c| !c.trim().is_empty());
    let applied = organizer::apply(&ctx.store, Op::Save { collection, item })?;
    organizer_changed(ctx, &applied.state);
    ok_json(applied_item(&applied)?)
}

/// Severities an agent finding can carry: the passive scanner's own vocabulary, so an
/// operator reads both lists the same way.
const FINDING_SEVERITIES: [&str; 4] = ["high", "medium", "low", "info"];

/// How sure the caller is. Recorded because a claim without it reads as certainty.
const FINDING_CONFIDENCE: [&str; 3] = ["certain", "firm", "tentative"];

/// Collection an agent finding lands in unless the caller names another.
const FINDINGS_COLLECTION: &str = "Findings";

/// Written into every agent finding's note, by this server, never by the caller.
///
/// The Source column marks the row inside the window, but a note is the part that gets
/// copied into a report, and a report is where a confusion about who claimed what does
/// real damage. "Claimed" is the load-bearing word: Ngintip did not verify this.
const FINDING_HEADER: &str = "[AGENT FINDING - claimed by an LLM over MCP, not verified by Ngintip]";

/// Record a finding the caller is claiming, in the Organizer, where the operator can
/// actually see it.
///
/// Deliberately not a parameter on `organizer_save`: this tool can require the fields a
/// report needs, and it forces the attribution that a caller must not be able to switch
/// off. `source`, `color` and the note header are set here and cannot be passed in.
fn findings_add(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let title = arg_str(a, "title").trim().to_string();
    let detail = arg_str(a, "detail").trim().to_string();
    if title.is_empty() || detail.is_empty() {
        return Err("give title (the claim in a line) and detail (what it is, how it was shown)".into());
    }
    let severity = arg_enum(a, "severity", &FINDING_SEVERITIES, "")?;
    if severity.is_empty() {
        return Err(format!("severity is required; use one of: {}", FINDING_SEVERITIES.join(", ")));
    }
    let confidence = arg_enum(a, "confidence", &FINDING_CONFIDENCE, "tentative")?;

    // Flows named as evidence. The one in `flowId` has its exchange stored with the
    // item; the rest are cited by id, because a saved item holds a single exchange.
    let flow_id = arg_int(a, "flowId", 0)?;
    let mut cited: Vec<u64> = Vec::new();
    if let Some(list) = a.get("citedFlowIds") {
        let arr = list.as_array().ok_or("citedFlowIds must be an array of flow ids")?;
        for v in arr {
            cited.push(v.as_u64().ok_or("citedFlowIds must be whole numbers")?);
        }
    }
    if flow_id > 0 && !cited.contains(&flow_id) {
        cited.insert(0, flow_id);
    }

    let evidence = if cited.is_empty() {
        "evidence: none cited".to_string()
    } else {
        format!("evidence: {} (cite the wire bytes, not a decoded view)", cited.iter().map(|id| format!("flow {id}")).collect::<Vec<_>>().join(", "))
    };
    let note = format!("{FINDING_HEADER}\nseverity: {severity}   confidence: {confidence}\n{evidence}\n\n{detail}");
    let color = match severity.as_str() {
        "high" => "red",
        "medium" => "orange",
        "low" => "yellow",
        _ => "gray",
    };

    let mut item = if flow_id > 0 {
        let flow = ctx.store.get(flow_id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {flow_id}"))?;
        let s = &flow.summary;
        NewItem {
            name: format!("{}{}", s.host, s.path),
            target: format!("{}://{}", s.scheme, s.host),
            // The viewer-capped text, not the full one: every Organizer edit rewrites the
            // whole organizer blob, so embedding megabyte bodies makes each later write
            // worse. The flow id in the note is the authoritative pointer; this is a copy
            // kept for convenience.
            request: raw::request_text(&flow),
            response: raw::response_text(&flow),
            status: (s.status != 0).then_some(s.status),
            ..Default::default()
        }
    } else {
        NewItem { name: title.clone(), ..Default::default() }
    };
    // Severity in the label as well: the Organizer already sorts by label, so this makes
    // agent findings group and order by severity with nothing added to the window.
    item.label = format!("[agent {severity}] {title}");
    item.note = note;
    item.color = color.into();
    // A distinct source value, not the "mcp" a saved request gets: an agent's CLAIM and
    // an agent's saved REQUEST must not look the same in the Source column.
    item.source = "mcp-finding".into();

    let collection = arg_str(a, "collection");
    let collection = Some(if collection.trim().is_empty() { FINDINGS_COLLECTION.to_string() } else { collection.trim().to_string() });
    let applied = organizer::apply(&ctx.store, Op::Save { collection, item })?;
    organizer_changed(ctx, &applied.state);

    let mut out = applied_item(&applied)?;
    out["attribution"] = json!(
        "Recorded as a claim by this server, not as a Ngintip finding: source=mcp-finding, the label is prefixed [agent <severity>], and the note opens with the header. Ngintip's own passive scanner never writes here."
    );
    // Warn, do not refuse. Reading the capture is unrestricted here, but a finding is
    // work product, and a finding about a host nobody authorised is not deliverable.
    let out_of_scope: Vec<u64> = cited
        .iter()
        .copied()
        .filter(|id| {
            ctx.store
                .get(*id)
                .ok()
                .flatten()
                .is_some_and(|f| !ctx.scope.contains(&f.summary.host))
        })
        .collect();
    if !out_of_scope.is_empty() {
        out["scopeWarning"] = json!(format!(
            "Cited flows {out_of_scope:?} are on hosts outside the target scope. The finding was recorded, but check it belongs in the engagement before it reaches a report."
        ));
    }
    ok_json(out)
}

/// Set a saved request's label, note and/or colour (omitted fields are kept).
fn organizer_set(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let id = arg_str(a, "id");
    let opt = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
    let (label, note, color) = (opt("label"), opt("note"), opt("color"));
    if label.is_none() && note.is_none() && color.is_none() {
        return Err("give at least one of label, note, color".into());
    }
    let applied = organizer::apply(&ctx.store, Op::Update { item_id: id, label, note, color })?;
    organizer_changed(ctx, &applied.state);
    ok_json(applied_item(&applied)?)
}

// ---- vault (read-only) ----------------------------------------------------------

fn vault_list(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let query = arg_str(a, "query");
    let entries = vault::entries(&ctx.store)?;
    let shown: Vec<Value> = entries
        .iter()
        .filter(|e| e.matches(&query))
        .map(|e| serde_json::to_value(e.redacted()).unwrap_or(Value::Null))
        .collect();
    ok_json(json!({ "total": entries.len(), "returned": shown.len(), "entries": shown }))
}

fn vault_get(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let (id, label) = (arg_str(a, "id"), arg_str(a, "label"));
    let entries = vault::entries(&ctx.store)?;
    let found: Vec<&vault::Entry> = if !id.trim().is_empty() {
        entries.iter().filter(|e| e.id == id.trim()).collect()
    } else if !label.trim().is_empty() {
        entries.iter().filter(|e| e.label.trim().eq_ignore_ascii_case(label.trim())).collect()
    } else {
        return Err("give id or label".into());
    };
    match found.as_slice() {
        [e] => ok_json(serde_json::to_value(e).unwrap_or(Value::Null)),
        [] => Err("no vault entry with that id or label (see vault_list)".into()),
        many => Err(format!("{} entries share that label; use the id from vault_list", many.len())),
    }
}

// ---- proxy status -----------------------------------------------------------

/// Said on every status reply, because it is what stops the two mistakes this tool
/// otherwise invites: thinking it can change the proxy, and thinking the absence of an
/// address means there is nothing bound.
const STATUS_NOTE: &str = "Read-only. This server cannot start, stop or rebind a listener; ask the operator. Bind addresses are deliberately not reported: `exposure` says how far a listener reaches without naming an address on the operator's machine or a client's network.";

fn proxy_status(ctx: &Arc<Ctx>) -> Result<String, String> {
    let addrs = ctx.bound_listeners();
    // Port and reach, never the address. A tunnel bind is a VPN address on somebody
    // else's network and a lan bind is the operator's internal address; neither is
    // needed for any decision a tool caller can make, and this reply may travel to a
    // model that is not running on this machine.
    let listeners: Vec<Value> = addrs
        .iter()
        .map(|a| json!({ "port": a.port(), "exposure": crate::commands::exposure_of_addr(a) }))
        .collect();

    let stats = ctx.store.stats().map_err(|e| e.to_string())?;
    let ext = ctx.extensions.status();
    let now = crate::store::now_ms();

    let mut out = json!({
        "listening": !listeners.is_empty(),
        "listeners": listeners,
        "flows": stats.flows,
        "wsMessages": stats.ws_messages,
        "intercept": {
            "requests": ctx.intercept.is_enabled(),
            "responses": ctx.intercept.holds_responses(),
            // The count, never the held messages: a held request is a stalled
            // connection, which is the fact that matters here.
            "pending": ctx.intercept.pending().len(),
            "websocket": ctx.ws_intercept.is_enabled(),
            "websocketPending": ctx.ws_intercept.pending().len(),
        },
        "invisible": ctx.invisible(),
        "scopePatterns": ctx.scope.get().len(),
        "extensions": {
            "enabled": ext.enabled,
            // Counts, not the structs: `ExtensionsStatus` also carries interpreter and
            // virtualenv paths, which contain the operator's username.
            "loaded": ext.extensions.len(),
            "running": ext.extensions.iter().filter(|e| e.running).count(),
            "failed": ext.extensions.iter().filter(|e| e.error.is_some()).count(),
        },
        "note": STATUS_NOTE,
    });
    if stats.newest_ts > 0 {
        out["newestFlowTs"] = json!(stats.newest_ts);
        // Models are unreliable at epoch arithmetic, so the age is computed here.
        out["newestFlowAgeMs"] = json!(now.saturating_sub(stats.newest_ts));
    }

    // "Nothing captured" has more than one cause, and a caller handed `listening: true,
    // flows: 0` will otherwise report that the application makes no HTTP requests. Name
    // the other causes rather than leaving it to guess.
    if stats.flows == 0 {
        let mut hints: Vec<&str> = Vec::new();
        if listeners.is_empty() {
            hints.push("No listener is bound: the proxy is not running. Ask the operator to start it.");
        } else {
            hints.push("A listener is bound but nothing has been captured. The client may not be pointed at the proxy.");
            hints.push("The CA may not be trusted on the client, in which case HTTPS fails before anything is recorded.");
        }
        if ctx.intercept.is_enabled() && !ctx.intercept.pending().is_empty() {
            hints.push("Intercept is holding messages, so traffic is stalled waiting for the operator.");
        }
        if !ctx.scope.get().is_empty() {
            hints.push("A target scope is set; it does not hide flows from history_list by default, but it does limit the passive scan.");
        }
        out["hints"] = json!(hints);
    }
    ok_json(out)
}

// ---- tool schemas & docs ----------------------------------------------------

fn tool_defs() -> Value {
    let obj = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    json!([
        { "name": "history_list", "description": "List captured HTTP flows (newest first). Filter by method, host substring, status, a 'contains' match on method+host+path, or inScope (only hosts in the target scope; default false = everything). Pages with a cursor, not an index: when `hasMore` is true, pass the returned `nextBeforeId` back as `beforeId` for the next page. `offset` is refused, because an index over a live capture silently re-reads and skips rows. `total` is the matching rows right now and drifts as capture continues; `newestId` shows it moving.",
          "inputSchema": obj(json!({ "method": {"type":"string"}, "host": {"type":"string"}, "status": {"type":"integer"}, "contains": {"type":"string"}, "inScope": {"type":"boolean"}, "limit": {"type":"integer"}, "beforeId": {"type":"integer"} }), json!([])) },
        { "name": "history_get", "description": "One flow's request and response. `request`/`response` are the bytes on the wire. Read only what you need: side=request|response halves the reply, part=headers answers a header question for a few hundred bytes, and part=body pages a large body with offset plus the returned nextOffset. A body comes back capped (maxBytes, 32 KB by default); the `requestSlice`/`responseSlice` receipt says what you got, whether it is `complete`, and whether capture itself had already truncated it. A binary body is not returned as text; ask part=body with encoding=base64 for its bytes. When an extension or an intercept edit gives the flow another version - most usefully a decode_* extension that makes an encrypted body readable - those come back under `versions` too (view=auto, the default), cut the same way. view=wire returns the wire alone; view=extension puts the readable form in `request`/`response`.",
          "inputSchema": obj(json!({ "id": {"type":"integer"}, "view": {"type":"string", "enum": ["auto","wire","extension"]}, "side": {"type":"string", "enum": ["both","request","response"]}, "part": {"type":"string", "enum": ["all","headers","body"]}, "offset": {"type":"integer"}, "maxBytes": {"type":"integer"}, "encoding": {"type":"string", "enum": ["text","base64"]} }), json!(["id"])) },
        { "name": "history_search", "description": "Full-text search across request/response bodies and WebSocket messages; returns matching flow summaries. Pages like history_list (nextBeforeId -> beforeId). Flows listed in `wsMatches` matched inside a WebSocket message, so read those with ws_messages rather than history_get. The index covers only the first 256 KB of each decoded body and skips binary bodies, so no match is not proof of absence; an empty result says so.",
          "inputSchema": obj(json!({ "query": {"type":"string"}, "limit": {"type":"integer"}, "beforeId": {"type":"integer"} }), json!(["query"])) },
        { "name": "ws_messages", "description": "Read one flow's WebSocket messages, oldest first. A WebSocket flow's history_get shows only the handshake, so this is the only way to see what the socket carried. Pages with a cursor (nextAfterId -> afterId) because a live socket keeps growing; filter with direction=client|server and `contains`. `payload` is a rendering: binary comes back as a hex dump of its first 256 bytes and a close message as its code and reason, so cite `length` and check `complete` rather than measuring the text.",
          "inputSchema": obj(json!({ "flowId": {"type":"integer"}, "afterId": {"type":"integer"}, "direction": {"type":"string","enum":["both","client","server"]}, "contains": {"type":"string"}, "limit": {"type":"integer"}, "maxBytes": {"type":"integer"} }), json!(["flowId"])) },
        { "name": "history_set_note", "description": "Set (or clear) the note on a captured flow by id. It shows in HTTP History; pass an empty note to clear it.",
          "inputSchema": obj(json!({ "id": {"type":"integer"}, "note": {"type":"string"} }), json!(["id","note"])) },
        { "name": "history_set_color", "description": "Set (or clear) the highlight colour on a captured flow by id. It tints the row in HTTP History. Allowed: red, orange, yellow, green, cyan, blue, purple, pink, gray; pass an empty string to clear it.",
          "inputSchema": obj(json!({ "id": {"type":"integer"}, "color": {"type":"string", "enum": ["red","orange","yellow","green","cyan","blue","purple","pink","gray",""]} }), json!(["id","color"])) },
        { "name": "history_get_color", "description": "List the highlight colours history_set_color accepts. With an optional id, also returns that flow's current colour.",
          "inputSchema": obj(json!({ "id": {"type":"integer"} }), json!([])) },
        { "name": "scope_get", "description": "Get the target scope host patterns (e.g. example.com, *.example.com). An empty list means everything is in scope.", "inputSchema": obj(json!({}), json!([])) },
        { "name": "scope_set", "description": "Replace the target scope host patterns (e.g. [\"example.com\", \"*.api.example.com\"]). Applies immediately and is saved with the project; an empty list means everything is in scope.",
          "inputSchema": obj(json!({ "patterns": {"type":"array","items":{"type":"string"}} }), json!(["patterns"])) },
        { "name": "scope_check", "description": "Check whether a host is in the target scope; returns inScope and which patterns matched.",
          "inputSchema": obj(json!({ "host": {"type":"string"} }), json!(["host"])) },
        { "name": "repeater_send", "description": "Send one request (raw+target, or clone flowId) and record it in Repeater history. Returns status/length, an 8 KB response preview, and the `flowId` it was recorded as, so the full exchange can be read back with history_get instead of guessed at. When a repeater-scope extension transformed the exchange, also the request actually sent and the response as it arrived.",
          "inputSchema": obj(json!({ "raw": {"type":"string"}, "target": {"type":"string"}, "flowId": {"type":"integer"}, "tab": {"type":"integer"} }), json!([])) },
        { "name": "repeater_send_batch", "description": "Send the same request many times, in parallel (default) or sequential. For race conditions (e.g. duplicate-registration). Each row carries the `flowId` it was recorded as, so an interesting one can be read with history_get.",
          "inputSchema": obj(json!({ "raw": {"type":"string"}, "target": {"type":"string"}, "flowId": {"type":"integer"}, "count": {"type":"integer"}, "mode": {"type":"string","enum":["parallel","sequential"]}, "tab": {"type":"integer"} }), json!([])) },
        { "name": "repeater_groups", "description": "List Repeater tab groups and their tabs.", "inputSchema": obj(json!({}), json!([])) },
        { "name": "repeater_send_group", "description": "Send every tab in a Repeater group, in parallel (default) or sequential. Each row carries the `flowId` it was recorded as.",
          "inputSchema": obj(json!({ "group": {"type":"string"}, "mode": {"type":"string","enum":["parallel","sequential"]} }), json!(["group"])) },
        { "name": "intruder_run", "description": "Run an Intruder attack. Mark positions in `raw` with the section sign §. type: sniper|battering|pitchfork|cluster. payloads: a list of lines (one set) or a list of lists (a set per position). Each result row carries the `flowId` it was recorded as, so an outlier can be read with history_get rather than hunted for in history_list.",
          "inputSchema": obj(json!({ "raw": {"type":"string"}, "target": {"type":"string"}, "flowId": {"type":"integer"}, "type": {"type":"string"}, "payloads": {}, "concurrency": {"type":"integer"} }), json!([])) },
        { "name": "passive_findings", "description": "List passive-scan findings (from the last scan). Filter by severity. Paged: `count` is every matching finding, and when `hasMore` is true pass the returned `nextOffset` back as `offset`. An index is right here, unlike the flow lists, because this array is only ever replaced wholesale by passive_scan.",
          "inputSchema": obj(json!({ "severity": {"type":"string","enum":["high","medium","low","info"]}, "limit": {"type":"integer"}, "offset": {"type":"integer"} }), json!([])) },
        { "name": "passive_scan", "description": "Run the passive scan over all captured flows now (secrets, tokens, risky headers) and store the findings.", "inputSchema": obj(json!({}), json!([])) },
        { "name": "passive_rules_get", "description": "Get the passive rules (built-in + custom).", "inputSchema": obj(json!({}), json!([])) },
        { "name": "passive_rules_set", "description": "Replace the custom passive rules. Each: {id,title,severity,pattern,flags,group}.",
          "inputSchema": obj(json!({ "rules": {"type":"array"} }), json!(["rules"])) },
        { "name": "organizer_list", "description": "List Organizer collections and their saved requests (without request/response text): id, #, method, host, path, status, label, note, colour, source. Optional collection (id or name) lists just that one. Also returns the active collection (where saves go) and the allowed colours.",
          "inputSchema": obj(json!({ "collection": {"type":"string"} }), json!([])) },
        { "name": "organizer_get", "description": "One saved request by id with its full request and response text, label, note, colour and collection.",
          "inputSchema": obj(json!({ "id": {"type":"string"} }), json!(["id"])) },
        { "name": "organizer_save", "description": "Save a request into the Organizer: a captured flow (flowId) or a raw request (request + target, optional response and status). It goes into collection (id or name; an unknown name creates it), else the active collection, else \"Saved\". Optional name, label, note, color. The open Organizer updates live.",
          "inputSchema": obj(json!({ "flowId": {"type":"integer"}, "request": {"type":"string"}, "target": {"type":"string"}, "response": {"type":"string"}, "status": {"type":"integer"}, "collection": {"type":"string"}, "name": {"type":"string"}, "label": {"type":"string"}, "note": {"type":"string"}, "color": {"type":"string", "enum": ["red","orange","yellow","green","cyan","blue","purple","pink","gray",""]} }), json!([])) },
        { "name": "organizer_set", "description": "Set the label, note and/or highlight colour of a saved request by id. Omit a field to keep it; an empty string clears it. Colours: red, orange, yellow, green, cyan, blue, purple, pink, gray. The open Organizer updates live.",
          "inputSchema": obj(json!({ "id": {"type":"string"}, "label": {"type":"string"}, "note": {"type":"string"}, "color": {"type":"string", "enum": ["red","orange","yellow","green","cyan","blue","purple","pink","gray",""]} }), json!(["id"])) },
        { "name": "findings_add", "description": "Record a finding YOU are claiming, where the operator can see it: it lands as an item in the Organizer's Findings collection, live in the open window. Requires title, severity and detail; cite the flows that show it with flowId and citedFlowIds. The attribution is not yours to set: the row is marked source=mcp-finding, the label is prefixed [agent <severity>], and the note opens with a header saying an LLM claimed this and Ngintip did not verify it. That matters because this text can end up in a client-facing report. Cite the wire bytes, not a decoded view.",
          "inputSchema": obj(json!({ "title": {"type":"string"}, "severity": {"type":"string","enum":["high","medium","low","info"]}, "detail": {"type":"string"}, "confidence": {"type":"string","enum":["certain","firm","tentative"]}, "flowId": {"type":"integer"}, "citedFlowIds": {"type":"array","items":{"type":"integer"}}, "collection": {"type":"string"} }), json!(["title","severity","detail"])) },
        { "name": "vault_list", "description": "List Vault credentials (Settings > Vault): label, role, note and typed fields (username, email, password, apikey, token, url, text). Secret values are hidden; call vault_get for one entry's full values. Optional query matches label, role, note, field names and non-secret values. Read-only.",
          "inputSchema": obj(json!({ "query": {"type":"string"} }), json!([])) },
        { "name": "vault_get", "description": "One Vault credential with every field value, secrets included, by id or by exact label (case-insensitive). Read-only.",
          "inputSchema": obj(json!({ "id": {"type":"string"}, "label": {"type":"string"} }), json!([])) },
        { "name": "extensions_status", "description": "State of the Python extension engine: interpreter, venv, and each loaded script (id, enabled, running, error).", "inputSchema": obj(json!({}), json!([])) },
        { "name": "extensions_enable", "description": "Turn the whole extension engine on or off.",
          "inputSchema": obj(json!({ "enabled": {"type":"boolean"} }), json!(["enabled"])) },
        { "name": "extension_toggle", "description": "Enable or disable one extension by id (see extensions_status).",
          "inputSchema": obj(json!({ "id": {"type":"string"}, "enabled": {"type":"boolean"} }), json!(["id","enabled"])) },
        { "name": "extensions_logs", "description": "Recent lines from the extension debug console (script stderr and lifecycle).", "inputSchema": obj(json!({}), json!([])) },
        { "name": "proxy_status", "description": "Whether the proxy is actually capturing, and if not, why not. Call this first when history_list comes back empty: it tells 'no listener is bound' from 'bound but nothing captured', and when there are no flows it lists the other causes (client not pointed at the proxy, CA not trusted, intercept holding traffic) so they are not mistaken for 'this app makes no HTTP requests'. Read-only: it cannot start, stop or rebind anything. Reports each listener's port and reach, never its bind address.", "inputSchema": obj(json!({}), json!([])) },
    ])
}

fn docs() -> String {
    // Placeholders keep JSON examples (with real braces) free of format escaping.
    DOCS_TEMPLATE
        .replace("%APP%", brand::NAME)
        .replace("%VER%", env!("CARGO_PKG_VERSION"))
        .replace("%PROTO%", PROTOCOL_VERSION)
}

const DOCS_TEMPLATE: &str = r####"# %APP% MCP server (v%VER%)

%APP% is an intercepting HTTP(S) proxy. This MCP server lets an LLM drive it: read
and search captured traffic, replay and fuzz requests, run the passive scanner,
organize saved requests in the Organizer, and read the project's credential Vault.
Everything an operator does in the Repeater, Intruder and Passive tabs is available
here, and every request this server sends is recorded in History like ordinary proxy
traffic.

Security: the server binds to 127.0.0.1 only. Treat all captured data as
sensitive; it can contain credentials, tokens and personal data.

## Transport and protocol

- Protocol: Model Context Protocol (MCP) over Streamable HTTP. Latest revision
  %PROTO% (the date is the MCP spec revision, not a release date). The server also
  accepts and echoes back an older revision a client requests in `initialize`.
- Endpoint: POST a single JSON-RPC 2.0 message to `/mcp`. The response is
  `Content-Type: application/json`.
- This page: GET the configured docs path (default `/docs`), served as Markdown.
- Send `Content-Type: application/json`. No authentication and no session are
  required; an `mcp-session-id` request header is accepted but ignored. CORS is
  open so local tools can call it from a browser.
- Methods: `initialize`, `tools/list`, `tools/call`, `ping`. A notification (a
  message with no `id`, such as `notifications/initialized` or
  `notifications/cancelled`) is answered with 202 Accepted and no body.

### JSON-RPC envelope

Request:

    { "jsonrpc": "2.0", "id": 1, "method": "<method>", "params": { } }

Success:

    { "jsonrpc": "2.0", "id": 1, "result": { } }

Error (unknown method, bad params, and so on):

    { "jsonrpc": "2.0", "id": 1, "error": { "code": -32601, "message": "..." } }

### Calling a tool

Every tool runs through `tools/call`:

    {
      "jsonrpc": "2.0",
      "id": 2,
      "method": "tools/call",
      "params": { "name": "history_list", "arguments": { "limit": 20 } }
    }

The result wraps the tool output as text:

    { "content": [ { "type": "text", "text": "<JSON string>" } ], "isError": false }

The `text` field is itself a JSON string. Parse it to get the structured result
documented for each tool below. When a tool fails, `isError` is true and `text`
is the error message.

## Quickstart

1. Initialize:

       { "jsonrpc": "2.0", "id": 1, "method": "initialize" }

2. List tools:

       { "jsonrpc": "2.0", "id": 2, "method": "tools/list" }

3. Call a tool:

       { "jsonrpc": "2.0", "id": 3, "method": "tools/call",
         "params": { "name": "history_list", "arguments": { "limit": 20 } } }

## Concepts

### Sourcing a request (Repeater and Intruder)

The Repeater and Intruder tools take a request in one of two ways:

- `raw` plus `target`: `raw` is the full request text (request line, headers, a
  blank line, then an optional body). `target` is the origin, for example
  `https://example.com`, and sets the scheme and host. Example `raw`:

      POST /login HTTP/1.1
      Host: example.com
      Content-Type: application/json

      {"user":"admin","pass":"x"}

- `flowId`: clone a captured flow by its numeric id (from `history_list` or
  `history_search`). The request text and target come from that flow.

If `flowId` is present it is used. Otherwise `raw` is required.

### Recording

Sends from `repeater_send`, `repeater_send_batch`, `repeater_send_group` and
`intruder_run` are recorded in History (the flows list), so you can inspect any of
them afterwards with `history_get`, using the `flowId` the send itself hands back rather
than hunting for the row in `history_list`. Recording happens on a background thread, so
an id can be read a moment before its row lands; `history_get` says so in that case, and
one retry is enough. A flow id means something only inside its project, and emptying
History reseeds the counter, so pin a citation with the `flowTs` returned beside the id.
The Repeater tools also append to a Repeater
tab history, chosen with `tab` (default 9001).

### Message versions (extensions)

An extension can define `decode_*` hooks that make an encrypted or encoded body
readable without changing anything on the wire, and `encode_*` / `process_*` hooks
that do change the message. A flow can therefore have more than one version:

- the **wire** message, which is what really crossed the connection. This is always
  what `request` / `response` mean by default, in every tool.
- the **extension** (decoded) version, produced by a `decode_*` hook. Display only:
  it was never sent and never received. On an API with encrypted bodies this is the
  only readable form, so ask for it when the wire body looks like ciphertext.
- the **original** version, which is what the message was before an intercept edit or
  an `encode_*` / `process_*` hook rewrote it.

A version is only ever reported when it actually differs from the wire, so an ordinary
flow carries no extra keys at all. The decoded version is computed on demand and needs
the extension enabled and the flow's host inside the target scope (`scope_get` /
`scope_set`); without that only the wire exists.

Never present a decoded body as what the server sent or received. If a finding depends
on the exact bytes, cite the wire version.

### Intruder positions and attack types

Mark each spot to inject in `raw` with the section sign (the single character
U+00A7, written here as `§`), one pair per position, for example:

    GET /user?id=§1§ HTTP/1.1
    Host: example.com

`type` selects how payloads fill positions:

- `sniper`: one position at a time, from a single payload set. Total requests are
  positions multiplied by payloads.
- `battering`: the same payload placed in every position at once (battering ram).
- `pitchfork`: one set per position, advanced together, stopping at the shortest
  set.
- `cluster`: every combination across the sets (cluster bomb, cartesian product).

`payloads` is either a list of strings (one shared set) or a list of lists (one
set per position, required for `pitchfork` and `cluster`). The generated request
count is capped at 5000.

### Paging

Every list here answers with `hasMore`. When it is true there is also a `next<X>` field,
and the rule is always the same: **pass the value of `next<X>` back as `<x>`**.

- `history_list`, `history_search`: `nextBeforeId` -> `beforeId`
- `ws_messages`: `nextAfterId` -> `afterId`
- `passive_findings`: `nextOffset` -> `offset`

The flow lists page by id, not by index, and refuse an `offset` argument. Capture is
live: new flows arrive at the newest end, so an index would move under you and you would
silently re-read rows you had seen and silently skip rows you had not. Ids do not move.
A flow that arrives while you are paging has a higher id and is simply outside the walk,
which is the right answer for "list what existed when I started".

`passive_findings` does take an `offset`, because that array is only ever replaced
wholesale by `passive_scan`; nothing is inserted into the middle of it between calls.

`total` (and `matches`, and `count`) is the size of the matching set at the moment of the
call. It is not a fixed denominator: it drifts while capture runs.

### Reading only part of a message

`history_get` returns whole messages by default, and a 2 MB body is 2 MB. Three arguments
keep a read small, and they compose:

- `side=request` or `side=response` returns one message instead of two. The other key is
  absent, not empty.
- `part=headers` returns the start line, the headers and the blank line, and no body.
  This is the cheapest useful read in the whole API: a few hundred bytes to answer a
  question about `Set-Cookie`, `Authorization` or a CSP header.
- `part=body` with `offset` and `maxBytes` pages a large body. `offset` counts BYTES of
  the decoded body, not characters, because every other size here is in bytes. The end is
  snapped to a character boundary and `nextOffset` reports where it landed, so paging by
  `nextOffset` never splits a character.

Each returned message carries a receipt, `requestSlice` or `responseSlice`, and the field
to check is `complete`. Other fields worth reading:

- `bodyBytes`: the decoded body's size. Absent under gzip, br or zstd, where the true
  length cannot be known without decompressing the whole thing.
- `capturedTruncated`: capture itself kept only the start of this body. A read can be
  `complete` and still be less than what crossed the wire; this is how you tell.
- `binary`: the body is not text, so it was not returned as text. Lossy UTF-8 of binary
  bytes is replacement characters, which is both larger than the body and useless. Ask
  again with `part=body` and `encoding=base64` for its bytes.

Nothing is appended to the message text itself when it is partial. The receipt carries
all of it, so a sliced body can still be diffed or hashed.

### WebSocket traffic

A WebSocket flow's `history_get` shows only the HTTP handshake. Everything the socket
carried lives in `ws_messages`, keyed by the same `flowId`. `history_get` says so, with
`wsMessages` and `wsNote`, whenever a flow carried any.

Fragments are already reassembled, so these are messages, not frames. Control messages
(close, ping, pong) appear as their own entries.

`payload` is a rendering, not the bytes. A binary message is a hex dump of its first 256
bytes; a close message is its code and reason. Three sizes can differ and all three are
reported: `length` is what the peer sent, `stored` is what capture kept, `returned` is
what this page read. Only treat `payload` as the whole payload when `complete` is true,
and quote `length` rather than measuring the text.

`history_search` matches inside WebSocket messages too. When it does, the flow ids appear
in `wsMatches`: read those with `ws_messages`, passing the same text as `contains`, or
`history_get` will show you a bare 101 handshake and look like it lied.

### Passive scan

`passive_scan` runs the built-in and custom rules over captured flows and stores
the findings. What gets scanned is controlled per project by the `passiveScope`
setting: a list of file extensions or content types, or `["*"]` for everything; an
unset or empty scope scans everything. `passive_findings` reads the stored
findings. `passive_rules_get` and `passive_rules_set` manage the custom rules.

### Organizer and Vault

The Organizer keeps named collections of saved requests: the request and the response
as they were when saved, each with a label, a note and a highlight colour. One
collection can be active; "Save to Organizer" in the app and `organizer_save` without a
`collection` put new items there, or into a "Saved" collection when none is active.
Saved-request ids are strings, unique across collections. Changes made over MCP show
up in an open Organizer right away.

The Vault (Settings > Vault) holds the target's credentials: per entry a label, a role,
a note and a list of typed fields (`username`, `email`, `password`, `apikey`, `token`,
`url`, `text`), each marked secret or not. It is read-only over MCP. `vault_list` hides
secret values and `vault_get` returns one entry in full, so fetch only the entry you
need. The Vault is stored unencrypted in the project file.

## Tools

### history_list

List captured flows, newest first.

Arguments (all optional):

- `method` (string): exact HTTP method, case insensitive.
- `host` (string): substring match on the host, case insensitive.
- `status` (integer): exact status code.
- `contains` (string): substring match over "method host path".
- `inScope` (boolean): only flows whose host is in the target scope. Default false
  (return everything). An empty scope means everything is in scope.
- `limit` (integer): maximum flows to return, 1 to 1000, default 100.
- `beforeId` (integer): the paging cursor. Returns flows with a lower id, so pass back
  the `nextBeforeId` from your last page. Absent or 0 starts at the newest.

`offset` is refused with an error naming `beforeId`. See Paging above for why an index
over a live capture silently re-reads and skips rows.

Returns: `{ total, newestId, returned, hasMore, flows: [summary, ...] }`, plus
`nextBeforeId` when `hasMore` is true. Each summary has `id`, `ts`, `method`, `scheme`,
`host`, `path`, `status`, `length`, `mime`, `durationMs` and more.

`total` is the rows matching your filters at the moment of the call, not a fixed
denominator: it drifts while capture runs, and `newestId` is how you see that it moved.

Example arguments:

    { "host": "api.example.com", "method": "POST", "inScope": true, "limit": 50 }

### history_get

Full request and response of one flow.

Arguments:

- `id` (integer, required).
- `view` (string): `auto` (default), `wire` or `extension`. Selects which versions come
  back. See Message versions below.
- `side` (string): `both` (default), `request` or `response`. The side you do not ask for
  is absent from the reply, not empty.
- `part` (string): `all` (default), `headers` or `body`.
- `offset` (integer): byte offset into the body. Requires a single `side`, and is not
  allowed with `part=headers`.
- `maxBytes` (integer): body bytes per side. Default 32768, maximum 1048576 (8192 for
  base64).
- `encoding` (string): `text` (default) or `base64`. Base64 returns the body's real
  bytes and requires `part=body`.

Returns: `{ summary, view, side, part, request, response }`, where `request` and
`response` are the text as it crossed the wire (Content-Encoding undone), cut to what you
asked for. `streaming` and `streamingNote` appear when the response is a provisional
preview, and `wsMessages` and `wsNote` when the flow carried WebSocket messages.

A body larger than `maxBytes` comes back cut, with a receipt in `requestSlice` or
`responseSlice`. The field to check is `complete`:

    { "summary": { "id": 4165, "status": 500, "length": 2097152, ... },
      "view": "wire", "side": "response", "part": "body",
      "response": "{\"error\":\"...",
      "responseSlice": {
        "part": "body",
        "unit": "bytes of the decoded body (Content-Encoding undone), rendered as lossy UTF-8",
        "offset": 0, "returned": 32768, "bodyBytes": 2097152,
        "complete": false, "hasMore": true, "nextOffset": 32768 } }

Other receipt fields, each present only when it applies: `bodyBytes` (absent under gzip,
br or zstd, where the decoded length cannot be known without decompressing everything),
`capturedTruncated` with `captureNote` (capture itself kept only the start, so a read can
be `complete` and still be less than what crossed the wire), `binary` with `mime` (the
body was not returned as text), `bodyIncluded: false`, and `noResponse` (the request never
got one).

Nothing is appended to the message text when it is partial, so a sliced body can still be
diffed or hashed. See Reading only part of a message above.

With `view=auto`, any version that differs from the wire is added:

    { "summary": { ... },
      "view": "wire",
      "request": "POST /api/pay HTTP/1.1\n...\n\n{\"cherry\":\"2EF18CB5...\"}",
      "response": "HTTP/1.1 200 OK\n...",
      "versions": { "requestExtension": "POST /api/pay HTTP/1.1\n...\n\n{\"amount\":1}" },
      "versionsNote": "..." }

`versions` and `versionsNote` are absent when there is nothing to add, which is the
normal case. See Message versions below.

`view=wire` returns the wire alone, never a `versions` key. `view=extension` puts the
readable form in `request` / `response` and adds `requestDecoded` / `responseDecoded`
booleans; when no decode applies it falls back to the wire and the flag is false.

### history_search

Full-text search across request and response bodies, and WebSocket messages.

Arguments: `query` (string, required); `limit` (integer, 1 to 1000, default 100);
`beforeId` (integer): the paging cursor, exactly as for `history_list`.

Returns: `{ matches, returned, hasMore, flows: [summary, ...] }`, plus `nextBeforeId`
when `hasMore` is true.

Two more keys appear when they apply:

- `wsMatches` (plus `wsNote`): flow ids that matched inside a WebSocket message rather
  than in the HTTP exchange. Read those with `ws_messages`, passing the same text as
  `contains`, or `history_get` will show you a bare 101 handshake.
- `note`, on an empty result only: the index holds just the first 256 KB of each decoded
  body, skips binary bodies, and matches whole tokens for queries of 3 characters or
  more. **No match is not proof of absence.** Confirm with `history_get` on the specific
  flow before reporting that something does not occur.

### history_set_note

Set (or clear) the note on a captured flow. The note shows in HTTP History and is
saved with the project. Open History views update live.

Arguments: `id` (integer, required); `note` (string, required; empty clears it).

Returns: `{ id, note, color }`.

### history_set_color

Set (or clear) the highlight colour on a captured flow. The colour tints the row in HTTP
History and is saved with the project. Open History views update live.

Arguments: `id` (integer, required); `color` (string, required). Allowed values:
`red`, `orange`, `yellow`, `green`, `cyan`, `blue`, `purple`, `pink`, `gray`; an empty
string clears it.

Returns: `{ id, note, color }`.

### history_get_color

List the highlight colours that `history_set_color` accepts (call this first to discover
valid values).

Arguments: `id` (integer, optional). With an `id`, also returns that flow's current colour.

Returns: `{ colors }`, or `{ id, color, colors }` when an `id` is given.

### ws_messages

One flow's WebSocket messages, oldest first.

Arguments: `flowId` (integer, required); `afterId` (cursor, exclusive); `direction`
(`both` default, `client`, `server`); `contains` (text filter); `limit` (default 50,
max 500); `maxBytes` (payload bytes per message, default 2048, max 65536).

Returns:

    { "flowId": 7, "total": 1284, "client": 640, "server": 644, "totalBytes": 3932160,
      "returned": 50, "hasMore": true, "nextAfterId": 414,
      "messages": [
        { "id": 412, "ts": 1731400000123, "direction": "client", "opcode": 1,
          "type": "text", "length": 68, "stored": 68, "returned": 68,
          "renderedAs": "text", "complete": true,
          "payload": "{\"action\":\"subscribe\"}" }
      ] }

Three caps bound a reply, and the tightest one wins: `limit`, `maxBytes`, and a 64 KB
budget on the total payload text. The budget overrides the other two, because `limit` and
`maxBytes` at their ceilings would otherwise permit a 32 MB reply; `budgetReached` says
when it was the budget that stopped the page. A single message larger than the whole
budget still comes back, alone, so paging always makes progress.

A payload that renders as hex is cut to 256 bytes, since hex runs to about five characters
per byte. `contains` is answered from the message index, which covers only text and binary
messages and only their first 256 KB, so it cannot find a needle in a ping payload or
past that point.

An unknown `flowId` is an error. A flow that exists but carried no WebSocket messages is
not: it returns `total: 0` and says so.

### proxy_status

Whether the proxy is capturing, and when it is not, why not.

Arguments: none.

Returns:

    { "listening": true,
      "listeners": [ { "port": 8080, "exposure": "local" } ],
      "flows": 1284, "wsMessages": 96,
      "newestFlowTs": 1731400000123, "newestFlowAgeMs": 4120,
      "intercept": { "requests": false, "responses": false, "pending": 0,
                     "websocket": false, "websocketPending": 0 },
      "invisible": false, "scopePatterns": 2,
      "extensions": { "enabled": true, "loaded": 3, "running": 3, "failed": 0 },
      "note": "Read-only. ..." }

Call this when `history_list` comes back empty. "Nothing captured" has several causes and
only this tool separates them: nothing bound at all, bound but the client is not pointed
at the proxy, the CA not trusted so HTTPS fails before anything is recorded, or intercept
holding traffic while it waits for the operator. When `flows` is 0 the reply carries a
`hints` list naming them, so an empty history is not mistaken for an application that
makes no HTTP requests.

Read-only by construction: this server is handed no way to start, stop or rebind a
listener. Ask the operator for that.

Each listener reports its `port` and its `exposure` (`local`, `lan`, `tunnel`, `all`) and
never its bind address. The reach is what you can act on; the address is an internal fact
about the operator's machine or a client's network, and it is not sent. `exposure: "all"`
means every interface, including ones that appear after the listener started.

`extensions` is counts, not detail, because the full status carries interpreter and
virtualenv paths.

### scope_get

Get the target scope host patterns (e.g. `example.com`, `*.example.com`).

Arguments: none.

Returns: `{ patterns: [string, ...], allInScope }`. `allInScope` is true when the
scope is empty, which means every host is in scope.

### scope_set

Replace the target scope. Takes effect immediately (proxy, intercept and passive
in-scope checks) and is saved with the project.

Arguments: `patterns` (array of host strings, required). Each is `example.com` or
`*.example.com`; they are trimmed, lowercased and de-duplicated. An empty list
clears the scope, which means everything is in scope.

Returns: `{ patterns, allInScope }` with the normalized patterns actually stored.

### scope_check

Check whether a host is in the target scope.

Arguments: `host` (string, required; a bare host, a port is ignored).

Returns: `{ host, inScope, matched: [pattern, ...], allInScope }`.

### repeater_send

Send one request and record it.

Arguments: `raw` plus `target`, or `flowId` (see Sourcing a request); `tab`
(integer, default 9001) chooses the Repeater tab history to append to.

Returns: `{ tab, status, length, durationMs, response }`, where `response` is the
first 8 KB of the raw response text. Use `history_get` for the full response.

When a repeater-scope extension transformed the exchange, two more keys appear:
`requestSent` (what actually left the proxy, which is not the `raw` you passed) and
`responseWire` (what arrived, before the extension made `response` readable), plus an
`extensionNote` saying so. See Message versions below.

### repeater_send_batch

Send the same request many times. This is the tool for race conditions, such as
beating a duplicate check.

Arguments: `raw` plus `target`, or `flowId`; `count` (integer, 1 to 500, default
10); `mode` (string, `parallel` default or `sequential`); `tab` (integer, default
9001).

Returns:

    { "sent": 30, "mode": "parallel",
      "byStatus": { "201": 4, "409": 26 },
      "results": [ { "i": 0, "status": 201, "length": 12, "durationMs": 40 } ] }

A failed send appears as `{ "i": <n>, "error": "...", "elapsedMs": <n> }`. Use
`mode: "parallel"` to fire everything at once for a race, and `mode: "sequential"`
as a control that should not trigger the race.

### repeater_groups

List Repeater tab groups and their tabs. No arguments. Returns the groups, each
with its tabs' id, name and target.

### repeater_send_group

Send every tab in a named group.

Arguments: `group` (string, required); `mode` (string, `parallel` default or
`sequential`).

Returns: `{ group, sent, results: [ { tab, flowId, status, length, durationMs } or
{ tab, error } ] }`. Each `flowId` is the History flow that send was recorded as, for
reading the full exchange with `history_get`.

### intruder_run

Run an Intruder attack (see Intruder positions and attack types).

Arguments: `raw` plus `target`, or `flowId`; `type` (string: `sniper` default,
`battering`, `pitchfork` or `cluster`); `payloads` (a list of strings, or a list
of lists); `concurrency` (integer, 1 to 50, default 10).

Returns:

    { "sent": 4, "byStatus": { "200": 1, "404": 3 },
      "results": [ { "n": 0, "payloads": ["1"], "status": 200, "length": 55, "timeMs": 12 } ] }

A failed request appears as `{ "n": <n>, "payloads": [...], "error": "...",
"timeMs": <n> }`.

Example arguments (sniper over one position):

    {
      "target": "https://example.com",
      "raw": "GET /user?id=§1§ HTTP/1.1\nHost: example.com\n\n",
      "type": "sniper",
      "payloads": ["1", "2", "3", "9999"],
      "concurrency": 10
    }

### passive_scan

Run the passive scan now over all in-scope captured flows and store the findings.
No arguments.

Returns: `{ scannedFlows, findings, bySeverity: { high, medium, low, info } }`.

### passive_findings

List findings from the last scan.

Arguments (all optional): `severity` (string): one of `high`, `medium`, `low`, `info`;
`limit` (integer, 1 to 2000, default 200); `offset` (integer): the paging index.

Returns: `{ count, offset, returned, hasMore, findings: [ { type, title, severity, where,
match, flowId, method, host, path, status } ] }`, plus `nextOffset` when `hasMore` is
true.

An index is right here, unlike the flow lists, because this array is only ever replaced
wholesale by `passive_scan`; nothing is inserted into the middle of it between calls.
`count` is every finding matching `severity`, not just the page.

### passive_rules_get

Get all passive rules, built-in and custom. No arguments.

### passive_rules_set

Replace the custom rules (built-in rules are not affected).

Arguments: `rules` (array, required). Each rule is `{ id, title, severity, pattern,
flags, group }`, where `pattern` is a regular expression string, `flags` may
include `i` for case insensitive, and `group` is the capture group number to
report (0 for the whole match).

### organizer_list

List collections and their saved requests, without the request/response text.

Arguments: `collection` (string, optional; an id or a name) to list just that one.

Returns: `{ activeCollection, colors, collections }`. `activeCollection` is
`{ id, name }` or null. Each collection is `{ id, name, active, count, items }` and each
item is `{ id, n, name, method, host, path, status, label, note, color, source, savedAt }`,
where `n` is its position in the collection.

### organizer_get

One saved request with its full text.

Arguments: `id` (string, required).

Returns: the item fields plus `target`, `request`, `response` and
`collection: { id, name, active }`.

### organizer_save

Save a request into the Organizer.

Arguments: either `flowId` (integer; a captured flow, whose note and colour come
along) or `request` (raw HTTP request text) plus `target` (`https://host[:port]`), with
optional `response` (raw response text) and `status`. Optional for both: `collection`
(an id or a name; an unknown name creates that collection), `name`, `label`, `note` and
`color` (as for `organizer_set`). Without `collection` the item goes to the active
collection, else to "Saved" (created if missing, then made active).

Returns: `{ collection: { id, name, active }, item }`, with the new item's `id`.

The item's `source` is `mcp`, which is how the window's Source column distinguishes a
save this server made from one the operator made by hand. To record a FINDING rather than
a request, use `findings_add`: it writes `source: "mcp-finding"` and an attribution header
you cannot switch off.

### organizer_set

Change a saved request's label, note and/or colour.

Arguments: `id` (string, required); `label`, `note`, `color` (strings, optional; give
at least one). An omitted field is kept and an empty string clears it. Colours: `red`,
`orange`, `yellow`, `green`, `cyan`, `blue`, `purple`, `pink`, `gray`.

Returns: `{ collection, item }` as for `organizer_save`.

### findings_add

Record a finding you are claiming, where the operator will actually see it.

Arguments: `title` (required, the claim in a line); `severity` (required: `high`,
`medium`, `low`, `info`); `detail` (required: what it is, what it allows, how it was
shown); `confidence` (`certain`, `firm`, `tentative`, default `tentative`); `flowId` (the
evidence flow, whose exchange is stored with the item); `citedFlowIds` (other flows named
in the finding); `collection` (default `Findings`).

The finding lands as an item in the Organizer, which is a backend-owned, event-notified
surface, so it appears in an open window immediately rather than waiting for a reload.

**The attribution is not yours to set.** This server writes, and you cannot override:

- `source: "mcp-finding"`, which shows in the Organizer's Source column, and which is a
  different value from the `mcp` a saved request gets. An agent's claim and an agent's
  saved request must not look alike.
- a label prefixed `[agent <severity>]`, which also makes findings sort together by
  severity in the existing table.
- a note header reading `[AGENT FINDING - claimed by an LLM over MCP, not verified by
  Ngintip]`, followed by the severity, the confidence and the cited flows.

The header exists because the note is the part that gets copied into a report, where a
confusion about who claimed what does real damage. Ngintip's own passive scanner never
writes here.

The colour is set from the severity (high red, medium orange, low yellow, info gray).

If a cited flow is on a host outside the target scope the finding is still recorded, and
the reply carries a `scopeWarning`. Reading the capture is unrestricted; a finding is work
product, and one about a host nobody authorised is not deliverable.

The stored copy of the exchange is the viewer-capped text, not the full body. The
`flowId` in the note is the authoritative pointer; use `history_get` for the bytes.

### vault_list

List Vault credentials with secret values hidden.

Arguments: `query` (string, optional). It matches the label, role, note, field names
and non-secret values; a secret value never matches.

Returns: `{ total, returned, entries }`. Each entry is
`{ id, label, role, note, fields }` and each field `{ name, kind, secret, value }`; a
secret value reads `[hidden, use vault_get]`.

### vault_get

One Vault credential with every value, secrets included.

Arguments: `id` (string) or `label` (string, exact, case-insensitive); one is
required. A label shared by several entries is an error; use the id instead.

Returns: `{ id, label, role, note, fields: [{ name, kind, secret, value }] }`.

### extensions_status

State of the Python extension engine.

Arguments: none.

Returns: `{ enabled, python, venv, venvReady, extensions: [ { id, name, path,
enabled, running, error, requirements }, ... ] }`. Extensions transform requests
and responses on the wire (for example to decrypt an encrypted body), and each
runs as a Python worker.

### extensions_enable

Turn the whole extension engine on or off.

Arguments: `enabled` (boolean, required).

Returns: the same shape as `extensions_status`.

### extension_toggle

Enable or disable a single extension by id.

Arguments: `id` (string, required, from `extensions_status`); `enabled` (boolean,
required).

Returns: the same shape as `extensions_status`.

### extensions_logs

Recent lines from the extension debug console (worker lifecycle and script stderr).

Arguments: none.

Returns: `{ logs: [string, ...] }`.

## Errors

A tool that cannot run returns `isError: true` with a message, for example a
missing flow id, an unparseable request, a payload count over the cap, or a group
with no tabs. Protocol level problems (an unknown method or bad params) come back
in the JSON-RPC `error` field with a numeric code.

## Notes

- The Intruder marker is the single character U+00A7 (`§`), used in pairs.
- Response previews are truncated to 8 KB; use `history_get` for full text.
- Every tool result is a JSON string inside the tool text content; parse it.
"####;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::{test_ctx, test_ctx_with_log};

    fn ctx() -> Arc<Ctx> {
        let dir = std::env::temp_dir().join(format!("mcp-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(test_ctx(&dir))
    }
    /// Parse a tool's output. Tool output is compact JSON, so a test reads the value it
    /// cares about instead of matching how the serializer spaced it.
    fn jparse(s: &str) -> Value {
        serde_json::from_str(s).unwrap_or_else(|e| panic!("tool output is not JSON ({e}): {s}"))
    }

    fn rand_suffix() -> String {
        // Nanos alone can collide when tests run in parallel (same instant), which
        // would share a store dir; a per-process counter keeps each ctx() isolated.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!("{nanos}-{}", N.fetch_add(1, Ordering::Relaxed))
    }

    #[tokio::test]
    async fn initialize_and_tools_list() {
        let c = ctx();
        let init = handle_rpc(&c, json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        let list = handle_rpc(&c, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await.unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"history_list"));
        assert!(names.contains(&"repeater_send_batch"));
        assert!(names.contains(&"intruder_run"));
        assert!(names.contains(&"extensions_status"));
        assert!(names.contains(&"extension_toggle"));
        for tool in ["organizer_list", "organizer_get", "organizer_save", "organizer_set", "vault_list", "vault_get"] {
            assert!(names.contains(&tool), "{tool} listed");
        }
        // A notification produces no response.
        assert!(handle_rpc(&c, json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.is_none());
    }

    #[tokio::test]
    async fn extension_tools_report_and_toggle() {
        let c = ctx();
        // No extensions loaded yet: engine off, empty list.
        let s = call_tool(&c, "extensions_status", &json!({})).await.unwrap();
        assert_eq!(jparse(&s)["enabled"], false, "{s}");
        // Turning the engine on flips the flag.
        let s = call_tool(&c, "extensions_enable", &json!({"enabled": true})).await.unwrap();
        assert_eq!(jparse(&s)["enabled"], true, "{s}");
        // Toggling a missing extension is an error.
        assert!(call_tool(&c, "extension_toggle", &json!({"id":"nope","enabled":true})).await.is_err());
        // Logs come back as a JSON array.
        assert!(call_tool(&c, "extensions_logs", &json!({})).await.unwrap().contains("logs"));
    }

    #[tokio::test]
    async fn initialize_negotiates_the_protocol_version() {
        let c = ctx();
        // Default advertised revision is current, not the oldest.
        assert_eq!(PROTOCOL_VERSION, "2025-06-18");
        // A supported revision the client asks for is echoed back.
        let init = handle_rpc(&c, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        // An older but supported one is honored too (compatibility).
        let old = handle_rpc(&c, json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2024-11-05"}})).await.unwrap();
        assert_eq!(old["result"]["protocolVersion"], "2024-11-05");
        // An unknown revision falls back to our default.
        let unknown = handle_rpc(&c, json!({"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"1999-01-01"}})).await.unwrap();
        assert_eq!(unknown["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn history_tools_read_flows() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};
        let c = ctx();
        let uri: Uri = "https://example.com/secret".parse().unwrap();
        let mut flow = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        flow.set_response(StatusCode::OK, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::from_static(b"token eyJ"));
        let id = c.store.push(flow).unwrap().id;

        let out = call_tool(&c, "history_list", &json!({"host":"example"})).await.unwrap();
        assert_eq!(jparse(&out)["total"], 1, "{out}");
        let got = call_tool(&c, "history_get", &json!({"id": id})).await.unwrap();
        assert!(got.contains("GET https://example.com/secret") || got.contains("GET /secret"), "{got}");

        // Unknown tool -> error content.
        let err = call_tool_rpc(&c, &json!({"name":"nope","arguments":{}})).await.unwrap();
        assert_eq!(err["isError"], true);
    }

    #[tokio::test]
    async fn set_note_and_scope_tools() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, Uri, Version};
        let c = ctx();
        let mk = |host: &str| {
            let uri: Uri = format!("https://{host}/a").parse().unwrap();
            let flow = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
            c.store.push(flow).unwrap().id
        };
        let in_id = mk("in.example.com");
        let out_id = mk("out.other.com");

        // Set a note, and it round-trips through the store.
        call_tool(&c, "history_set_note", &json!({"id": in_id, "note": "check IDOR"})).await.unwrap();
        assert_eq!(c.store.get(in_id).unwrap().unwrap().summary.note, "check IDOR");
        // Clearing works, and an unknown id errors.
        call_tool(&c, "history_set_note", &json!({"id": in_id, "note": ""})).await.unwrap();
        assert_eq!(c.store.get(in_id).unwrap().unwrap().summary.note, "");
        assert!(call_tool(&c, "history_set_note", &json!({"id": 99999, "note": "x"})).await.is_err());

        // Set a highlight colour; it round-trips and preserves the note.
        call_tool(&c, "history_set_note", &json!({"id": in_id, "note": "keep"})).await.unwrap();
        call_tool(&c, "history_set_color", &json!({"id": in_id, "color": "red"})).await.unwrap();
        let f = c.store.get(in_id).unwrap().unwrap();
        assert_eq!((f.summary.color.as_str(), f.summary.note.as_str()), ("red", "keep"), "colour set, note preserved");
        // Clearing the colour keeps the note; an invalid colour errors.
        call_tool(&c, "history_set_color", &json!({"id": in_id, "color": ""})).await.unwrap();
        assert_eq!(c.store.get(in_id).unwrap().unwrap().summary.color, "");
        assert!(call_tool(&c, "history_set_color", &json!({"id": in_id, "color": "chartreuse"})).await.is_err(), "invalid colour rejected");
        call_tool(&c, "history_set_note", &json!({"id": in_id, "note": ""})).await.unwrap();

        // Discover the palette; with an id, also the flow's current colour.
        let avail = call_tool(&c, "history_get_color", &json!({})).await.unwrap();
        assert!(avail.contains("\"red\"") && avail.contains("\"gray\""), "{avail}");
        call_tool(&c, "history_set_color", &json!({"id": in_id, "color": "blue"})).await.unwrap();
        let cur = call_tool(&c, "history_get_color", &json!({"id": in_id})).await.unwrap();
        assert_eq!(jparse(&cur)["color"], "blue", "{cur}");
        assert!(jparse(&cur)["colors"].is_array(), "{cur}");
        call_tool(&c, "history_set_color", &json!({"id": in_id, "color": ""})).await.unwrap();

        // Scope set (normalizes + persists), then get/check.
        let set = call_tool(&c, "scope_set", &json!({"patterns": [" *.Example.com ", "", "*.example.com"]})).await.unwrap();
        assert!(set.contains("*.example.com"), "{set}");
        assert!(!set.contains("Example"), "normalized (lowercased/deduped): {set}");
        assert_eq!(c.scope.get(), vec!["*.example.com"], "applied to the live scope");
        assert_eq!(c.store.get_list(crate::commands::SCOPE_KEY).unwrap(), vec!["*.example.com"], "persisted to the project");
        assert!(call_tool(&c, "scope_set", &json!({})).await.is_err(), "patterns required");

        let got = call_tool(&c, "scope_get", &json!({})).await.unwrap();
        assert!(got.contains("*.example.com"), "{got}");
        assert_eq!(jparse(&got)["allInScope"], false, "{got}");
        let chk = call_tool(&c, "scope_check", &json!({"host": "in.example.com"})).await.unwrap();
        assert_eq!(jparse(&chk)["inScope"], true, "{chk}");
        let chk2 = call_tool(&c, "scope_check", &json!({"host": "out.other.com"})).await.unwrap();
        assert_eq!(jparse(&chk2)["inScope"], false, "{chk2}");
        assert!(call_tool(&c, "scope_check", &json!({})).await.is_err(), "host required");

        // history_list inScope filter (default off = both, on = only in-scope).
        let both = call_tool(&c, "history_list", &json!({})).await.unwrap();
        assert_eq!(jparse(&both)["total"], 2, "{both}");
        let scoped = call_tool(&c, "history_list", &json!({"inScope": true})).await.unwrap();
        assert_eq!(jparse(&scoped)["total"], 1, "only in-scope: {scoped}");
        let _ = out_id;
    }

    /// The window only learns about an MCP write through an event, so the channel name
    /// and the payload shape are a contract across the Rust/JS boundary. Both sides are
    /// otherwise tested with doubles, which is exactly the situation where a renamed
    /// channel passes every test and does nothing in the running app. The literals here
    /// are the ones `store.svelte.js` listens on and reads.
    #[tokio::test]
    async fn mcp_writes_announce_themselves_on_the_channels_the_window_listens_on() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};

        let dir = std::env::temp_dir().join(format!("mcp-ev-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let (c, events) = test_ctx_with_log(&dir);
        let c = std::sync::Arc::new(c);

        let uri: Uri = "https://example.com/a".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("access-control-allow-origin", "*".parse().unwrap());
        let mut flow = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        flow.set_response(StatusCode::OK, Version::HTTP_11, &h, bytes::Bytes::from_static(b"x"));
        c.store.push(flow).unwrap();

        call_tool(&c, "passive_scan", &json!({})).await.unwrap();
        call_tool(&c, "scope_set", &json!({ "patterns": ["example.com"] })).await.unwrap();
        call_tool(&c, "passive_rules_set", &json!({ "rules": [] })).await.unwrap();

        let log = events.lock().unwrap().clone();
        let named = |n: &str| log.iter().find(|(e, _)| e == n).map(|(_, p)| p.clone());

        // store.svelte.js: listen('passive', ...) and reads payload.findings
        let passive = named("passive").expect("no event on the 'passive' channel");
        assert!(passive.get("findings").and_then(|v| v.as_u64()).is_some(), "payload has no numeric `findings`: {passive}");

        // store.svelte.js: listen('setting', ...) and switches on payload.key
        let keys: Vec<String> = log
            .iter()
            .filter(|(e, _)| e == "setting")
            .filter_map(|(_, p)| p.get("key").and_then(|v| v.as_str()).map(String::from))
            .collect();
        assert!(keys.contains(&"scope".to_string()), "scope_set announced no `scope` key: {keys:?}");
        assert!(keys.contains(&"passiveRules".to_string()), "passive_rules_set announced no `passiveRules` key: {keys:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn passive_scan_and_rules_roundtrip() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};
        let c = ctx();
        let uri: Uri = "https://example.com/a".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("access-control-allow-origin", "*".parse().unwrap());
        let mut flow = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        flow.set_response(StatusCode::OK, Version::HTTP_11, &h, bytes::Bytes::from_static(b"contact admin@example.com"));
        c.store.push(flow).unwrap();

        let scan = call_tool(&c, "passive_scan", &json!({})).await.unwrap();
        assert_eq!(jparse(&scan)["scannedFlows"], 1, "{scan}");
        let found = call_tool(&c, "passive_findings", &json!({})).await.unwrap();
        assert!(found.contains("email"), "{found}");
        assert!(found.contains("cors"), "{found}");

        // Custom rules persist.
        call_tool(&c, "passive_rules_set", &json!({"rules":[{"id":"x","title":"X","severity":"low","pattern":"foo","flags":""}]})).await.unwrap();
        let rules = call_tool(&c, "passive_rules_get", &json!({})).await.unwrap();
        assert!(rules.contains("\"pattern\":\"foo\""), "{rules}");
    }

    #[tokio::test]
    async fn passive_scan_honors_in_scope_only() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};
        let c = ctx();
        for host in ["in.example.com", "out.other.com"] {
            let uri: Uri = format!("https://{host}/a").parse().unwrap();
            let mut flow = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
            flow.set_response(StatusCode::OK, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::from_static(b"reach a@b.com"));
            c.store.push(flow).unwrap();
        }
        // Without the setting, both hosts are scanned.
        let all = call_tool(&c, "passive_scan", &json!({})).await.unwrap();
        assert_eq!(jparse(&all)["scannedFlows"], 2, "{all}");
        // Scope to example.com and restrict to in-scope only.
        c.scope.set(vec!["*.example.com".to_string()]);
        c.store.put_setting("passiveInScope", &json!(true)).unwrap();
        let scoped = call_tool(&c, "passive_scan", &json!({})).await.unwrap();
        assert_eq!(jparse(&scoped)["scannedFlows"], 1, "only the in-scope host: {scoped}");
    }

    #[test]
    fn all_builtin_rules_compile() {
        for (id, _title, _sev, pat, _ci, group) in BUILTIN_RULES {
            let re = regex::Regex::new(pat).unwrap_or_else(|e| panic!("rule {id} did not compile: {e}"));
            // The reported capture group must exist in the pattern.
            assert!(*group <= re.captures_len().saturating_sub(1) + 1, "rule {id}: group {group} out of range");
        }
    }

    #[test]
    fn email_post_filter_drops_false_positives() {
        // Real address is kept.
        assert!(plausible_email("contact john@example.com now", 8, "john@example.com"));
        // URL userinfo (preceded by '/') and dotted/token prefixes are rejected.
        assert!(!plausible_email("https://key@cdn.example.com/x", 8, "key@cdn.example.com"));
        assert!(!plausible_email("a.support@site.com", 2, "support@site.com"));
        // Code/asset extensions that only look like a TLD are rejected.
        assert!(!plausible_email("'main@bundle.js'", 1, "main@bundle.js"));
        assert!(!plausible_email("x theme@dark.css", 2, "theme@dark.css"));
        // A real TLD after a filename-ish label still passes.
        assert!(plausible_email("mail dev@team.dev", 5, "dev@team.dev"));
    }

    #[test]
    fn new_secret_rules_match_samples() {
        let re = |id: &str| {
            let (_, _, _, pat, _, _) = BUILTIN_RULES.iter().find(|r| r.0 == id).unwrap();
            regex::Regex::new(pat).unwrap()
        };
        assert!(re("vite-exposed-secret").is_match(r#"VITE_API_KEY = "sk_live_abcd1234efgh""#));
        assert!(re("azure-storage-key").is_match(&format!("AccountKey={}==", "A".repeat(86))));
        assert!(re("gcp-service-account").is_match(r#"{ "type": "service_account" }"#));
        assert!(re("google-oauth-client-secret").is_match(&format!("GOCSPX-{}", "a".repeat(28))));
        assert!(re("gitlab-pat").is_match(&format!("glpat-{}", "x".repeat(20))));
        assert!(re("digitalocean-token").is_match(&format!("dop_v1_{}", "a".repeat(64))));
        // The newly added provider tokens match in the Rust scanner too.
        assert!(re("slack-app-token").is_match("xapp-1-A012BC-1112131415-abcdef012345"));
        assert!(re("telegram-bot-token").is_match(&format!("123456789:AA{}", "a".repeat(33))));
        assert!(re("discord-bot-token").is_match(&format!("M{}.{}.{}", "a".repeat(23), "b".repeat(6), "c".repeat(27))));
        assert!(re("square-access-token").is_match(&format!("sq0atp-{}", "a".repeat(22))));
        assert!(re("pypi-token").is_match(&format!("pypi-AgEIcHlwaS5vcmc{}", "a".repeat(50))));
        assert!(re("postman-key").is_match(&format!("PMAK-{}-{}", "a".repeat(24), "b".repeat(34))));
        assert!(re("linear-key").is_match(&format!("lin_api_{}", "a".repeat(40))));
        assert!(re("databricks-token").is_match(&format!("dapi{}", "a".repeat(32))));
        assert!(re("figma-token").is_match(&format!("figd_{}", "a".repeat(40))));
        assert!(re("dynatrace-token").is_match(&format!("dt0c01.{}.{}", "A".repeat(24), "A".repeat(64))));
        assert!(re("vault-token").is_match(&format!("hvs.{}", "a".repeat(24))));
        assert!(re("cloudinary-url").is_match("cloudinary://123456789012:abcDEF_secret@mycloud"));
        assert!(re("age-secret-key").is_match(&format!("AGE-SECRET-KEY-1{}", "A".repeat(58))));
        // Ordinary code doesn't trip the VITE rule (no quoted value assigned).
        assert!(!re("vite-exposed-secret").is_match(r#"const VITE_MODE = mode;"#));
        // Prose mentioning products doesn't trip the token rules.
        assert!(!re("vault-token").is_match("the vault was empty"));
        assert!(!re("cloudinary-url").is_match("cloudinary integration guide"));
    }

    #[test]
    fn email_regex_rejects_garbage_local_parts() {
        let (_, _, _, pat, _, _) = BUILTIN_RULES.iter().find(|r| r.0 == "email").unwrap();
        let re = regex::Regex::new(pat).unwrap();
        // The real scan is regex + plausible_email (the regex alone can't do the
        // JS lookbehind), so test the whole path as passive_scan runs it.
        let hit = |text: &str| re.find_iter(text).any(|m| plausible_email(text, m.start(), m.as_str()));
        // Consecutive/leading/trailing dots in the local part are not real emails.
        assert!(!hit("6..@qA.AR")); // the reported false positive
        assert!(!hit("a..b@x.com"));
        assert!(!hit(".a@x.com"));
        // Genuine addresses still match.
        assert!(hit("john.doe@example.com"));
        assert!(hit("jane.doe+tag@corp.io"));
        assert!(hit("contact admin@example.com now"));
    }

    #[test]
    fn docs_are_detailed_and_use_plain_dashes() {
        let d = docs();
        // Only plain hyphens: no em dash or en dash anywhere.
        assert!(!d.contains('\u{2014}'), "docs contain an em dash");
        assert!(!d.contains('\u{2013}'), "docs contain an en dash");
        // Every tool is documented, and so is every argument it accepts. Checking only
        // the name let a real gap through once: six tools gained arguments that the
        // schema advertised and this page did not mention, so a reader of the docs would
        // never have passed them.
        for t in tool_defs().as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            let section = d
                .split("\n### ")
                .find(|s| s.starts_with(name) && s[name.len()..].starts_with('\n'))
                .unwrap_or_else(|| panic!("docs missing a '### {name}' section"));
            // Where a reader actually looks for an argument: the paragraph beginning
            // "Arguments" plus the section's bullet lines. Searching the whole section
            // has no teeth, which this test proved on itself: a cursor argument removed
            // from the argument list still "passed" on a passing mention of it in the
            // prose two paragraphs down.
            let mut spec = String::new();
            let mut in_args = false;
            for line in section.lines() {
                if line.starts_with("Arguments") {
                    in_args = true;
                } else if in_args && line.trim().is_empty() {
                    in_args = false;
                }
                if in_args || line.starts_with("- `") {
                    spec.push_str(line);
                    spec.push('\n');
                }
            }
            for arg in t["inputSchema"]["properties"].as_object().unwrap().keys() {
                assert!(
                    spec.contains(&format!("`{arg}`")),
                    "the docs for {name} never list its `{arg}` argument (found: {spec})"
                );
            }
        }
        // Detailed, with the key concepts covered.
        assert!(d.len() > 4000, "docs should be detailed, got {} bytes", d.len());
        for kw in ["JSON-RPC", "tools/call", "flowId", "sniper", "passiveScope", "127.0.0.1", brand::NAME] {
            assert!(d.contains(kw), "docs missing {kw}");
        }
    }

    #[test]
    fn scope_filters_flows_by_extension_and_mime() {
        let set = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect::<std::collections::HashSet<String>>());
        let scope = set(&["js", "json", "html", "css"]);
        // Path extension in scope.
        assert!(flow_in_scope("/assets/main.abc123.js", "", &scope));
        assert!(flow_in_scope("/style.css?v=2", "", &scope));
        // Extension-less API path, but the content-type is in scope.
        assert!(flow_in_scope("/api/users", "json", &scope));
        assert!(flow_in_scope("/api/users", "application/json", &scope));
        // Out of scope: image + font.
        assert!(!flow_in_scope("/logo.png", "png", &scope));
        assert!(!flow_in_scope("/f.woff2", "font/woff2", &scope));
        // No scope, empty scope, or '*' → scan everything.
        assert!(flow_in_scope("/logo.png", "png", &None));
        assert!(flow_in_scope("/logo.png", "png", &set(&["*"])));
    }

    #[test]
    fn version_headers_outrank_bare_product_names() {
        assert!(has_version("nginx/1.25.3"));
        assert!(has_version("Apache/2.4.41 (Ubuntu)"));
        assert!(has_version("PHP/8.2"));
        assert!(has_version("4.0.30319"));
        assert!(!has_version("cloudflare"));
        assert!(!has_version("Varnish"));
        assert!(!has_version("AmazonS3")); // a trailing digit is not a version
    }

    #[tokio::test]
    async fn organizer_tools_save_list_set_get_and_notify_the_ui() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};
        let dir = crate::e2e::temp_dir("mcp-organizer");
        let (ctx, log) = crate::e2e::test_ctx_with_log(&dir);
        let c = Arc::new(ctx);
        let call = |name: &'static str, args: Value| {
            let c = c.clone();
            async move { call_tool(&c, name, &args).await }
        };
        let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap();

        // A captured flow with a note and a colour.
        let uri: Uri = "https://shop.test/api/orders/7".parse().unwrap();
        let mut flow = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        flow.set_response(StatusCode::OK, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::from_static(b"{\"order\":7}"));
        let fid = c.store.push(flow).unwrap().id;
        c.store.set_meta(fid, "owner check", "red").unwrap();

        // Saving the flow with nothing active lands in a new, active "Saved".
        let out = parse(call("organizer_save", json!({"flowId": fid, "label": "IDOR?"})).await.unwrap());
        assert_eq!((out["collection"]["name"].as_str(), out["collection"]["active"].as_bool()), (Some("Saved"), Some(true)));
        let item = &out["item"];
        assert_eq!((item["method"].as_str(), item["host"].as_str(), item["path"].as_str()), (Some("GET"), Some("shop.test"), Some("/api/orders/7")));
        assert_eq!(
            (item["status"].as_u64(), item["label"].as_str(), item["note"].as_str(), item["color"].as_str(), item["source"].as_str()),
            // "mcp", not "history": the Source column has to distinguish a save this
            // server made from one the operator made by hand in the window.
            (Some(200), Some("IDOR?"), Some("owner check"), Some("red"), Some("mcp"))
        );
        let id = item["id"].as_str().unwrap().to_string();
        // The UI was told about the new revision.
        assert!(log.lock().unwrap().iter().any(|(e, p)| e == "organizer" && p["rev"] == 1));

        // A raw request into a named collection, created on the fly (not made active).
        let raw = call("organizer_save", json!({"request": "POST /login HTTP/1.1\r\nHost: shop.test\r\n\r\nu=a", "target": "https://shop.test", "collection": "Auth", "color": "blue"})).await.unwrap();
        let rawv = jparse(&raw);
        assert_eq!(
            (rawv["collection"]["name"].as_str(), rawv["item"]["source"].as_str(), rawv["collection"]["active"].as_bool()),
            (Some("Auth"), Some("mcp"), Some(false)),
            "{raw}"
        );
        // Bad input errors and saves nothing.
        assert!(call("organizer_save", json!({"target": "https://x"})).await.is_err());
        assert!(call("organizer_save", json!({"flowId": fid, "color": "teal"})).await.is_err());
        assert!(call("organizer_save", json!({"flowId": 99999})).await.is_err());

        // List: both collections, the active one named, items without their text.
        let list = parse(call("organizer_list", json!({})).await.unwrap());
        assert_eq!(list["activeCollection"]["name"], "Saved");
        assert_eq!(list["collections"].as_array().unwrap().len(), 2);
        assert_eq!(list["collections"][0]["items"][0]["n"], 1);
        assert!(list["collections"][0]["items"][0].get("request").is_none());
        let only = parse(call("organizer_list", json!({"collection": "auth"})).await.unwrap());
        assert_eq!((only["collections"].as_array().unwrap().len(), only["collections"][0]["count"].as_u64()), (1, Some(1)));
        assert!(call("organizer_list", json!({"collection": "nope"})).await.is_err());

        // Set: an omitted label is kept, "" clears the colour, the note is replaced.
        let set = parse(call("organizer_set", json!({"id": id, "note": "confirmed", "color": ""})).await.unwrap());
        assert_eq!((set["item"]["label"].as_str(), set["item"]["note"].as_str(), set["item"]["color"].as_str()), (Some("IDOR?"), Some("confirmed"), Some("")));
        assert!(call("organizer_set", json!({"id": id})).await.is_err(), "nothing to set");
        assert!(call("organizer_set", json!({"id": id, "color": "teal"})).await.is_err());
        assert!(call("organizer_set", json!({"id": "nope", "label": "x"})).await.is_err());

        // Get: the full request and response text.
        let got = parse(call("organizer_get", json!({"id": id})).await.unwrap());
        assert!(got["request"].as_str().unwrap().starts_with("GET /api/orders/7"));
        assert!(got["response"].as_str().unwrap().contains("{\"order\":7}"));
        assert_eq!((got["collection"]["name"].as_str(), got["note"].as_str()), (Some("Saved"), Some("confirmed")));
        assert!(call("organizer_get", json!({"id": "nope"})).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A few flows with bodies, for the paging and slicing tests.
    fn seed_flows(c: &Arc<Ctx>, n: usize, body: &'static [u8]) -> Vec<u64> {
        use crate::store::Flow;
        use http::{HeaderMap, HeaderValue, Method as M, StatusCode, Uri, Version};
        let mut ids = Vec::new();
        for i in 0..n {
            let uri: Uri = format!("https://api.test/item/{i}").parse().unwrap();
            let mut h = HeaderMap::new();
            h.insert("content-length", HeaderValue::from_static("0"));
            let mut f = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &h, bytes::Bytes::new());
            let mut rh = HeaderMap::new();
            rh.insert("content-type", HeaderValue::from_static("application/json"));
            rh.insert("content-length", HeaderValue::from_str(&body.len().to_string()).unwrap());
            f.set_response(StatusCode::OK, Version::HTTP_11, &rh, bytes::Bytes::from_static(body));
            ids.push(c.store.push(f).unwrap().id);
        }
        ids
    }

    /// Paging a live capture by index silently re-reads and skips rows, so these lists
    /// page by id and refuse an index. The test walks a list while inserting, which is
    /// the case an index gets wrong.
    #[tokio::test]
    async fn history_lists_page_by_cursor_and_refuse_an_index() {
        let c = ctx();
        let ids = seed_flows(&c, 12, b"{}");

        // Walk the whole history in pages of 5, following the cursor.
        let mut seen: Vec<u64> = Vec::new();
        let mut before = 0u64;
        for _ in 0..10 {
            let args = if before == 0 { json!({"limit": 5}) } else { json!({"limit": 5, "beforeId": before}) };
            let page = jparse(&call_tool(&c, "history_list", &args).await.unwrap());
            for f in page["flows"].as_array().unwrap() {
                seen.push(f["id"].as_u64().unwrap());
            }
            if !page["hasMore"].as_bool().unwrap() {
                // Exhausted: no cursor is offered, so there is no loop to fall into.
                assert!(page.get("nextBeforeId").is_none(), "{page}");
                break;
            }
            before = page["nextBeforeId"].as_u64().expect("a cursor while more remain");
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(seen.len(), ids.len(), "every flow seen exactly once: {seen:?}");
        assert_eq!(sorted.len(), seen.len(), "no flow seen twice: {seen:?}");

        // Newest first, and a flow arriving mid-walk has a higher id, so it is simply
        // outside the walk instead of shifting a page boundary.
        let first = jparse(&call_tool(&c, "history_list", &json!({"limit": 5})).await.unwrap());
        let cursor = first["nextBeforeId"].as_u64().unwrap();
        seed_flows(&c, 3, b"{}");
        let next = jparse(&call_tool(&c, "history_list", &json!({"limit": 5, "beforeId": cursor})).await.unwrap());
        let next_ids: Vec<u64> = next["flows"].as_array().unwrap().iter().map(|f| f["id"].as_u64().unwrap()).collect();
        assert!(next_ids.iter().all(|id| *id <= cursor), "the walk does not jump to new arrivals: {next_ids:?}");
        assert!(next["newestId"].as_u64().unwrap() > cursor, "and it can see that capture moved");

        // An index is refused by name rather than ignored.
        let err = call_tool(&c, "history_list", &json!({"offset": 5})).await.unwrap_err();
        assert!(err.contains("beforeId"), "{err}");
        // A cursor sent as a string is refused too: falling back to the default here
        // would mean re-reading page one forever.
        let err = call_tool(&c, "history_list", &json!({"beforeId": "7"})).await;
        assert!(err.is_ok(), "an all-digit string is a near miss, not a different intent");
        let err = call_tool(&c, "history_list", &json!({"beforeId": "newest"})).await.unwrap_err();
        assert!(err.contains("whole number"), "{err}");

        // Search pages the same way, with the same words.
        let s = jparse(&call_tool(&c, "history_search", &json!({"query": "api.test", "limit": 4})).await.unwrap());
        assert_eq!(s["returned"], 4);
        assert!(s["hasMore"].as_bool().unwrap() && s["nextBeforeId"].is_number(), "{s}");
        // A search that finds nothing says why that is not proof of absence.
        let none = jparse(&call_tool(&c, "history_search", &json!({"query": "zzz-not-there"})).await.unwrap());
        assert_eq!(none["matches"], 0);
        assert!(none["note"].as_str().is_some_and(|n| n.contains("not proof of absence")), "{none}");
    }

    /// Reading a whole 2 MB body to answer a question about a header is the most
    /// expensive mistake this surface used to force.
    #[tokio::test]
    async fn history_get_reads_only_the_part_asked_for() {
        let c = ctx();
        // 40 KB of body, over the default 32 KB cap.
        static BIG: &[u8] = &[b'x'; 40 * 1024];
        let id = seed_flows(&c, 1, BIG)[0];

        // Default: one side is capped, and the receipt says so and how to continue.
        let d = jparse(&call_tool(&c, "history_get", &json!({"id": id})).await.unwrap());
        let r = &d["responseSlice"];
        assert_eq!(r["complete"], false, "{d}");
        assert_eq!(r["hasMore"], true);
        assert_eq!(r["bodyBytes"], 40 * 1024);
        assert_eq!(r["returned"], 32 * 1024);
        assert!(r["unit"].as_str().unwrap().contains("bytes"), "{r}");

        // Paging the body reassembles it exactly, and nothing but the body comes back.
        let mut body = String::new();
        let mut at = 0u64;
        loop {
            let p = jparse(
                &call_tool(&c, "history_get", &json!({"id": id, "side": "response", "part": "body", "offset": at, "maxBytes": 9000}))
                    .await
                    .unwrap(),
            );
            body.push_str(p["response"].as_str().unwrap());
            assert!(p.get("request").is_none(), "side=response returns one message: {p}");
            if !p["responseSlice"]["hasMore"].as_bool().unwrap() {
                break;
            }
            at = p["responseSlice"]["nextOffset"].as_u64().unwrap();
        }
        assert_eq!(body.len(), BIG.len(), "the body reassembles from its pages");

        // Headers only: a few hundred bytes, no body, and it does not pretend to be all.
        let h = jparse(&call_tool(&c, "history_get", &json!({"id": id, "side": "response", "part": "headers"})).await.unwrap());
        let text = h["response"].as_str().unwrap();
        assert!(text.starts_with("HTTP/1.1 200") && text.ends_with("\n\n") && text.len() < 400, "{text}");
        assert_eq!(h["responseSlice"]["bodyIncluded"], false);
        assert_eq!(h["responseSlice"]["bodyBytes"], 40 * 1024);

        // An offset with no side named is ambiguous, so it is refused rather than guessed.
        assert!(call_tool(&c, "history_get", &json!({"id": id, "offset": 10})).await.is_err());
        assert!(call_tool(&c, "history_get", &json!({"id": id, "side": "response", "part": "headers", "offset": 10})).await.is_err());
        assert!(call_tool(&c, "history_get", &json!({"id": id, "encoding": "base64"})).await.is_err(), "base64 needs part=body");
        assert!(call_tool(&c, "history_get", &json!({"id": id, "part": "trailer"})).await.is_err());

        // A small flow is unchanged: no receipt at all, and the text it always returned.
        let small = seed_flows(&c, 1, b"{\"ok\":true}")[0];
        let s = jparse(&call_tool(&c, "history_get", &json!({"id": small, "view": "wire"})).await.unwrap());
        assert!(s.get("responseSlice").is_none() && s.get("requestSlice").is_none(), "{s}");
        assert!(s["response"].as_str().unwrap().ends_with("{\"ok\":true}"));
    }

    /// Lossy UTF-8 of binary bytes is replacement characters: bigger than the body and
    /// worth nothing. It must not be the default answer.
    #[tokio::test]
    async fn history_get_does_not_dump_a_binary_body_as_text() {
        use crate::store::Flow;
        use http::{HeaderMap, HeaderValue, Method as M, StatusCode, Uri, Version};
        let c = ctx();
        let png: Vec<u8> = b"\x89PNG\r\n\x1a\n".iter().copied().chain((0u8..=255).cycle().take(4096)).collect();
        let uri: Uri = "https://api.test/logo.png".parse().unwrap();
        let mut f = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        let mut rh = HeaderMap::new();
        rh.insert("content-type", HeaderValue::from_static("image/png"));
        rh.insert("content-length", HeaderValue::from_str(&png.len().to_string()).unwrap());
        f.set_response(StatusCode::OK, Version::HTTP_11, &rh, bytes::Bytes::from(png.clone()));
        let id = c.store.push(f).unwrap().id;

        let d = jparse(&call_tool(&c, "history_get", &json!({"id": id, "side": "response"})).await.unwrap());
        let r = &d["responseSlice"];
        assert_eq!((r["binary"].as_bool(), r["bodyIncluded"].as_bool(), r["returned"].as_u64()), (Some(true), Some(false), Some(0)), "{d}");
        assert!(!d["response"].as_str().unwrap().contains('\u{fffd}'), "no replacement characters are shipped");
        assert!(r["note"].as_str().unwrap().contains("base64"), "and it says how to get the bytes: {r}");

        // base64 returns the real bytes, capped, and the range reassembles.
        let b = jparse(
            &call_tool(&c, "history_get", &json!({"id": id, "side": "response", "part": "body", "encoding": "base64", "maxBytes": 64}))
                .await
                .unwrap(),
        );
        let decoded = STANDARD.decode(b["response"].as_str().unwrap()).expect("valid base64");
        assert_eq!(decoded, png[..64], "the first bytes, exactly");
        assert_eq!(b["responseSlice"]["nextOffset"], 64);
    }

    /// A WebSocket flow's history_get is only the handshake, so the messages have to be
    /// reachable and have to describe themselves honestly.
    #[tokio::test]
    async fn ws_messages_pages_a_socket_and_says_what_a_payload_is() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};
        let c = ctx();
        let uri: Uri = "https://api.test/socket".parse().unwrap();
        let mut f = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        f.set_response(StatusCode::SWITCHING_PROTOCOLS, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        let fid = c.store.push(f).unwrap().id;

        for i in 0..6 {
            let dir = if i % 2 == 0 { "client" } else { "server" };
            let text = format!("{{\"n\":{i},\"token\":\"needle-{i}\"}}");
            c.store.push_ws(fid, 1000 + i, dir, 1, text.len() as u64, text.as_bytes()).unwrap();
        }
        // A binary message whose true size is larger than what capture kept.
        let blob: Vec<u8> = b"\x00\x01\x02\x03".iter().copied().cycle().take(2048).collect();
        c.store.push_ws(fid, 2000, "server", 2, 9_000_000, &blob).unwrap();
        // And a close message.
        c.store.push_ws(fid, 2001, "server", 8, 5, b"\x03\xe8bye").unwrap();

        // Totals, then a page, then the cursor.
        let p1 = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": fid, "limit": 3})).await.unwrap());
        assert_eq!((p1["total"].as_u64(), p1["client"].as_u64(), p1["server"].as_u64()), (Some(8), Some(3), Some(5)), "{p1}");
        assert_eq!(p1["returned"], 3);
        assert_eq!(p1["hasMore"], true);
        let cursor = p1["nextAfterId"].as_u64().unwrap();
        let p2 = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": fid, "afterId": cursor, "limit": 100})).await.unwrap());
        let ids: Vec<u64> = p2["messages"].as_array().unwrap().iter().map(|m| m["id"].as_u64().unwrap()).collect();
        assert!(ids.iter().all(|i| *i > cursor), "the cursor does not repeat a message: {ids:?}");
        assert_eq!(p1["returned"].as_u64().unwrap() + p2["returned"].as_u64().unwrap(), 8);

        // The binary message: hex, cut, and all three sizes reported, so nobody quotes
        // the length of the text as the size of the payload.
        let all = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": fid, "limit": 100, "maxBytes": 65536})).await.unwrap());
        let bin = all["messages"].as_array().unwrap().iter().find(|m| m["opcode"] == 2).expect("the binary message");
        assert_eq!((bin["type"].as_str(), bin["renderedAs"].as_str()), (Some("binary"), Some("hex")), "{bin}");
        assert_eq!((bin["length"].as_u64(), bin["stored"].as_u64(), bin["returned"].as_u64()), (Some(9_000_000), Some(2048), Some(256)));
        assert_eq!(bin["complete"], false);
        assert!(bin["payload"].as_str().unwrap().starts_with("00000000  00 01 02 03"), "{bin}");
        assert!(all["note"].as_str().is_some_and(|n| n.contains("rendering")), "{all}");
        // The close message reads as a close, not as bytes.
        let close = all["messages"].as_array().unwrap().iter().find(|m| m["opcode"] == 8).expect("the close message");
        assert_eq!((close["type"].as_str(), close["renderedAs"].as_str(), close["payload"].as_str()), (Some("close"), Some("close"), Some("close 1000 bye")));

        // Filters.
        let only = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": fid, "direction": "client", "limit": 100})).await.unwrap());
        assert!(only["messages"].as_array().unwrap().iter().all(|m| m["direction"] == "client"), "{only}");
        let found = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": fid, "contains": "needle-3"})).await.unwrap());
        assert_eq!(found["returned"], 1, "{found}");
        assert!(found["containsNote"].as_str().is_some(), "the index's limits are stated: {found}");
        assert!(call_tool(&c, "ws_messages", &json!({"flowId": fid, "direction": "upstream"})).await.is_err());

        // The handshake flow points at them, so a reader is not left thinking it is empty.
        let hs = jparse(&call_tool(&c, "history_get", &json!({"id": fid})).await.unwrap());
        assert_eq!(hs["wsMessages"], 8, "{hs}");
        assert!(hs["wsNote"].as_str().unwrap().contains("ws_messages"), "{hs}");

        // A search that matched inside a message says so, instead of handing back a flow
        // whose history_get shows only a 101 handshake.
        let s = jparse(&call_tool(&c, "history_search", &json!({"query": "needle-3"})).await.unwrap());
        assert_eq!(s["wsMatches"], json!([fid]), "{s}");
        assert!(s["wsNote"].as_str().unwrap().contains("ws_messages"), "{s}");

        // A flow with no messages is a fact, not an error; an unknown flow is an error.
        let plain = seed_flows(&c, 1, b"{}")[0];
        let empty = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": plain})).await.unwrap());
        assert_eq!((empty["total"].as_u64(), empty["returned"].as_u64()), (Some(0), Some(0)), "{empty}");
        assert!(empty["note"].as_str().unwrap().contains("no WebSocket"), "{empty}");
        assert!(call_tool(&c, "ws_messages", &json!({"flowId": 99999})).await.is_err());
        assert!(call_tool(&c, "ws_messages", &json!({})).await.is_err(), "flowId is required");
    }

    /// The budget has to beat limit x maxBytes, or one call can return 32 MB.
    #[tokio::test]
    async fn ws_messages_stops_at_its_byte_budget() {
        use crate::store::Flow;
        use http::{HeaderMap, Method as M, StatusCode, Uri, Version};
        let c = ctx();
        let uri: Uri = "https://api.test/firehose".parse().unwrap();
        let mut f = Flow::request(c.store.next_id(), &M::GET, &uri, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        f.set_response(StatusCode::SWITCHING_PROTOCOLS, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        let fid = c.store.push(f).unwrap().id;
        let big = "y".repeat(WS_BYTES_MAX);
        for i in 0..40 {
            c.store.push_ws(fid, 3000 + i, "server", 1, big.len() as u64, big.as_bytes()).unwrap();
        }
        let out = jparse(&call_tool(&c, "ws_messages", &json!({"flowId": fid, "limit": 500, "maxBytes": WS_BYTES_MAX})).await.unwrap());
        assert_eq!(out["budgetReached"], true, "{}", out["returned"]);
        let spent: usize = out["messages"].as_array().unwrap().iter().map(|m| m["payload"].as_str().unwrap().len()).sum();
        assert!(spent <= WS_BUDGET + WS_BYTES_MAX, "bounded at about the budget, got {spent}");
        assert!(out["returned"].as_u64().unwrap() < 40, "it stopped early: {}", out["returned"]);
        assert!(out["nextAfterId"].is_number(), "and it can be continued: {out}");
    }

    /// An empty History has several causes, and a caller that cannot tell them apart
    /// reports that the application makes no HTTP requests.
    #[tokio::test]
    async fn proxy_status_diagnoses_an_empty_capture_without_leaking_addresses() {
        let c = ctx();
        let s = jparse(&call_tool(&c, "proxy_status", &json!({})).await.unwrap());
        assert_eq!((s["listening"].as_bool(), s["flows"].as_u64()), (Some(false), Some(0)));
        let hints = s["hints"].as_array().expect("hints when nothing was captured");
        assert!(hints.iter().any(|h| h.as_str().unwrap().contains("not running")), "{hints:?}");
        assert!(s["note"].as_str().unwrap().contains("Read-only"), "{s}");

        // A loopback listener and a non-loopback one: the port and the reach are
        // reported, the address never is.
        c.add_listener("127.0.0.1:8080".parse().unwrap());
        c.add_listener("10.1.2.3:8081".parse().unwrap());
        let s = jparse(&call_tool(&c, "proxy_status", &json!({})).await.unwrap());
        assert_eq!(s["listening"], true);
        let ls = s["listeners"].as_array().unwrap();
        assert_eq!(ls.len(), 2, "{s}");
        for l in ls {
            assert!(l["port"].is_number() && l["exposure"].is_string(), "{l}");
            assert!(l.get("host").is_none() && l.get("addr").is_none(), "no bind address is reported: {l}");
        }
        let text = serde_json::to_string(&s).unwrap();
        assert!(!text.contains("10.1.2.3"), "a LAN address must not reach a tool result: {text}");
        assert!(!text.contains("127.0.0.1"), "nor a loopback one, so the rule has no exceptions to learn");
        // The counts stand in for the extension status, which carries filesystem paths.
        assert!(s["extensions"]["loaded"].is_number() && s["extensions"].get("python").is_none(), "{s}");

        // Once something is captured, the hints stop and the age appears.
        seed_flows(&c, 2, b"{}");
        let s = jparse(&call_tool(&c, "proxy_status", &json!({})).await.unwrap());
        assert_eq!(s["flows"], 2);
        assert!(s.get("hints").is_none(), "{s}");
        assert!(s["newestFlowAgeMs"].is_number() && s["newestFlowTs"].is_number(), "{s}");
    }

    /// A finding this server records can end up in a client-facing report, so it must be
    /// impossible to make it look like one Ngintip itself produced.
    #[tokio::test]
    async fn findings_add_records_a_claim_the_caller_cannot_disown() {
        let c = ctx();
        let id = seed_flows(&c, 1, b"{\"order\":7}")[0];

        let out = jparse(
            &call_tool(
                &c,
                "findings_add",
                &json!({
                    "title": "IDOR on /item/{id}",
                    "severity": "high",
                    "confidence": "firm",
                    "detail": "Item 0 belongs to another user; only the path id was changed.",
                    "flowId": id,
                    "citedFlowIds": [id],
                }),
            )
            .await
            .unwrap(),
        );
        let item = &out["item"];
        // The three attribution layers, none of them settable by the caller.
        assert_eq!(item["source"], "mcp-finding", "distinct from a saved request's 'mcp': {item}");
        assert!(item["label"].as_str().unwrap().starts_with("[agent high] "), "{item}");
        let note = item["note"].as_str().unwrap();
        assert!(note.starts_with(FINDING_HEADER), "{note}");
        assert!(note.contains("claimed by an LLM"), "the load-bearing word: {note}");
        assert!(note.contains("severity: high") && note.contains("confidence: firm"), "{note}");
        assert!(note.contains(&format!("flow {id}")), "the evidence is cited: {note}");
        assert!(note.ends_with("only the path id was changed."), "{note}");
        assert_eq!(item["color"], "red", "severity sets the colour: {item}");
        assert_eq!(out["collection"]["name"], "Findings");
        assert!(out["attribution"].as_str().unwrap().contains("mcp-finding"), "{out}");

        // Attempts to dress it up as something else are ignored, not honoured.
        let faked = jparse(
            &call_tool(
                &c,
                "findings_add",
                &json!({"title": "x", "severity": "info", "detail": "y", "source": "passive", "color": "green", "note": "clean"}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(faked["item"]["source"], "mcp-finding", "{faked}");
        assert_eq!(faked["item"]["color"], "gray");
        assert!(faked["item"]["note"].as_str().unwrap().starts_with(FINDING_HEADER));

        // The fields a report needs are required, and the vocabulary is the scanner's.
        assert!(call_tool(&c, "findings_add", &json!({"title": "x", "severity": "high"})).await.is_err());
        assert!(call_tool(&c, "findings_add", &json!({"title": "x", "detail": "y"})).await.is_err(), "severity required");
        assert!(call_tool(&c, "findings_add", &json!({"title": "x", "severity": "critical", "detail": "y"})).await.is_err());
        assert!(call_tool(&c, "findings_add", &json!({"title": "x", "severity": "low", "detail": "y", "flowId": 99999})).await.is_err());

        // Out of scope is a warning, not a refusal: reading the capture is unrestricted,
        // but a finding is work product.
        c.scope.set(vec!["elsewhere.test".into()]);
        let warned = jparse(
            &call_tool(&c, "findings_add", &json!({"title": "x", "severity": "low", "detail": "y", "flowId": id})).await.unwrap(),
        );
        assert!(warned["scopeWarning"].as_str().is_some_and(|w| w.contains("outside the target scope")), "{warned}");
        assert_eq!(warned["item"]["source"], "mcp-finding", "still recorded");

        // It lands in the Organizer, and the window is told, so it is not a write-only
        // hole: the whole point of choosing this surface.
        let listed = jparse(&call_tool(&c, "organizer_list", &json!({"collection": "Findings"})).await.unwrap());
        assert_eq!(listed["collections"][0]["count"], 3, "{listed}");
    }

    /// An encrypted API is readable in the window, so it has to be readable over MCP
    /// too: the version a `decode_*` extension produces must reach `history_get`
    /// without ever displacing the bytes that actually crossed the wire.
    #[tokio::test]
    async fn history_get_serves_the_decoded_version_beside_the_wire() {
        use crate::store::Flow;
        use http::{HeaderMap, HeaderValue, Method as M, StatusCode, Uri, Version};
        if crate::extensions::detect_python().is_none() {
            eprintln!("skipping: no python interpreter");
            return;
        }
        let c = ctx();
        let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap();
        // Real captures carry Content-Length, and a hook that changes nothing then
        // re-serializes to identical bytes, which is what "no change" relies on.
        let with_len = |n: usize| {
            let mut h = HeaderMap::new();
            h.insert("content-length", HeaderValue::from_str(&n.to_string()).unwrap());
            h
        };

        // A captured flow whose bodies stand in for ciphertext.
        let uri: Uri = "https://api.t/blue/api/transfer".parse().unwrap();
        let body = bytes::Bytes::from_static(b"cherry-request");
        let mut flow = Flow::request(c.store.next_id(), &M::POST, &uri, Version::HTTP_11, &with_len(body.len()), body);
        let res_body = bytes::Bytes::from_static(b"cherry-response");
        flow.set_response(StatusCode::OK, Version::HTTP_11, &with_len(res_body.len()), res_body);
        let fid = c.store.push(flow).unwrap().id;

        // With no extension loaded there is one version and no extra keys at all.
        let bare = parse(call_tool(&c, "history_get", &json!({"id": fid})).await.unwrap());
        assert_eq!(bare["request"].as_str().unwrap().lines().next(), Some("POST /blue/api/transfer HTTP/1.1"));
        assert!(bare.get("versions").is_none(), "nothing to add: {bare}");

        // A decode-only extension, scoped to this host by the script itself.
        let dir = crate::e2e::temp_dir("mcp-decode");
        let script = dir.join("d.py");
        std::fs::write(
            &script,
            "def decode_request(req, ctx):\n    if ctx.host == 'api.t':\n        req.text = req.text.upper()\n\n\
             def decode_response(req, ctx):\n    if ctx.host == 'api.t':\n        req.text = req.text.upper()\n",
        )
        .unwrap();
        c.extensions.configure(dir.clone(), None, None);
        c.extensions.set_extensions(vec![crate::extensions::ExtensionSpec {
            id: "d".into(),
            name: "decode".into(),
            path: script.to_string_lossy().into_owned(),
            enabled: true,
            scope: "both".into(),
            requirements: vec![],
            body: String::new(),
        }]);
        c.extensions.set_enabled(true);
        assert!(c.extensions.active(), "extension worker should be running");

        // auto (the default): the wire stays where it was, the readable form is added.
        let auto = parse(call_tool(&c, "history_get", &json!({"id": fid})).await.unwrap());
        assert_eq!(auto["view"], "wire");
        assert!(auto["request"].as_str().unwrap().contains("cherry-request"), "wire untouched: {auto}");
        assert!(auto["response"].as_str().unwrap().contains("cherry-response"), "wire untouched: {auto}");
        assert!(auto["versions"]["requestExtension"].as_str().unwrap().contains("CHERRY-REQUEST"), "{auto}");
        assert!(auto["versions"]["responseExtension"].as_str().unwrap().contains("CHERRY-RESPONSE"), "{auto}");
        assert!(auto["versionsNote"].as_str().is_some_and(|n| n.contains("decode_*")), "{auto}");
        // decode_* changes nothing on the wire, so there is no Original to report.
        assert!(auto["versions"].get("requestOriginal").is_none(), "{auto}");
        assert!(auto["versions"].get("responseOriginal").is_none(), "{auto}");

        // view=wire: the thrifty shape this tool had before, even with a decode active.
        let wire = parse(call_tool(&c, "history_get", &json!({"id": fid, "view": "wire"})).await.unwrap());
        assert!(wire.get("versions").is_none(), "{wire}");
        assert!(wire["request"].as_str().unwrap().contains("cherry-request"));

        // view=extension: the readable form in the usual place, flagged as decoded.
        let ext = parse(call_tool(&c, "history_get", &json!({"id": fid, "view": "extension"})).await.unwrap());
        assert!(ext["request"].as_str().unwrap().contains("CHERRY-REQUEST"), "{ext}");
        assert!(ext["response"].as_str().unwrap().contains("CHERRY-RESPONSE"), "{ext}");
        assert_eq!((ext["requestDecoded"].as_bool(), ext["responseDecoded"].as_bool()), (Some(true), Some(true)));

        // A host the script ignores gets no decoded version, so nothing is invented.
        let other: Uri = "https://other.t/x".parse().unwrap();
        let mut f2 = Flow::request(c.store.next_id(), &M::GET, &other, Version::HTTP_11, &HeaderMap::new(), bytes::Bytes::new());
        let plain = bytes::Bytes::from_static(b"plain");
        f2.set_response(StatusCode::OK, Version::HTTP_11, &with_len(plain.len()), plain);
        let other_id = c.store.push(f2).unwrap().id;
        let out = parse(call_tool(&c, "history_get", &json!({"id": other_id})).await.unwrap());
        assert!(out.get("versions").is_none(), "no decode for a host the script skips: {out}");
        let ext_other = parse(call_tool(&c, "history_get", &json!({"id": other_id, "view": "extension"})).await.unwrap());
        assert_eq!(ext_other["requestDecoded"], false, "falls back to the wire, and says so: {ext_other}");
        assert!(ext_other["response"].as_str().unwrap().contains("plain"));

        // An unknown view is refused rather than quietly treated as the default.
        assert!(call_tool(&c, "history_get", &json!({"id": fid, "view": "plaintext"})).await.is_err());
        // And through the path a real client takes, not just the dispatcher: the schema
        // has to advertise the parameter, or no client will ever pass it.
        let list = handle_rpc(&c, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).await.unwrap();
        let def = list["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "history_get").cloned().expect("history_get listed");
        let views: Vec<&str> = def["inputSchema"]["properties"]["view"]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(views, VIEWS.to_vec(), "schema advertises the views: {def}");
        let rpc = handle_rpc(
            &c,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"history_get","arguments":{"id":fid,"view":"extension"}}}),
        )
        .await
        .unwrap();
        assert_eq!(rpc["result"]["isError"], false, "{rpc}");
        let text = rpc["result"]["content"][0]["text"].as_str().unwrap().to_string();
        assert!(parse(text.clone())["request"].as_str().unwrap().contains("CHERRY-REQUEST"), "{text}");
        // A rejected view is a tool error the model can read, not a protocol error.
        let bad = handle_rpc(
            &c,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"history_get","arguments":{"id":fid,"view":"nope"}}}),
        )
        .await
        .unwrap();
        assert_eq!(bad["result"]["isError"], true, "{bad}");
        assert!(bad["result"]["content"][0]["text"].as_str().unwrap().contains("invalid view"), "{bad}");

        // Out of scope, the decode never runs: the wire is all there is.
        c.scope.set(vec!["other.t".into()]);
        let scoped = parse(call_tool(&c, "history_get", &json!({"id": fid})).await.unwrap());
        assert!(scoped.get("versions").is_none(), "host out of Target scope: {scoped}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn vault_tools_hide_secrets_in_listings_and_never_write() {
        let c = ctx();
        let stored = json!({ "entries": [
            { "id": "v1", "label": "Admin", "role": "admin", "note": "", "fields": [
                { "id": "f1", "kind": "email", "name": "Email", "secret": false, "value": "admin@shop.test" },
                { "id": "f2", "kind": "password", "name": "Password", "secret": true, "value": "S3cret!" },
            ]},
            { "id": "v2", "label": "admin", "username": "root", "password": "toor" },
        ]});
        c.store.put_setting("vault", &stored).unwrap();

        let list = call_tool(&c, "vault_list", &json!({})).await.unwrap();
        assert!(list.contains("admin@shop.test") && list.contains(vault::HIDDEN), "{list}");
        assert!(!list.contains("S3cret!") && !list.contains("toor"), "no secret in a listing: {list}");
        assert_eq!(jparse(&call_tool(&c, "vault_list", &json!({"query": "root"})).await.unwrap())["returned"], 1);
        assert_eq!(jparse(&call_tool(&c, "vault_list", &json!({"query": "S3cret"})).await.unwrap())["returned"], 0);

        // vault_get returns the secrets; a label two entries share must use the id.
        assert!(call_tool(&c, "vault_get", &json!({"id": "v1"})).await.unwrap().contains("S3cret!"));
        assert!(call_tool(&c, "vault_get", &json!({"id": "v2"})).await.unwrap().contains("toor"));
        assert!(call_tool(&c, "vault_get", &json!({"label": "ADMIN"})).await.is_err(), "ambiguous label");
        assert!(call_tool(&c, "vault_get", &json!({"id": "nope"})).await.is_err());
        assert!(call_tool(&c, "vault_get", &json!({})).await.is_err());
        assert_eq!(c.store.get_setting("vault").unwrap().unwrap(), stored, "read-only");
    }
}
