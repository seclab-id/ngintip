//! MCP (Model Context Protocol) server over HTTP, so an LLM can drive Cegat:
//! read/search history, send to Repeater (recorded), run parallel batches and
//! Intruder attacks, and view/trigger/configure the passive scan.
//!
//! Transport: MCP "Streamable HTTP". Clients POST JSON-RPC to the endpoint and
//! get a JSON response; GET <docs_path> returns human/LLM docs. Bound to
//! 127.0.0.1 only — the tools are powerful, so it's local-only by design.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http::{header, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::brand;
use crate::proxy::Ctx;
use crate::raw;

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

async fn call_tool(ctx: &Arc<Ctx>, name: &str, a: &Value) -> Result<String, String> {
    match name {
        "history_list" => history_list(ctx, a),
        "history_get" => history_get(ctx, a),
        "history_search" => history_search(ctx, a),
        "history_set_note" => history_set_note(ctx, a),
        "history_set_color" => history_set_color(ctx, a),
        "history_get_color" => history_get_color(ctx, a),
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
        other => Err(format!("unknown tool: {other}")),
    }
}

fn ok_json(v: Value) -> Result<String, String> {
    Ok(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into()))
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
    let limit = arg_u64(a, "limit", 100).clamp(1, 1000) as usize;

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
    let items: Vec<Value> = all.iter().rev().take(limit).map(|f| serde_json::to_value(f).unwrap_or(Value::Null)).collect();
    ok_json(json!({ "total": total, "returned": items.len(), "flows": items }))
}

fn history_get(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let id = arg_u64(a, "id", 0);
    let flow = ctx.store.get(id).map_err(|e| e.to_string())?.ok_or_else(|| format!("no flow {id}"))?;
    ok_json(json!({
        "summary": serde_json::to_value(&flow.summary).unwrap_or(Value::Null),
        "request": raw::request_text_full(&flow),
        "response": raw::response_text_full(&flow),
    }))
}

fn history_search(ctx: &Arc<Ctx>, a: &Value) -> Result<String, String> {
    let query = arg_str(a, "query");
    if query.trim().is_empty() {
        return Err("query is required".into());
    }
    let limit = arg_u64(a, "limit", 100).clamp(1, 1000) as usize;
    let ids: std::collections::HashSet<u64> = ctx.store.search(&query).map_err(|e| e.to_string())?.into_iter().collect();
    let items: Vec<Value> = ctx
        .store
        .summaries()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|f| ids.contains(&f.id))
        .rev()
        .take(limit)
        .map(|f| serde_json::to_value(f).unwrap_or(Value::Null))
        .collect();
    ok_json(json!({ "matches": ids.len(), "returned": items.len(), "flows": items }))
}

/// Highlight colours a flow can be tagged with (mirrors HIGHLIGHTS in src/lib/colors.js);
/// an empty string clears the highlight.
const HIGHLIGHT_COLORS: [&str; 9] = ["red", "orange", "yellow", "green", "cyan", "blue", "purple", "pink", "gray"];

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
    ctx.scope.set(patterns);
    let normalized = ctx.scope.get(); // trimmed, lowercased, deduped
    ctx.store.put_list(crate::commands::SCOPE_KEY, &normalized).map_err(|e| e.to_string())?;
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
    let preview: String = res.response_raw.chars().take(RESP_PREVIEW).collect();
    ok_json(json!({
        "tab": tab,
        "status": res.status,
        "length": res.length,
        "durationMs": res.duration_ms,
        "response": preview,
    }))
}

async fn send_many(ctx: &Arc<Ctx>, tab: u64, target: &str, raw: &str, count: usize, parallel: bool) -> Vec<Value> {
    let one = |i: usize| {
        let (ctx, target, raw) = (ctx.clone(), target.to_string(), raw.to_string());
        async move {
            let started = Instant::now();
            match crate::commands::send_recorded(&ctx, Some(tab), &target, &raw, true).await {
                Ok(r) => json!({ "i": i, "status": r.status, "length": r.length, "durationMs": r.duration_ms }),
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
                Ok(r) => json!({ "tab": id, "status": r.status, "length": r.length, "durationMs": r.duration_ms }),
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
                Ok(r) => json!({ "n": n, "payloads": payloads, "status": r.status, "length": r.length, "timeMs": r.duration_ms }),
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
    ok_json(json!({ "count": filtered.len(), "findings": filtered }))
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
    ctx.store.put_setting("passiveRules", &rules).map_err(|e| e.to_string())?;
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
    // Compile builtin rules + custom rules from settings (skip any that fail).
    let mut rules: Vec<(String, String, String, regex::Regex, usize)> = Vec::new();
    for (id, title, sev, pat, _ci, group) in BUILTIN_RULES {
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
    let _ = ctx.store.put_setting("passiveFindings", &Value::Array(findings.clone()));
    let _ = ctx.store.put_setting("passiveScanned", &serde_json::to_value(&scanned).unwrap_or(json!([])));

    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in &findings {
        *counts.entry(f.get("severity").and_then(|x| x.as_str()).unwrap_or("info").to_string()).or_insert(0) += 1;
    }
    ok_json(json!({ "scannedFlows": scanned.len(), "findings": findings.len(), "bySeverity": counts }))
}

// ---- tool schemas & docs ----------------------------------------------------

fn tool_defs() -> Value {
    let obj = |props: Value, required: Value| json!({ "type": "object", "properties": props, "required": required });
    json!([
        { "name": "history_list", "description": "List captured HTTP flows (newest first). Filter by method, host substring, status, a 'contains' match on method+host+path, or inScope (only hosts in the target scope; default false = everything).",
          "inputSchema": obj(json!({ "method": {"type":"string"}, "host": {"type":"string"}, "status": {"type":"integer"}, "contains": {"type":"string"}, "inScope": {"type":"boolean"}, "limit": {"type":"integer"} }), json!([])) },
        { "name": "history_get", "description": "Full request and response text of one flow by id.",
          "inputSchema": obj(json!({ "id": {"type":"integer"} }), json!(["id"])) },
        { "name": "history_search", "description": "Full-text search across request/response bodies; returns matching flow summaries.",
          "inputSchema": obj(json!({ "query": {"type":"string"}, "limit": {"type":"integer"} }), json!(["query"])) },
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
        { "name": "repeater_send", "description": "Send one request (raw+target, or clone flowId) and record it in Repeater history. Returns status/length/response preview.",
          "inputSchema": obj(json!({ "raw": {"type":"string"}, "target": {"type":"string"}, "flowId": {"type":"integer"}, "tab": {"type":"integer"} }), json!([])) },
        { "name": "repeater_send_batch", "description": "Send the same request many times, in parallel (default) or sequential. For race conditions (e.g. duplicate-registration).",
          "inputSchema": obj(json!({ "raw": {"type":"string"}, "target": {"type":"string"}, "flowId": {"type":"integer"}, "count": {"type":"integer"}, "mode": {"type":"string","enum":["parallel","sequential"]}, "tab": {"type":"integer"} }), json!([])) },
        { "name": "repeater_groups", "description": "List Repeater tab groups and their tabs.", "inputSchema": obj(json!({}), json!([])) },
        { "name": "repeater_send_group", "description": "Send every tab in a Repeater group, in parallel (default) or sequential.",
          "inputSchema": obj(json!({ "group": {"type":"string"}, "mode": {"type":"string","enum":["parallel","sequential"]} }), json!(["group"])) },
        { "name": "intruder_run", "description": "Run an Intruder attack. Mark positions in `raw` with the section sign §. type: sniper|battering|pitchfork|cluster. payloads: a list of lines (one set) or a list of lists (a set per position).",
          "inputSchema": obj(json!({ "raw": {"type":"string"}, "target": {"type":"string"}, "flowId": {"type":"integer"}, "type": {"type":"string"}, "payloads": {}, "concurrency": {"type":"integer"} }), json!([])) },
        { "name": "passive_findings", "description": "List passive-scan findings (from the last scan). Filter by severity.",
          "inputSchema": obj(json!({ "severity": {"type":"string","enum":["high","medium","low","info"]} }), json!([])) },
        { "name": "passive_scan", "description": "Run the passive scan over all captured flows now (secrets, tokens, risky headers) and store the findings.", "inputSchema": obj(json!({}), json!([])) },
        { "name": "passive_rules_get", "description": "Get the passive rules (built-in + custom).", "inputSchema": obj(json!({}), json!([])) },
        { "name": "passive_rules_set", "description": "Replace the custom passive rules. Each: {id,title,severity,pattern,flags,group}.",
          "inputSchema": obj(json!({ "rules": {"type":"array"} }), json!(["rules"])) },
        { "name": "extensions_status", "description": "State of the Python extension engine: interpreter, venv, and each loaded script (id, enabled, running, error).", "inputSchema": obj(json!({}), json!([])) },
        { "name": "extensions_enable", "description": "Turn the whole extension engine on or off.",
          "inputSchema": obj(json!({ "enabled": {"type":"boolean"} }), json!(["enabled"])) },
        { "name": "extension_toggle", "description": "Enable or disable one extension by id (see extensions_status).",
          "inputSchema": obj(json!({ "id": {"type":"string"}, "enabled": {"type":"boolean"} }), json!(["id","enabled"])) },
        { "name": "extensions_logs", "description": "Recent lines from the extension debug console (script stderr and lifecycle).", "inputSchema": obj(json!({}), json!([])) },
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
and search captured traffic, replay and fuzz requests, and run the passive
scanner. Everything an operator does in the Repeater, Intruder and Passive tabs is
available here, and every request this server sends is recorded in History like
ordinary proxy traffic.

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
them afterwards with `history_get`. The Repeater tools also append to a Repeater
tab history, chosen with `tab` (default 9001).

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

### Passive scan

`passive_scan` runs the built-in and custom rules over captured flows and stores
the findings. What gets scanned is controlled per project by the `passiveScope`
setting: a list of file extensions or content types, or `["*"]` for everything; an
unset or empty scope scans everything. `passive_findings` reads the stored
findings. `passive_rules_get` and `passive_rules_set` manage the custom rules.

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

Returns: `{ total, returned, flows: [summary, ...] }`. Each summary has `id`, `ts`,
`method`, `scheme`, `host`, `path`, `status`, `length`, `mime`, `durationMs` and
more.

Example arguments:

    { "host": "api.example.com", "method": "POST", "inScope": true, "limit": 50 }

### history_get

Full request and response of one flow.

Arguments: `id` (integer, required).

Returns: `{ summary, request, response }`, where `request` and `response` are the
full decoded text.

### history_search

Full-text search across request and response bodies.

Arguments: `query` (string, required); `limit` (integer, 1 to 1000, default 100).

Returns: `{ matches, returned, flows: [summary, ...] }`.

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

Returns: `{ group, sent, results: [ { tab, status, length, durationMs } or
{ tab, error } ] }`.

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

Arguments: `severity` (string, optional): one of `high`, `medium`, `low`, `info`.

Returns: `{ count, findings: [ { type, title, severity, where, match, flowId,
method, host, path, status } ] }`.

### passive_rules_get

Get all passive rules, built-in and custom. No arguments.

### passive_rules_set

Replace the custom rules (built-in rules are not affected).

Arguments: `rules` (array, required). Each rule is `{ id, title, severity, pattern,
flags, group }`, where `pattern` is a regular expression string, `flags` may
include `i` for case insensitive, and `group` is the capture group number to
report (0 for the whole match).

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
    use crate::e2e::test_ctx;

    fn ctx() -> Arc<Ctx> {
        let dir = std::env::temp_dir().join(format!("mcp-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(test_ctx(&dir))
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
        // A notification produces no response.
        assert!(handle_rpc(&c, json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.is_none());
    }

    #[tokio::test]
    async fn extension_tools_report_and_toggle() {
        let c = ctx();
        // No extensions loaded yet: engine off, empty list.
        let s = call_tool(&c, "extensions_status", &json!({})).await.unwrap();
        assert!(s.contains("\"enabled\": false"), "{s}");
        // Turning the engine on flips the flag.
        let s = call_tool(&c, "extensions_enable", &json!({"enabled": true})).await.unwrap();
        assert!(s.contains("\"enabled\": true"), "{s}");
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
        assert!(out.contains("\"total\": 1"), "{out}");
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
        assert!(cur.contains("\"color\": \"blue\"") && cur.contains("\"colors\""), "{cur}");
        call_tool(&c, "history_set_color", &json!({"id": in_id, "color": ""})).await.unwrap();

        // Scope set (normalizes + persists), then get/check.
        let set = call_tool(&c, "scope_set", &json!({"patterns": [" *.Example.com ", "", "*.example.com"]})).await.unwrap();
        assert!(set.contains("*.example.com"), "{set}");
        assert!(!set.contains("Example"), "normalized (lowercased/deduped): {set}");
        assert_eq!(c.scope.get(), vec!["*.example.com"], "applied to the live scope");
        assert_eq!(c.store.get_list(crate::commands::SCOPE_KEY).unwrap(), vec!["*.example.com"], "persisted to the project");
        assert!(call_tool(&c, "scope_set", &json!({})).await.is_err(), "patterns required");

        let got = call_tool(&c, "scope_get", &json!({})).await.unwrap();
        assert!(got.contains("*.example.com") && got.contains("\"allInScope\": false"), "{got}");
        let chk = call_tool(&c, "scope_check", &json!({"host": "in.example.com"})).await.unwrap();
        assert!(chk.contains("\"inScope\": true"), "{chk}");
        let chk2 = call_tool(&c, "scope_check", &json!({"host": "out.other.com"})).await.unwrap();
        assert!(chk2.contains("\"inScope\": false"), "{chk2}");
        assert!(call_tool(&c, "scope_check", &json!({})).await.is_err(), "host required");

        // history_list inScope filter (default off = both, on = only in-scope).
        let both = call_tool(&c, "history_list", &json!({})).await.unwrap();
        assert!(both.contains("\"total\": 2"), "{both}");
        let scoped = call_tool(&c, "history_list", &json!({"inScope": true})).await.unwrap();
        assert!(scoped.contains("\"total\": 1"), "only in-scope: {scoped}");
        let _ = out_id;
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
        assert!(scan.contains("\"scannedFlows\": 1"), "{scan}");
        let found = call_tool(&c, "passive_findings", &json!({})).await.unwrap();
        assert!(found.contains("email"), "{found}");
        assert!(found.contains("cors"), "{found}");

        // Custom rules persist.
        call_tool(&c, "passive_rules_set", &json!({"rules":[{"id":"x","title":"X","severity":"low","pattern":"foo","flags":""}]})).await.unwrap();
        let rules = call_tool(&c, "passive_rules_get", &json!({})).await.unwrap();
        assert!(rules.contains("\"pattern\": \"foo\""), "{rules}");
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
        assert!(all.contains("\"scannedFlows\": 2"), "{all}");
        // Scope to example.com and restrict to in-scope only.
        c.scope.set(vec!["*.example.com".to_string()]);
        c.store.put_setting("passiveInScope", &json!(true)).unwrap();
        let scoped = call_tool(&c, "passive_scan", &json!({})).await.unwrap();
        assert!(scoped.contains("\"scannedFlows\": 1"), "only the in-scope host: {scoped}");
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
        // Every tool is documented.
        for t in tool_defs().as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            assert!(d.contains(name), "docs missing tool {name}");
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
}
