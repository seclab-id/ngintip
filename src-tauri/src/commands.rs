//! Tauri commands invoked from the UI (`src/lib/api.js`).

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri, Version};
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tokio::net::TcpListener;

use crate::{
    body::{self, CAPTURE_LIMIT},
    brand,
    config::{self, Config, ProxyAuth},
    har,
    intercept::{Decision, FilterRule, InterceptFilter, PendingItem},
    proxy, raw,
    rules::Rule,
    scope,
    store::{self, FlowSummary},
    upstream::UpstreamProxy,
    AppState,
};

type CmdResult<T> = Result<T, String>;

pub struct Running {
    host: String,
    port: u16,
    task: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    running: bool,
    host: String,
    port: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetail {
    summary: FlowSummary,
    request_raw: String,
    response_raw: String,
    /// The viewer capped the request/response body; the full body can be
    /// fetched with `flow_body`.
    request_truncated_view: bool,
    response_truncated_view: bool,
    /// True when the response is an image; the preview loads on demand via
    /// `flow_image` so opening a flow never base64-encodes an image it won't show.
    response_is_image: bool,
    /// True when the body looks binary, so the UI offers a hex Pretty view (loaded on
    /// demand via `flow_hex`). The Raw view always shows the body as-is.
    request_is_binary: bool,
    response_is_binary: bool,
    /// Earlier request/response versions (Original / Extension), when captured.
    variants: store::FlowVariants,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaInfo {
    path: String,
    pem: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeaterResponse {
    pub status: u16,
    pub duration_ms: u64,
    pub length: usize,
    pub response_raw: String,
    pub response_image: Option<String>,
    /// The request actually sent when a repeater-scope extension changed it (the
    /// typed request stays in the editor as the "original").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_final: Option<String>,
    /// The response before a repeater-scope extension transformed it, so the UI can
    /// offer an Original view next to the transformed ("final") one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_original: Option<String>,
}

async fn stop(state: &AppState) {
    state.ctx.set_listener(None);
    let running = state.server.lock().unwrap().take();
    if let Some(r) = running {
        r.task.abort();
        // Wait so the listener is dropped before a re-bind on the same port.
        let _ = r.task.await;
    }
}

/// Bind `port`, or the next free port after it, so a second Ngintip instance (or any
/// app holding the port) doesn't block startup. Returns the listener and the port it
/// actually bound. Only "address in use" rolls forward; other errors fail fast.
async fn bind_proxy(host: &str, port: u16) -> Result<(TcpListener, u16), String> {
    const MAX_TRIES: u16 = 20;
    let mut last = String::new();
    for offset in 0..MAX_TRIES {
        let p = port.saturating_add(offset);
        match TcpListener::bind((host, p)).await {
            Ok(listener) => {
                let actual = listener.local_addr().map(|a| a.port()).unwrap_or(p);
                return Ok((listener, actual));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => last = format!("{host}:{p} in use"),
            Err(e) => return Err(format!("Failed to listen on {host}:{p}: {e}")),
        }
    }
    Err(format!("No free port in {port}..{} ({last})", port.saturating_add(MAX_TRIES - 1)))
}

#[tauri::command]
pub async fn start_proxy(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> CmdResult<ProxyStatus> {
    stop(&state).await;
    let (listener, bound) = bind_proxy(&host, port).await?;
    let task = tauri::async_runtime::spawn(proxy::serve(listener, state.ctx.clone()));
    if bound != port {
        state.ctx.log_info("proxy", format!("port {port} busy; using {host}:{bound} instead (another proxy?)"));
    }
    state.ctx.log_info("proxy", format!("proxy started on {host}:{bound}"));
    *state.server.lock().unwrap() = Some(Running {
        host: host.clone(),
        port: bound,
        task,
    });
    Ok(ProxyStatus {
        running: true,
        host,
        port: bound,
    })
}

#[tauri::command]
pub async fn stop_proxy(state: State<'_, AppState>) -> CmdResult<()> {
    stop(&state).await;
    state.ctx.log_info("proxy", "proxy stopped");
    Ok(())
}

#[tauri::command]
pub fn proxy_status(state: State<AppState>) -> Option<ProxyStatus> {
    state.server.lock().unwrap().as_ref().map(|r| ProxyStatus {
        running: true,
        host: r.host.clone(),
        port: r.port,
    })
}

#[tauri::command]
pub fn list_flows(state: State<AppState>) -> CmdResult<Vec<FlowSummary>> {
    state.ctx.store.summaries().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_flow(state: State<AppState>, id: u64) -> CmdResult<Option<FlowDetail>> {
    let flow = state.ctx.store.get(id).map_err(|e| e.to_string())?;
    Ok(flow.map(|f| FlowDetail {
        request_raw: raw::request_text(&f),
        response_raw: raw::response_text(&f),
        request_truncated_view: raw::view_truncated(&f.req_headers, &f.req_body),
        response_truncated_view: raw::view_truncated(&f.res_headers, &f.res_body),
        response_is_image: raw::is_image(&f.res_headers, &f.res_body),
        request_is_binary: raw::body_is_binary(&f.req_headers, &f.req_body),
        response_is_binary: raw::body_is_binary(&f.res_headers, &f.res_body),
        variants: f.variants,
        summary: f.summary,
    }))
}

/// Hex dump of a flow's request or response body, for the Pretty (hex) view. Fetched
/// on demand so the Raw view stays as-is and opening a flow never builds a hex dump.
#[tauri::command]
pub fn flow_hex(state: State<AppState>, id: u64, side: String) -> CmdResult<Option<String>> {
    let flow = state.ctx.store.get(id).map_err(|e| e.to_string())?;
    Ok(flow.map(|f| {
        if side == "request" {
            raw::hex_view(&f.req_headers, &f.req_body)
        } else {
            raw::hex_view(&f.res_headers, &f.res_body)
        }
    }))
}

/// Base64 `data:` URL for an image response, fetched only when the Preview tab is
/// opened (so listing/opening flows never encodes images eagerly).
#[tauri::command]
pub fn flow_image(state: State<AppState>, id: u64) -> CmdResult<Option<String>> {
    let flow = state.ctx.store.get(id).map_err(|e| e.to_string())?;
    Ok(flow.and_then(|f| raw::image_data_url(&f.res_headers, &f.res_body)))
}

/// Messages of a WebSocket flow (status 101), oldest first.
/// Full request or response text (no view cap) for the viewer's Show all.
#[tauri::command]
pub fn flow_body(state: State<AppState>, id: u64, side: String) -> CmdResult<Option<String>> {
    let flow = state.ctx.store.get(id).map_err(|e| e.to_string())?;
    Ok(flow.map(|f| if side == "request" { raw::request_text_full(&f) } else { raw::response_text_full(&f) }))
}

#[tauri::command]
pub fn ws_messages(state: State<AppState>, flow_id: u64) -> CmdResult<Vec<store::WsMessage>> {
    state.ctx.store.ws_messages(flow_id).map_err(|e| e.to_string())
}

/// Save the user's note and highlight colour for a flow.
#[tauri::command]
pub fn set_flow_meta(state: State<AppState>, id: u64, note: String, color: String) -> CmdResult<()> {
    state
        .ctx
        .store
        .set_meta(id, note.trim(), &color)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_flows(state: State<AppState>, ids: Vec<u64>) -> CmdResult<()> {
    state.ctx.store.delete(&ids).map_err(|e| e.to_string())
}

/// Reseed the flow-id counter to the current max id (compacts numbering after a delete).
#[tauri::command]
pub fn reset_flow_ids(state: State<AppState>) -> CmdResult<()> {
    state.ctx.store.reset_ids().map_err(|e| e.to_string())
}

/// IDs of flows whose headers, (decoded) bodies or WebSocket messages contain
/// `query`, case-insensitive.
#[tauri::command]
pub async fn search_flows(state: State<'_, AppState>, query: String) -> CmdResult<Vec<u64>> {
    let ctx = state.ctx.clone();
    tauri::async_runtime::spawn_blocking(move || ctx.store.search(&query))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Project setting used only by the UI (e.g. Repeater tabs), as JSON.
#[tauri::command]
pub fn get_setting(state: State<AppState>, key: String) -> CmdResult<Option<serde_json::Value>> {
    state.ctx.store.get_setting(&key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_setting(state: State<AppState>, key: String, value: serde_json::Value) -> CmdResult<()> {
    state.ctx.store.put_setting(&key, &value).map_err(|e| e.to_string())
}

/// Settings key holding the project's scope patterns.
pub const SCOPE_KEY: &str = "scope";

#[tauri::command]
pub fn get_scope(state: State<AppState>) -> Vec<String> {
    state.ctx.scope.get()
}

/// Replace the scope; returns the normalized patterns actually saved.
#[tauri::command]
pub fn set_scope(state: State<AppState>, patterns: Vec<String>) -> CmdResult<Vec<String>> {
    let patterns = scope::normalize(patterns);
    state
        .ctx
        .store
        .put_list(SCOPE_KEY, &patterns)
        .map_err(|e| e.to_string())?;
    state.ctx.scope.set(patterns.clone());
    Ok(patterns)
}

/// Settings key holding the project's Match & Replace rules.
pub const RULES_KEY: &str = "match_replace";

fn saved_rules(ctx: &proxy::Ctx) -> Vec<Rule> {
    ctx.store
        .get_setting(RULES_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Activate the open project's rules (invalid ones were never saved).
pub fn load_rules(ctx: &proxy::Ctx) {
    if let Err(e) = ctx.rules.set(&saved_rules(ctx)) {
        eprintln!("[rules] {e}");
    }
}

/// Try a rule set against a pasted sample without saving or affecting traffic.
#[tauri::command]
pub fn test_rules(rules: Vec<Rule>, sample: String, response: bool) -> CmdResult<String> {
    let engine = crate::rules::Rules::default();
    engine.set(&rules)?;
    engine.apply_to_raw(&sample, response)
}

#[tauri::command]
pub fn get_rules(state: State<AppState>) -> Vec<Rule> {
    saved_rules(&state.ctx)
}

/// Validate, activate and save Match & Replace rules for this project.
#[tauri::command]
pub fn set_rules(state: State<AppState>, rules: Vec<Rule>) -> CmdResult<()> {
    state.ctx.rules.set(&rules)?;
    let value = serde_json::to_value(&rules).map_err(|e| e.to_string())?;
    state.ctx.store.put_setting(RULES_KEY, &value).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetInterface {
    name: String,
    ip: String,
}

/// Addresses the proxy can listen on: all interfaces, loopback, then each IPv4 NIC.
#[tauri::command]
pub fn list_interfaces() -> Vec<NetInterface> {
    let mut out = vec![
        NetInterface { name: "All interfaces".into(), ip: "0.0.0.0".into() },
        NetInterface { name: "Loopback".into(), ip: "127.0.0.1".into() },
    ];
    let mut nics: Vec<NetInterface> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.ip().is_ipv4() && !i.is_loopback())
        .map(|i| NetInterface { ip: i.ip().to_string(), name: i.name })
        .collect();
    nics.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(nics);
    out
}

#[tauri::command]
pub fn clear_flows(state: State<AppState>) -> CmdResult<()> {
    state.ctx.store.clear().map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    path: String,
    name: String,
}

/// The project the session is saved to/from: the document if one is set,
/// otherwise the live working file (an unsaved project).
fn project_info_of(state: &AppState) -> ProjectInfo {
    let path = state.document.lock().unwrap().clone().unwrap_or_else(|| state.ctx.store.path());
    ProjectInfo {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.display().to_string(),
    }
}

#[tauri::command]
pub fn project_info(state: State<AppState>) -> ProjectInfo {
    project_info_of(&state)
}

/// Build the search index for flows saved before it existed.
pub fn index_in_background(ctx: &std::sync::Arc<proxy::Ctx>) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if let Err(e) = ctx.store.backfill_search_index() {
            eprintln!("[search] indexing failed: {e:#}");
        }
    });
}

// --- Project file format & working-copy model ---------------------------------
//
// A project document is compressed SQLite (zstd; gzip in older saves) behind a small
// header. The app never edits it in place: it works on a live, uncompressed "working"
// copy under
// <data_dir>/working, and Save compresses that back to the document. This keeps
// documents small while the live DB stays a real SQLite file that auto-persists.

/// Magic prefix for a compressed project (see [`crate::brand`]): the brand magic
/// + a version byte, then the compressed SQLite. Plain SQLite files
/// ("SQLite format 3\0") are still opened directly, so older uncompressed
/// projects keep working. The legacy "CEGATC" compressed format is not supported.
///
/// Version 1 = gzip (older saves, still read). Version 2 = zstd (current): faster to
/// save and smaller, with long-distance matching for cross-flow repeats.
const PROJECT_VERSION: u8 = 2;
const PROJECT_VERSION_GZIP: u8 = 1;

/// The magic that prefixes `bytes`, if it's a compressed project (current or a
/// legacy brand). Every magic is the same length, so this doubles as the header
/// magic length.
fn project_magic(bytes: &[u8]) -> Option<&'static [u8]> {
    std::iter::once(brand::MAGIC)
        .chain(brand::LEGACY_MAGICS.iter().copied())
        .find(|m| bytes.len() > m.len() && &bytes[..m.len()] == *m)
}

/// Pointer files in the app data dir: the current working file and the document.
const WORKING_PTR: &str = "working_ptr";
const DOCUMENT_PTR: &str = "document_ptr";
/// Legacy (pre-compression) last-project pointer, read once for migration.
const LAST_PROJECT: &str = "last_project";

fn human_size(n: usize) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

pub fn is_compressed(bytes: &[u8]) -> bool {
    project_magic(bytes).is_some()
}

/// zstd level for saving. High enough to beat gzip's ratio comfortably, low enough
/// that multithreading keeps it faster than the old gzip-6 save on a typical machine.
const ZSTD_LEVEL: i32 = 12;

// In-memory compressor. Production Save now streams via `compress_file_to` (flat memory);
// this is kept as the reference/round-trip counterpart exercised by the tests.
#[cfg(test)]
pub fn compress_bytes(raw: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut out = Vec::with_capacity(raw.len() / 3 + brand::MAGIC.len() + 1);
    out.extend_from_slice(brand::MAGIC);
    out.push(PROJECT_VERSION);

    let mut enc = zstd::stream::write::Encoder::new(&mut out, ZSTD_LEVEL).map_err(|e| format!("Failed to compress: {e}"))?;
    // Best-effort tuning; each returns Err only when unsupported, in which case we
    // just compress a little slower / larger rather than failing the save. Use every
    // core (capped at 16 for sanity): more workers = faster save on big projects.
    let workers = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1).min(16);
    let _ = enc.multithread(workers);
    let _ = enc.long_distance_matching(true);
    let _ = enc.include_contentsize(true);
    enc.write_all(raw).map_err(|e| format!("Failed to compress: {e}"))?;
    enc.finish().map_err(|e| format!("Failed to compress: {e}"))?;
    Ok(out)
}

/// Stream `src` through the zstd encoder straight into `dest` (project MAGIC+version
/// header first), using a fixed buffer regardless of file size. The Save path uses this
/// instead of reading the whole SQLite file into a Vec and compressing it in RAM: a
/// multi-GB project no longer needs multi-GB of peak memory to save. The on-disk format
/// is byte-compatible with `compress_bytes` (same header + zstd stream), so
/// `decompress_bytes` reads either. Returns (uncompressed size, compressed size).
fn compress_file_to(src: &Path, dest: &Path) -> Result<(u64, u64), String> {
    use std::io::Write;
    let raw_len = std::fs::metadata(src).map_err(|e| format!("Failed to read project: {e}"))?.len();
    let mut rdr = std::fs::File::open(src).map_err(|e| format!("Failed to read project: {e}"))?;
    let mut dst = std::fs::File::create(dest).map_err(|e| format!("Failed to write {}: {e}", dest.display()))?;
    dst.write_all(brand::MAGIC).map_err(|e| format!("Failed to write: {e}"))?;
    dst.write_all(&[PROJECT_VERSION]).map_err(|e| format!("Failed to write: {e}"))?;

    let mut enc = zstd::stream::write::Encoder::new(&mut dst, ZSTD_LEVEL).map_err(|e| format!("Failed to compress: {e}"))?;
    let workers = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1).min(16);
    let _ = enc.multithread(workers);
    let _ = enc.long_distance_matching(true);
    let _ = enc.include_contentsize(true);
    std::io::copy(&mut rdr, &mut enc).map_err(|e| format!("Failed to compress: {e}"))?;
    enc.finish().map_err(|e| format!("Failed to compress: {e}"))?;

    let comp_len = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    Ok((raw_len, comp_len))
}

pub fn decompress_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let Some(magic) = project_magic(bytes) else {
        return Err(format!("Not a {} project file", brand::NAME));
    };
    let header = magic.len() + 1;
    let version = bytes[magic.len()];
    let body = &bytes[header..];
    match version {
        PROJECT_VERSION => zstd::stream::decode_all(body).map_err(|e| format!("Failed to decompress: {e}")),
        PROJECT_VERSION_GZIP => {
            // Projects saved by older versions use gzip.
            let mut dec = flate2::read::GzDecoder::new(body);
            let mut raw = Vec::new();
            dec.read_to_end(&mut raw).map_err(|e| format!("Failed to decompress: {e}"))?;
            Ok(raw)
        }
        v => Err(format!("Unsupported .{} format version {v}", brand::PROJECT_EXT)),
    }
}

fn read_ptr(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

fn write_ptr(dir: &Path, name: &str, path: &Path) {
    let _ = std::fs::write(dir.join(name), path.to_string_lossy().as_ref());
}

fn working_dir(dir: &Path) -> PathBuf {
    dir.join("working")
}

fn fresh_working_path(dir: &Path) -> PathBuf {
    let wd = working_dir(dir);
    let _ = std::fs::create_dir_all(&wd);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    wd.join(format!("{}-{}.cegat", std::process::id(), nanos))
}

fn remove_working_file(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suf in ["-wal", "-shm"] {
        let mut s = path.as_os_str().to_owned();
        s.push(suf);
        let _ = std::fs::remove_file(PathBuf::from(s));
    }
}

/// Delete working files left over from previous runs, keeping `keep` (and its
/// WAL sidecars, which share its path prefix).
fn clean_stray_working(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(working_dir(dir)) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if let Some(k) = keep {
            if p.to_string_lossy().starts_with(k.to_string_lossy().as_ref()) {
                continue;
            }
        }
        let _ = std::fs::remove_file(&p);
    }
}

/// Point the live store at a new working file, remembering it and deleting the
/// previous one.
fn switch_working(dir: &Path, state: &AppState, wp: &Path) -> CmdResult<()> {
    let old = read_ptr(dir, WORKING_PTR);
    state.ctx.store.switch(wp).map_err(|e| format!("Failed to open project: {e:#}"))?;
    write_ptr(dir, WORKING_PTR, wp);
    if let Some(old) = old {
        if old != wp {
            remove_working_file(&old);
        }
    }
    Ok(())
}

/// Reload scope, Match & Replace rules, extensions and the search index from the
/// live store, so everything reflects the project that was just opened.
fn adopt(state: &AppState) -> CmdResult<()> {
    let scope = state.ctx.store.get_list(SCOPE_KEY).map_err(|e| e.to_string())?;
    state.ctx.scope.set(scope);
    load_rules(&state.ctx);
    load_extensions_into(&state.ctx);
    index_in_background(&state.ctx);
    Ok(())
}

/// Record the current document path (or clear it) in memory and on disk.
fn set_document(dir: &Path, state: &AppState, doc: Option<&Path>) {
    *state.document.lock().unwrap() = doc.map(|p| p.to_path_buf());
    // Interpreter + venv are per-project now (in the project's settings): point the
    // manager at this project's folder and apply its saved interpreter/venv.
    apply_project_python_venv(&state.ctx, dir, doc);
    match doc {
        Some(p) => write_ptr(dir, DOCUMENT_PTR, p),
        None => {
            let _ = std::fs::remove_file(dir.join(DOCUMENT_PTR));
        }
    }
}

/// Open the store at startup: reuse the last working file if it's still there
/// (so unsaved edits survive a restart), otherwise load the document (or migrate
/// the legacy last-project) into a fresh working copy.
pub fn open_startup(dir: &Path) -> anyhow::Result<(store::Store, Option<PathBuf>)> {
    let document = read_ptr(dir, DOCUMENT_PTR)
        .or_else(|| read_ptr(dir, LAST_PROJECT))
        .filter(|p| p.is_file());

    if let Some(wp) = read_ptr(dir, WORKING_PTR).filter(|p| p.is_file()) {
        clean_stray_working(dir, Some(&wp));
        return Ok((store::Store::open(&wp)?, document));
    }

    let wp = fresh_working_path(dir);
    if let Some(doc) = &document {
        let bytes = std::fs::read(doc)?;
        let raw = if is_compressed(&bytes) {
            decompress_bytes(&bytes).map_err(|e| anyhow::anyhow!("{e}"))?
        } else {
            bytes
        };
        std::fs::write(&wp, &raw)?;
    }
    let store = store::Store::open(&wp)?;
    write_ptr(dir, WORKING_PTR, &wp);
    clean_stray_working(dir, Some(&wp));
    Ok((store, document))
}

/// Start a fresh, empty project (no document until the user saves).
#[tauri::command]
pub fn blank_project(app: tauri::AppHandle, state: State<AppState>) -> CmdResult<ProjectInfo> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let wp = fresh_working_path(&dir);
    switch_working(&dir, &state, &wp)?;
    state.ctx.scope.set(Vec::new());
    load_rules(&state.ctx);
    set_document(&dir, &state, None);
    // A blank project has no extensions (its store has none), so this clears the
    // previous project's ones from the running manager.
    load_extensions_into(&state.ctx);
    Ok(project_info_of(&state))
}

/// Open a project file (compressed `.cegat` or a legacy plain SQLite one) by
/// loading it into a fresh working copy.
#[tauri::command]
pub fn open_project(app: tauri::AppHandle, state: State<AppState>, path: String) -> CmdResult<ProjectInfo> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("Failed to read {path}: {e}"))?;
    let raw = if is_compressed(&bytes) { decompress_bytes(&bytes)? } else { bytes };

    let wp = fresh_working_path(&dir);
    std::fs::write(&wp, &raw).map_err(|e| format!("Failed to open project: {e}"))?;
    switch_working(&dir, &state, &wp)?;
    // Point the manager at the project folder + its saved interpreter/venv FIRST, so the
    // extension set that `adopt` loads starts its workers with the right interpreter.
    set_document(&dir, &state, Some(Path::new(&path)));
    adopt(&state)?;
    state.ctx.log_info("project", format!("opened project {path}"));
    Ok(project_info_of(&state))
}

/// Result of saving a project: sizes so the UI can show the compression win.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    name: String,
    path: String,
    raw_size: usize,
    compressed_size: usize,
    percent: usize,
    summary: String,
}

fn make_save_result(path: &Path, raw_len: usize, out_len: usize) -> SaveResult {
    let percent = if raw_len == 0 { 0 } else { 100 - (out_len * 100 / raw_len).min(100) };
    SaveResult {
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        path: path.display().to_string(),
        raw_size: raw_len,
        compressed_size: out_len,
        percent,
        summary: format!("{} -> {} ({percent}% smaller)", human_size(raw_len), human_size(out_len)),
    }
}

/// VACUUM the project to a temp file, then stream-compress it straight to `dest` via a
/// sibling temp + atomic rename. Streaming keeps peak memory flat (a multi-GB project no
/// longer loads wholesale into RAM to save), and the rename means a failure or crash
/// mid-write never corrupts the user's existing project file. Returns (raw size, bytes).
fn snapshot_compress_to(ctx: &proxy::Ctx, dest: &Path) -> Result<(u64, u64), String> {
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
    );
    // 1. Defragmented copy of the live DB to a temp file (VACUUM INTO).
    let vac = std::env::temp_dir().join(format!("{}-snapshot-{stamp}.{}", brand::ID, brand::PROJECT_EXT));
    ctx.store.save_as(&vac).map_err(|e| format!("Failed to read project: {e:#}"))?;
    // 2. Compress it into a sibling temp, then atomically swap it in.
    let out_tmp = dest.with_file_name(format!(
        ".{}.{stamp}.part",
        dest.file_name().and_then(|s| s.to_str()).unwrap_or("project"),
    ));
    let res = compress_file_to(&vac, &out_tmp);
    let _ = std::fs::remove_file(&vac);
    match res {
        Ok(sizes) => match std::fs::rename(&out_tmp, dest) {
            Ok(()) => Ok(sizes),
            Err(e) => {
                let _ = std::fs::remove_file(&out_tmp);
                Err(format!("Failed to write {}: {e}", dest.display()))
            }
        },
        Err(e) => {
            let _ = std::fs::remove_file(&out_tmp);
            Err(e)
        }
    }
}

/// Snapshot the live project, compress it, and write it to `path` as the document.
async fn write_document(dir: PathBuf, state: State<'_, AppState>, path: PathBuf) -> CmdResult<SaveResult> {
    // Capture the latest script text (in case a file was edited without a UI change)
    // so the saved project carries a current, portable copy, and the debug console so
    // it travels with the project too.
    refresh_extension_bodies(&state.ctx);
    persist_extension_logs(&state.ctx);
    // Snapshot + compress off the main thread so a big project does not freeze the UI.
    let ctx = state.ctx.clone();
    let dest = path.clone();
    let (raw_len, comp_len) = tauri::async_runtime::spawn_blocking(move || snapshot_compress_to(&ctx, &dest))
        .await
        .map_err(|e| e.to_string())??;
    set_document(&dir, &state, Some(path.as_path()));
    state.ctx.log_info("project", format!("saved project to {}", path.display()));
    Ok(make_save_result(&path, raw_len as usize, comp_len as usize))
}

/// Save (compress) the current project to a chosen `.cegat` path.
#[tauri::command]
pub async fn save_project_as(app: tauri::AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<SaveResult> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    write_document(dir, state, PathBuf::from(path)).await
}

/// Save (compress) the current project to its existing document path.
#[tauri::command]
pub async fn save_project(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<SaveResult> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let doc = state
        .document
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "No project file yet. Use Save Project As first.".to_string())?;
    write_document(dir, state, doc).await
}

/// Recent log lines shown in the Logs view.
#[tauri::command]
pub fn logs(state: State<AppState>) -> Vec<proxy::LogEntry> {
    state.ctx.logs()
}

#[tauri::command]
pub fn clear_logs(state: State<AppState>) {
    state.ctx.clear_logs();
}

/// Delete specific log lines by id.
#[tauri::command]
pub fn delete_logs(state: State<AppState>, ids: Vec<u64>) -> CmdResult<()> {
    state.ctx.store.delete_logs(&ids).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterceptStatus {
    enabled: bool,
    responses: bool,
    pending: Vec<PendingItem>,
}

#[tauri::command]
pub fn intercept_status(state: State<AppState>) -> InterceptStatus {
    InterceptStatus {
        enabled: state.ctx.intercept.is_enabled(),
        responses: state.ctx.intercept.holds_responses(),
        pending: state.ctx.intercept.pending(),
    }
}

#[tauri::command]
pub fn set_intercept(state: State<AppState>, enabled: bool) {
    state.ctx.intercept.set_enabled(enabled);
}

/// Also hold every in-scope response (not only ones asked for per request).
#[tauri::command]
pub fn set_intercept_responses(state: State<AppState>, enabled: bool) {
    state.ctx.intercept.set_responses(enabled);
}

/// Forward a held message; `raw` is the edited text, or null to send it unchanged.
/// `hold_response` (requests only) holds the response to this request as well.
#[tauri::command]
pub fn intercept_forward(state: State<AppState>, key: u64, raw: Option<String>, hold_response: bool) -> bool {
    state.ctx.intercept.resolve(key, Decision::Forward { raw, hold_response })
}

#[tauri::command]
pub fn intercept_drop(state: State<AppState>, key: u64) -> bool {
    state.ctx.intercept.resolve(key, Decision::Drop)
}

/// Replace the advanced intercept filter (which in-scope messages get held).
#[tauri::command]
pub fn set_intercept_filter(state: State<AppState>, rules: Vec<FilterRule>) {
    state.ctx.intercept.set_filter(InterceptFilter { rules });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsInterceptStatus {
    enabled: bool,
    pending: Vec<crate::ws::WsPendingItem>,
}

#[tauri::command]
pub fn ws_intercept_status(state: State<AppState>) -> WsInterceptStatus {
    WsInterceptStatus {
        enabled: state.ctx.ws_intercept.is_enabled(),
        pending: state.ctx.ws_intercept.pending(),
    }
}

/// Toggle WebSocket intercept. Applies to connections opened while it is on.
#[tauri::command]
pub fn set_ws_intercept(state: State<AppState>, enabled: bool) {
    state.ctx.ws_intercept.set_enabled(enabled);
}

/// Forward a held WebSocket message; `text` is the edited payload (sent as UTF-8), or
/// null to send it unchanged.
#[tauri::command]
pub fn ws_intercept_forward(state: State<AppState>, key: u64, text: Option<String>) -> bool {
    let data = text.map(|t| t.into_bytes());
    state.ctx.ws_intercept.resolve(key, crate::ws::WsDecision::Forward { data })
}

#[tauri::command]
pub fn ws_intercept_drop(state: State<AppState>, key: u64) -> bool {
    state.ctx.ws_intercept.resolve(key, crate::ws::WsDecision::Drop)
}

/// Process/system resource usage for the Settings > Usage panel.
#[tauri::command]
pub fn usage_stats(state: State<AppState>) -> crate::usage::UsageStats {
    state.usage.lock().unwrap().snapshot()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashInfo {
    dir: Option<String>,
    count: usize,
    latest: Option<String>,
}

/// Where crash dumps live, how many there are, and the newest one's name.
#[tauri::command]
pub fn crash_info() -> CrashInfo {
    let s = crate::crash::summary();
    CrashInfo { dir: s.dir, count: s.count, latest: s.latest }
}

// ---- Python extensions ----

use crate::extensions::ExtensionSpec;

/// Settings keys holding the project's extensions and everything that belongs with
/// them: the master on/off state, the Python interpreter, the custom venv directory
/// and the debug console. These all live in the project (so they travel in the
/// .ngintip and a blank project starts clean), matching the per-extension on/off state
/// that already rides inside each spec.
pub const EXTENSIONS_KEY: &str = "extensions";
pub const EXTENSIONS_ENABLED_KEY: &str = "extensions_enabled";
pub const PYTHON_PATH_KEY: &str = "python_path";
pub const VENV_PATH_KEY: &str = "venv_path";
pub const EXTENSION_LOGS_KEY: &str = "extension_logs";

/// A non-empty, trimmed string setting from the open project, or None.
fn project_setting_string(ctx: &proxy::Ctx, key: &str) -> Option<String> {
    ctx.store
        .get_setting(key)
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(str::to_string))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Point the extension manager at the open project's folder (the default venv location)
/// and apply this project's saved Python interpreter and custom venv path. The script
/// list, on/off state and debug console come separately via `load_extensions_into`.
pub fn apply_project_python_venv(ctx: &proxy::Ctx, base_dir: &Path, doc: Option<&Path>) {
    let venv = project_setting_string(ctx, VENV_PATH_KEY).map(PathBuf::from);
    ctx.extensions.configure(base_dir.to_path_buf(), doc.map(|p| p.to_path_buf()), venv);
    ctx.extensions.set_python(project_setting_string(ctx, PYTHON_PATH_KEY));
}

/// Save the current debug-console lines into the project so they travel in the .ngintip
/// and come back when it is reopened.
pub fn persist_extension_logs(ctx: &proxy::Ctx) {
    if let Ok(v) = serde_json::to_value(ctx.extensions.logs()) {
        let _ = ctx.store.put_setting(EXTENSION_LOGS_KEY, &v);
    }
}

/// The extensions saved in the current project (with their embedded script copies).
fn stored_extensions(ctx: &proxy::Ctx) -> Vec<ExtensionSpec> {
    ctx.store
        .get_setting(EXTENSIONS_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Load the current project's extensions (list + on/off) into the running manager.
/// Called on startup and on every project open/blank so the set is per-project.
pub fn load_extensions_into(ctx: &proxy::Ctx) {
    let specs = stored_extensions(ctx);
    let enabled = ctx
        .store
        .get_setting(EXTENSIONS_ENABLED_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let logs: Vec<String> = ctx
        .store
        .get_setting(EXTENSION_LOGS_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    ctx.extensions.load(specs, enabled, logs);
}

/// Persist the extension list into the project and apply it. Each spec's embedded
/// script copy is refreshed from its live file when that file still exists (so the
/// project always carries the latest source); when the file is gone, the previously
/// stored copy is kept (the frontend never sends the body back).
fn persist_extensions(ctx: &proxy::Ctx, incoming: Vec<ExtensionSpec>) -> CmdResult<()> {
    let prev = stored_extensions(ctx);
    let merged: Vec<ExtensionSpec> = incoming
        .into_iter()
        .map(|mut spec| {
            if !spec.path.is_empty() {
                if let Ok(src) = std::fs::read_to_string(&spec.path) {
                    spec.body = src;
                }
            }
            if spec.body.is_empty() {
                if let Some(p) = prev.iter().find(|p| p.id == spec.id) {
                    spec.body = p.body.clone();
                }
            }
            spec
        })
        .collect();
    let value = serde_json::to_value(&merged).map_err(|e| e.to_string())?;
    ctx.store.put_setting(EXTENSIONS_KEY, &value).map_err(|e| e.to_string())?;
    ctx.extensions.set_extensions(merged);
    Ok(())
}

/// Before saving, re-read each extension's live script into its stored copy, so the
/// saved project carries the latest source even if the file was edited without any
/// UI change (a no-op when nothing changed).
pub fn refresh_extension_bodies(ctx: &proxy::Ctx) {
    let mut specs = stored_extensions(ctx);
    let mut changed = false;
    for s in &mut specs {
        if !s.path.is_empty() {
            if let Ok(src) = std::fs::read_to_string(&s.path) {
                if src != s.body {
                    s.body = src;
                    changed = true;
                }
            }
        }
    }
    if changed {
        if let Ok(v) = serde_json::to_value(&specs) {
            let _ = ctx.store.put_setting(EXTENSIONS_KEY, &v);
        }
    }
}

/// One-time move of any globally-configured extensions (old `config.json`) into the
/// currently open project, then clear them from config so they stop being global.
/// After this, extensions live only in projects.
pub fn migrate_config_extensions(ctx: &proxy::Ctx, config_path: &Path, config: &mut Config) {
    let mut config_changed = false;

    // The extension list + master on/off moved from config into the open project.
    if !config.extensions.is_empty() || config.extensions_enabled {
        // Only seed a project that has no extensions of its own yet (so the move happens
        // exactly once, into whatever project is open at upgrade time).
        let already = ctx.store.get_setting(EXTENSIONS_KEY).ok().flatten().is_some();
        if !already {
            let mut specs = config.extensions.clone();
            for s in &mut specs {
                if s.body.is_empty() && !s.path.is_empty() {
                    if let Ok(src) = std::fs::read_to_string(&s.path) {
                        s.body = src;
                    }
                }
            }
            if let Ok(v) = serde_json::to_value(&specs) {
                let _ = ctx.store.put_setting(EXTENSIONS_KEY, &v);
            }
            let _ = ctx.store.put_setting(EXTENSIONS_ENABLED_KEY, &serde_json::json!(config.extensions_enabled));
        }
        config.extensions.clear();
        config.extensions_enabled = false;
        config_changed = true;
    }

    // The interpreter + custom venv moved from config into the open project too; seed
    // them once (only if the project has none yet), then clear them from the config.
    if !config.python_path.is_empty() {
        if ctx.store.get_setting(PYTHON_PATH_KEY).ok().flatten().is_none() {
            let _ = ctx.store.put_setting(PYTHON_PATH_KEY, &serde_json::json!(config.python_path));
        }
        config.python_path.clear();
        config_changed = true;
    }
    if !config.venv_path.is_empty() {
        if ctx.store.get_setting(VENV_PATH_KEY).ok().flatten().is_none() {
            let _ = ctx.store.put_setting(VENV_PATH_KEY, &serde_json::json!(config.venv_path));
        }
        config.venv_path.clear();
        config_changed = true;
    }

    if config_changed {
        let _ = config::save(config_path, config);
    }
}

#[tauri::command]
pub fn extensions_status(state: State<AppState>) -> crate::extensions::ExtensionsStatus {
    state.ctx.extensions.status()
}

#[tauri::command]
pub fn detect_python() -> Option<String> {
    crate::extensions::detect_python()
}

#[tauri::command]
pub fn set_python_path(state: State<AppState>, path: String) -> CmdResult<crate::extensions::ExtensionsStatus> {
    let path = path.trim().to_string();
    // Per-project now: save into the open project's settings (travels in the .ngintip),
    // not into the global config.
    state
        .ctx
        .store
        .put_setting(PYTHON_PATH_KEY, &serde_json::json!(path))
        .map_err(|e| format!("Failed to save settings: {e:#}"))?;
    state.ctx.extensions.set_python((!path.is_empty()).then_some(path));
    Ok(state.ctx.extensions.status())
}

#[tauri::command]
pub fn set_extensions(
    state: State<AppState>,
    extensions: Vec<crate::extensions::ExtensionSpec>,
) -> CmdResult<crate::extensions::ExtensionsStatus> {
    // Extensions belong to the project now, so they are saved into the project's
    // settings (and travel in the .ngintip), not into the global config.
    persist_extensions(&state.ctx, extensions)?;
    Ok(state.ctx.extensions.status())
}

#[tauri::command]
pub fn set_extensions_enabled(state: State<AppState>, enabled: bool) -> CmdResult<crate::extensions::ExtensionsStatus> {
    state
        .ctx
        .store
        .put_setting(EXTENSIONS_ENABLED_KEY, &serde_json::json!(enabled))
        .map_err(|e| format!("Failed to save settings: {e:#}"))?;
    state.ctx.extensions.set_enabled(enabled);
    Ok(state.ctx.extensions.status())
}

/// Create the per-project venv and pip-install every extension's requirements.
/// Runs off the main thread (venv + pip can take a while).
#[tauri::command]
pub async fn setup_extension_venv(state: State<'_, AppState>) -> CmdResult<String> {
    let ctx = state.ctx.clone();
    tauri::async_runtime::spawn_blocking(move || ctx.extensions.setup_venv())
        .await
        .map_err(|e| e.to_string())?
}

/// Set (or clear, with "") a custom venv directory; persisted and applied.
#[tauri::command]
pub fn set_extension_venv_path(state: State<AppState>, path: String) -> CmdResult<crate::extensions::ExtensionsStatus> {
    let path = path.trim().to_string();
    // Per-project now: save into the open project's settings, not the global config.
    state
        .ctx
        .store
        .put_setting(VENV_PATH_KEY, &serde_json::json!(path))
        .map_err(|e| format!("Failed to save settings: {e:#}"))?;
    state.ctx.extensions.set_venv_path((!path.is_empty()).then(|| PathBuf::from(path)));
    Ok(state.ctx.extensions.status())
}

/// Install one or more pip packages into the venv (space/comma separated). Off-thread.
#[tauri::command]
pub async fn install_extension_package(state: State<'_, AppState>, packages: String) -> CmdResult<String> {
    let ctx = state.ctx.clone();
    let pkgs: Vec<String> = packages.split([' ', ',', '\n']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    tauri::async_runtime::spawn_blocking(move || ctx.extensions.install_packages(pkgs))
        .await
        .map_err(|e| e.to_string())?
}

/// Packages installed in the venv (`pip freeze`). Off-thread.
#[tauri::command]
pub async fn extension_packages(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let ctx = state.ctx.clone();
    tauri::async_runtime::spawn_blocking(move || ctx.extensions.installed_packages())
        .await
        .map_err(|e| e.to_string())
}

/// Starter extension script (the documented template) for "New from template".
#[tauri::command]
pub fn extension_template() -> String {
    include_str!("../../examples/template_extension.py").to_string()
}

#[tauri::command]
pub fn extension_logs(state: State<AppState>) -> Vec<String> {
    state.ctx.extensions.logs()
}

/// Dry-run one extension on a sample request/response and return the transformed
/// message plus whatever it printed. Runs off the main thread.
#[tauri::command]
pub async fn test_extension(
    state: State<'_, AppState>,
    id: String,
    phase: String,
    raw: String,
) -> CmdResult<crate::extensions::TestResult> {
    let ctx = state.ctx.clone();
    let ph = if phase == "response" { crate::extensions::Phase::Response } else { crate::extensions::Phase::Request };
    tauri::async_runtime::spawn_blocking(move || ctx.extensions.test(&id, ph, raw.as_bytes()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_extension_logs(state: State<AppState>) {
    state.ctx.extensions.clear_logs();
    // Keep the cleared console cleared when the project is next saved/reopened.
    let _ = state.ctx.store.put_setting(EXTENSION_LOGS_KEY, &serde_json::json!(Vec::<String>::new()));
}

/// Export the whole project history as a HAR file. Returns the entry count.
#[tauri::command]
pub async fn export_har(state: State<'_, AppState>, path: String) -> CmdResult<usize> {
    let flows = state.ctx.store.all().map_err(|e| e.to_string())?;
    let ws = state.ctx.store.all_ws().map_err(|e| e.to_string())?;
    let har = har::build(&flows, &ws);
    let json = serde_json::to_vec_pretty(&har).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| format!("Failed to write {path}: {e}"))?;
    Ok(flows.len())
}

/// Write raw bytes (base64) to a path. Used by the Repeater capture editor to
/// save an annotated PNG the webview produced.
#[tauri::command]
pub fn save_binary(path: String, data: String) -> CmdResult<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD.decode(data.as_bytes()).map_err(|e| format!("invalid image data: {e}"))?;
    std::fs::write(&path, bytes).map_err(|e| format!("Failed to write {path}: {e}"))
}

/// Put an image on the system clipboard from raw RGBA canvas pixels (base64). The
/// webview's async clipboard image API is unreliable, so the capture editor sends
/// `canvas.getImageData()` here and we copy it natively via arboard.
#[tauri::command]
pub fn copy_image(width: usize, height: usize, data: String) -> CmdResult<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD.decode(data.as_bytes()).map_err(|e| format!("invalid image data: {e}"))?;
    let expected = width.checked_mul(height).and_then(|n| n.checked_mul(4));
    if expected != Some(bytes.len()) || width == 0 || height == 0 {
        return Err(format!("image data does not match {width}x{height} RGBA"));
    }
    let img = arboard::ImageData { width, height, bytes: bytes.into() };
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_image(img))
        .map_err(|e| format!("clipboard: {e}"))
}

/// Export the whole history as a Burp Suite items XML file. Returns the count.
#[tauri::command]
pub async fn export_burp(state: State<'_, AppState>, path: String) -> CmdResult<usize> {
    let flows = state.ctx.store.all().map_err(|e| e.to_string())?;
    let xml = crate::burp::build(&flows);
    tokio::fs::write(&path, xml)
        .await
        .map_err(|e| format!("Failed to write {path}: {e}"))?;
    Ok(flows.len())
}

#[tauri::command]
pub fn get_config(state: State<AppState>) -> Config {
    state.config.lock().unwrap().clone()
}

// ---- MCP server -------------------------------------------------------------

pub struct McpRunning {
    port: u16,
    docs_path: String,
    task: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    running: bool,
    port: u16,
    docs_path: String,
}

/// Start the MCP server (127.0.0.1 only) on `port`, serving docs at `docs_path`.
pub async fn start_mcp(ctx: std::sync::Arc<proxy::Ctx>, port: u16, docs_path: String) -> Result<McpRunning, String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("Failed to start MCP server on 127.0.0.1:{port}: {e}"))?;
    let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let docs = if docs_path.trim().is_empty() { "/docs".to_string() } else { docs_path };
    let task = tauri::async_runtime::spawn(crate::mcp::serve(listener, ctx.clone(), docs.clone()));
    ctx.log_info("mcp", format!("MCP server on http://127.0.0.1:{bound}/mcp (docs {docs})"));
    Ok(McpRunning { port: bound, docs_path: docs, task })
}

async fn stop_mcp(state: &AppState) {
    let running = state.mcp.lock().unwrap().take();
    if let Some(r) = running {
        r.task.abort();
        let _ = r.task.await;
    }
}

/// Enable/disable the MCP server; persists the choice, port and docs path.
#[tauri::command]
pub async fn set_mcp(state: State<'_, AppState>, enabled: bool, port: u16, docs_path: String) -> CmdResult<McpStatus> {
    stop_mcp(&state).await;
    let want = if port == 0 { 8082 } else { port };
    let docs = if docs_path.trim().is_empty() { "/docs".to_string() } else { docs_path };
    let mut running = false;
    let mut bound = want;
    let mut docs_out = docs.clone();
    if enabled {
        let r = start_mcp(state.ctx.clone(), want, docs.clone()).await?;
        bound = r.port;
        docs_out = r.docs_path.clone();
        *state.mcp.lock().unwrap() = Some(r);
        running = true;
    }
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.mcp = enabled;
    next.mcp_port = bound;
    next.mcp_docs_path = docs_out.clone();
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    *config = next;
    Ok(McpStatus { running, port: bound, docs_path: docs_out })
}

#[tauri::command]
pub fn mcp_status(state: State<AppState>) -> McpStatus {
    if let Some(r) = state.mcp.lock().unwrap().as_ref() {
        return McpStatus { running: true, port: r.port, docs_path: r.docs_path.clone() };
    }
    let c = state.config.lock().unwrap();
    McpStatus {
        running: false,
        port: if c.mcp_port == 0 { 8082 } else { c.mcp_port },
        docs_path: if c.mcp_docs_path.is_empty() { "/docs".into() } else { c.mcp_docs_path.clone() },
    }
}

/// Toggle invisible proxying (non-proxy-aware clients routed by SNI/Host).
#[tauri::command]
pub fn set_invisible(state: State<AppState>, enabled: bool) -> CmdResult<()> {
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.invisible = enabled;
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    state.ctx.set_invisible(enabled);
    *config = next;
    state.ctx.log_info("proxy", format!("invisible proxy {}", if enabled { "enabled" } else { "disabled" }));
    Ok(())
}

/// Persist the fixed tokio worker-thread count (0 = auto / one per core). Read at startup,
/// so it takes effect on the next app launch, not immediately.
#[tauri::command]
pub fn set_threads(state: State<AppState>, threads: u16) -> CmdResult<()> {
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.threads = threads;
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    *config = next;
    Ok(())
}

/// Set (or clear, with "") the upstream proxy. Returns the saved URL.
#[tauri::command]
pub fn set_upstream(state: State<AppState>, url: String) -> CmdResult<String> {
    let url = url.trim().to_string();
    let proxy = UpstreamProxy::parse(&url).map_err(|e| e.to_string())?;
    if let Some(p) = &proxy {
        let own: Uri = format!("http://{}/", format_host_port(&p.host, p.port))
            .parse()
            .map_err(|e| format!("{e}"))?;
        if state.ctx.is_self(&own) {
            return Err(format!("Upstream proxy cannot point at {}'s own listener", brand::NAME));
        }
    }
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.upstream = url.clone();
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    state.ctx.set_upstream(proxy).map_err(|e| e.to_string())?;
    *config = next;
    Ok(url)
}

fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

/// Turn proxy authentication on/off; applies to new requests immediately.
#[tauri::command]
pub fn set_auth(state: State<AppState>, auth: ProxyAuth) -> CmdResult<()> {
    auth.validate()?;
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.auth = auth;
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    state.ctx.set_auth(next.auth.credentials());
    *config = next;
    Ok(())
}

/// Write the current CA to `path`. `.pem` gets PEM text; `.der`/`.crt`/`.cer`
/// get DER bytes (what Android and Windows import). Format follows the extension.
#[tauri::command]
pub fn export_ca(state: State<AppState>, path: String) -> CmdResult<()> {
    let lower = path.to_ascii_lowercase();
    let bytes = if lower.ends_with(".pem") {
        state.ctx.ca.pem().into_bytes()
    } else {
        state.ctx.ca.der()
    };
    std::fs::write(&path, bytes).map_err(|e| format!("Failed to write {path}: {e}"))
}

/// Open an http(s) URL in the system default browser. Used to open the proxy's
/// CA download page so the CA can be installed straight into the browser.
#[tauri::command]
pub fn open_url(url: String) -> CmdResult<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("Only http(s) URLs can be opened".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", &url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| format!("Failed to open browser: {e}"))
}

// ---- browser engine ---------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    /// Configured browser binary path (empty = not set).
    path: String,
    /// Whether that path (resolving a macOS .app to its executable) exists.
    exists: bool,
    /// Extra launch flags.
    flags: String,
    /// Whether an in-app download is supported on this OS/arch.
    can_download: bool,
}

fn browser_status_of(config: &Config) -> BrowserStatus {
    let path = config.browser_path.clone();
    let exists = !path.is_empty() && crate::browser::resolve_binary(&path).exists();
    BrowserStatus { path, exists, flags: config.browser_flags.clone(), can_download: crate::browser::cft_platform().is_some() }
}

#[tauri::command]
pub fn browser_status(state: State<AppState>) -> BrowserStatus {
    browser_status_of(&state.config.lock().unwrap())
}

/// Set (or clear) the browser binary path and launch flags.
#[tauri::command]
pub fn set_browser(state: State<AppState>, path: String, flags: String) -> CmdResult<BrowserStatus> {
    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.browser_path = path.trim().to_string();
    next.browser_flags = flags.trim().to_string();
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    *config = next;
    Ok(browser_status_of(&config))
}

/// Look for an installed Chrome/Chromium/Edge/Brave; if found, save it.
#[tauri::command]
pub fn detect_browser(state: State<AppState>) -> CmdResult<BrowserStatus> {
    let found = crate::browser::detect();
    let mut config = state.config.lock().unwrap();
    if let Some(path) = found {
        let mut next = config.clone();
        next.browser_path = path;
        config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
        *config = next;
    }
    Ok(browser_status_of(&config))
}

/// Download the latest Chrome for Testing into the app data dir and set it as the
/// browser. Emits `browser-download` progress events while it runs.
#[tauri::command]
pub async fn download_browser(app: tauri::AppHandle, state: State<'_, AppState>) -> CmdResult<BrowserStatus> {
    let dir = state
        .config_path
        .parent()
        .map(|p| p.join("browser"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("{}-browser", brand::ID)));
    state.ctx.log_info("browser", format!("downloading Chrome for Testing to {}", dir.display()));

    let app2 = app.clone();
    let last = std::sync::atomic::AtomicU64::new(u64::MAX);
    let path = crate::browser::download(&dir, |downloaded, total| {
        let pct = total.map(|t| if t > 0 { downloaded * 100 / t } else { 0 });
        let key = pct.unwrap_or(downloaded / (512 * 1024));
        if last.swap(key, std::sync::atomic::Ordering::Relaxed) != key {
            let _ = app2.emit("browser-download", serde_json::json!({ "downloaded": downloaded, "total": total, "pct": pct }));
        }
    })
    .await
    .map_err(|e| {
        let _ = app.emit("browser-download", serde_json::json!({ "error": e }));
        format!("Download failed: {e}")
    })?;

    let mut config = state.config.lock().unwrap();
    let mut next = config.clone();
    next.browser_path = path.clone();
    config::save(&state.config_path, &next).map_err(|e| format!("Failed to save settings: {e:#}"))?;
    *config = next;
    state.ctx.log_info("browser", format!("browser ready at {path}"));
    let _ = app.emit("browser-download", serde_json::json!({ "done": true, "path": path }));
    Ok(browser_status_of(&config))
}

/// Close any browser this app launched (used by "Close Project" and on app exit).
#[tauri::command]
pub fn close_launched_browsers(state: State<AppState>) {
    state.kill_browsers();
}

/// Whether a browser this app launched is still running (reaps exited ones). Lets the
/// UI show "Browser opened" while it's up and revert once it's closed.
#[tauri::command]
pub fn browser_running(state: State<AppState>) -> bool {
    if let Ok(mut list) = state.browsers.lock() {
        list.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        !list.is_empty()
    } else {
        false
    }
}

/// Where this project's PERSISTENT browser profiles live: `<project dir>/ngintip_browser/`
/// when the project has been saved, otherwise under the app data dir. The folder for one
/// profile is `<project name>_<profile>` (e.g. `myproject_default`, `myproject_work`), so
/// cookies/history are kept per project and per profile. Names are sanitised to safe dir
/// components.
fn browser_profile_dir(state: &AppState, name: &str) -> PathBuf {
    let doc = state.document.lock().unwrap().clone();
    let fallback = state.config_path.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let (base, stem) = match doc {
        Some(p) => (
            p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| fallback.clone()),
            p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled".into()),
        ),
        None => (fallback, "untitled".to_string()),
    };
    base.join(format!("{}_browser", brand::ID))
        .join(format!("{}_{}", crate::extensions::sanitize_name(&stem), crate::extensions::sanitize_name(name)))
}

/// Launch the configured browser wired to `proxy`. `profile` picks a PERSISTENT per-project
/// profile (cookies/history saved next to the `.ngintip`); None = a throwaway profile.
#[tauri::command]
pub fn launch_browser(
    state: State<AppState>,
    proxy: String,
    url: Option<String>,
    cdp_port: Option<u16>,
    profile: Option<String>,
) -> CmdResult<()> {
    let (path, mut flags) = {
        let c = state.config.lock().unwrap();
        (c.browser_path.clone(), c.browser_flags.clone())
    };
    // CDP (Chrome DevTools Protocol): expose the debugging endpoint on 127.0.0.1 only.
    // Opt-in per project (see the Browser settings), so it's off unless asked.
    if let Some(port) = cdp_port {
        flags.push_str(&format!(" --remote-debugging-port={port} --remote-debugging-address=127.0.0.1"));
    }
    let profile_dir = profile.as_deref().map(|name| browser_profile_dir(&state, name));
    let child = crate::browser::launch(&path, &proxy, flags.trim(), url.as_deref(), profile_dir.as_deref())?;
    // Track it so it's closed when the app quits; drop handles for ones that already
    // exited so the list can't grow without bound.
    {
        let mut list = state.browsers.lock().unwrap();
        list.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        list.push(child);
    }
    state.ctx.log_info("browser", format!("launched {path} via {proxy}"));
    Ok(())
}

/// Throw away the current root CA and generate a fresh one. Returns the new CA.
/// Browsers must re-trust it.
#[tauri::command]
pub fn regenerate_ca(state: State<AppState>) -> CmdResult<CaInfo> {
    state.ctx.ca.regenerate().map_err(|e| format!("Failed to regenerate CA: {e:#}"))?;
    state.ctx.log_info("ca", "root CA regenerated; re-trust it on clients");
    Ok(ca_info(state))
}

#[tauri::command]
pub fn ca_info(state: State<AppState>) -> CaInfo {
    CaInfo {
        path: state.ctx.ca.path.display().to_string(),
        pem: state.ctx.ca.pem(),
    }
}

/// Repeater: send a hand-edited raw request to `target` (e.g. https://example.com).
/// With `tab`, the send is recorded in that tab's history (also when it fails
/// on the network; a request that doesn't parse isn't recorded).
#[tauri::command]
pub async fn send_raw(
    state: State<'_, AppState>,
    target: String,
    raw: String,
    tab: Option<u64>,
) -> CmdResult<RepeaterResponse> {
    let auto_length = state.config.lock().unwrap().auto_content_length;
    let Some(tab) = tab else {
        return repeater_send(&state.ctx, &target, &raw, auto_length).await;
    };
    // Register a cancel channel for this tab; `cancel_send` fires it to abort a slow
    // send (dropping the send future cancels the in-flight upstream request).
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    state.sends.lock().unwrap().insert(tab, tx);
    let result = tokio::select! {
        r = send_and_record(&state.ctx, tab, &target, &raw, auto_length) => r,
        _ = rx => Err("Cancelled".to_string()),
    };
    state.sends.lock().unwrap().remove(&tab);
    result
}

/// Cancel the in-flight Repeater send for `tab` (if any). Returns whether one was
/// cancelled.
#[tauri::command]
pub fn cancel_send(state: State<AppState>, tab: u64) -> bool {
    if let Some(tx) = state.sends.lock().unwrap().remove(&tab) {
        let _ = tx.send(());
        true
    } else {
        false
    }
}

/// Toggle auto-adding / recomputing Content-Length on Repeater & Intruder sends.
#[tauri::command]
pub fn set_auto_content_length(state: State<AppState>, enabled: bool) -> CmdResult<()> {
    let mut config = state.config.lock().unwrap();
    config.auto_content_length = enabled;
    config::save(&state.config_path, &config).map_err(|e| e.to_string())
}

/// A tab's sends, oldest first (responses left out; see `repeater_entry`).
#[tauri::command]
pub fn repeater_history(state: State<AppState>, tab: u64) -> CmdResult<Vec<store::RepeaterEntry>> {
    state.ctx.store.repeater_history(tab).map_err(|e| e.to_string())
}

/// One history entry including its response.
#[tauri::command]
pub fn repeater_entry(state: State<AppState>, id: u64) -> CmdResult<Option<store::RepeaterEntry>> {
    state.ctx.store.repeater_entry(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_repeater_history(state: State<AppState>, tab: u64) -> CmdResult<()> {
    state.ctx.store.clear_repeater_history(tab).map_err(|e| e.to_string())
}

/// Build and send; if an HTTP/2 attempt fails (some servers mishandle h2), retry
/// once over HTTP/1.1 so a usable response still comes back.
///
/// `apply_ext` runs repeater-scope extensions: the request is transformed before it
/// is sent (returned as `request_final`) and the response after it arrives
/// (`response_original` keeps what arrived), so the Repeater shows Final vs Original
/// like History does. It is on for interactive Repeater tab sends and off for bulk
/// sends (Intruder), which must not pay the per-request Python round-trip.
async fn send_with_fallback(ctx: &std::sync::Arc<proxy::Ctx>, target: &str, raw: &str, auto_length: bool, apply_ext: bool) -> CmdResult<RepeaterResponse> {
    let sent = if apply_ext { ext_request_repeater(ctx, raw).await } else { None };
    let sent_raw = sent.as_deref().unwrap_or(raw);
    let (request, proto) = repeater_request(target, sent_raw, auto_length, None)?;
    let mut result = repeater_exchange(ctx, request, proto, apply_ext).await;
    if result.is_err() && proto == proxy::Proto::PreferHttp2 {
        if let Ok((request, _)) = repeater_request(target, sent_raw, auto_length, Some(proxy::Proto::Http1)) {
            let retry = repeater_exchange(ctx, request, proxy::Proto::Http1, apply_ext).await;
            if retry.is_ok() {
                result = retry;
            }
        }
    }
    // Surface the transformed request that was actually sent (typed one stays in the
    // editor). The runner serializes with CRLF; the editor/viewer use LF.
    if let (Ok(res), Some(_)) = (&mut result, &sent) {
        res.request_final = Some(sent_raw.replace("\r\n", "\n"));
    }
    result
}

pub async fn send_and_record(ctx: &std::sync::Arc<proxy::Ctx>, tab: u64, target: &str, raw: &str, auto_length: bool) -> CmdResult<RepeaterResponse> {
    // An unparseable request is rejected before anything is recorded; only real
    // sends (including failed ones) end up in the tab's history.
    repeater_request(target, raw, auto_length, None)?;
    // Interactive Repeater tab send: repeater-scope extensions apply.
    let result = send_with_fallback(ctx, target, raw, auto_length, true).await;
    record_repeater_history(ctx, tab, target, raw, &result);
    result
}

/// Append a send (success or failure) to a Repeater tab's history.
fn record_repeater_history(ctx: &proxy::Ctx, tab: u64, target: &str, raw: &str, result: &CmdResult<RepeaterResponse>) {
    let mut entry = store::RepeaterEntry {
        id: 0,
        tab_id: tab,
        ts: store::now_ms(),
        target: target.to_string(),
        raw: raw.to_string(),
        status: 0,
        duration_ms: 0,
        length: 0,
        response_raw: String::new(),
        error: None,
    };
    match result {
        Ok(res) => {
            entry.status = res.status;
            entry.duration_ms = res.duration_ms;
            entry.length = res.length;
            entry.response_raw = res.response_raw.clone();
        }
        Err(e) => entry.error = Some(e.clone()),
    }
    if let Err(e) = ctx.store.push_repeater(&entry) {
        eprintln!("[repeater] history: {e:#}");
    }
}

/// Build and send without recording. Used for bulk sends (Intruder) and programmatic
/// one-offs, so repeater-scope extensions are NOT applied here (see `send_and_record`).
pub async fn repeater_send(ctx: &std::sync::Arc<proxy::Ctx>, target: &str, raw: &str, auto_length: bool) -> CmdResult<RepeaterResponse> {
    send_with_fallback(ctx, target, raw, auto_length, false).await
}

/// Resolve the absolute URL for a raw request against `target` (as repeater does).
fn resolve_uri(target: &str, req_target: &str) -> Result<Uri, String> {
    let url = if req_target.starts_with("http://") || req_target.starts_with("https://") {
        req_target.to_string()
    } else {
        format!("{}{}", target.trim().trim_end_matches('/'), req_target)
    };
    url.parse().map_err(|e| format!("Invalid URL {url:?}: {e}"))
}

/// Build the request-side Flow (method/uri/version/headers/body) from a raw
/// request, so a tool-driven send can be recorded in History.
fn request_flow(ctx: &proxy::Ctx, target: &str, raw: &str) -> CmdResult<store::Flow> {
    let parsed = raw::parse_request(raw).map_err(|e| e.to_string())?;
    let uri = resolve_uri(target, &parsed.target)?;
    let method = Method::from_bytes(parsed.method.as_bytes()).map_err(|e| e.to_string())?;
    let version = if parsed.version.starts_with("HTTP/2") { Version::HTTP_2 } else { Version::HTTP_11 };
    let mut headers = HeaderMap::new();
    for (k, v) in &parsed.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            headers.append(name, val);
        }
    }
    let body = raw::restore_crlf(&parsed.body, None, &parsed.headers);
    Ok(store::Flow::request(ctx.store.next_id(), &method, &uri, version, &headers, body))
}

/// Send a request AND record it in History (the flows table), so MCP/tool-driven
/// traffic shows up in the History tab alongside proxied traffic and persists in
/// the .cegat. The real response bytes/headers are recorded (or the error). When
/// `tab` is given, the send is also appended to that Repeater tab's history.
pub async fn send_recorded(ctx: &std::sync::Arc<proxy::Ctx>, tab: Option<u64>, target: &str, raw: &str, auto_length: bool) -> CmdResult<RepeaterResponse> {
    // Repeater-scope extensions transform the request before it is sent; History
    // then shows the sent (transformed) request with the typed one under "Original".
    let sent = ext_request_repeater(ctx, raw).await;
    let sent_raw = sent.as_deref().unwrap_or(raw);
    let mut flow = request_flow(ctx, target, sent_raw)?;
    if sent.is_some() {
        flow.variants.req_original = Some(raw.to_string());
    }
    let (request, proto) = repeater_request(target, sent_raw, auto_length, None)?;

    let started = Instant::now();
    let mut outcome = exchange_parts(ctx, request, proto).await;
    if outcome.is_err() && proto == proxy::Proto::PreferHttp2 {
        if let Ok((req2, _)) = repeater_request(target, sent_raw, auto_length, Some(proxy::Proto::Http1)) {
            let retry = exchange_parts(ctx, req2, proxy::Proto::Http1).await;
            if retry.is_ok() {
                outcome = retry;
            }
        }
    }
    let dur = started.elapsed().as_millis() as u64;
    let result = match outcome {
        Ok((parts, bytes, truncated)) => {
            // Repeater-scope response extensions (e.g. decrypt) before display/record.
            let (parts, bytes) = match ext_response_repeater(ctx, &parts, &bytes).await {
                Some((p, b, original)) => {
                    flow.variants.res_original = Some(original);
                    (p, b)
                }
                None => (parts, bytes),
            };
            flow.set_response(parts.status, parts.version, &parts.headers, bytes.clone());
            flow.summary.duration_ms = dur;
            ctx.record_flow(flow);
            Ok(build_response(&parts, &bytes, truncated, dur))
        }
        Err(e) => {
            flow.set_error(e.clone());
            flow.summary.duration_ms = dur;
            ctx.record_flow(flow);
            Err(e)
        }
    };
    if let Some(tab) = tab {
        record_repeater_history(ctx, tab, target, raw, &result);
    }
    result
}

/// Connection-specific headers HTTP/2 forbids (RFC 9113 §8.2.2). Sending any of
/// them over h2 makes the server reset the stream with a protocol error, so they
/// must be dropped for h2 requests (`te: trailers` is the only exception, but we
/// don't do trailers, so we drop it too).
fn h2_forbidden_header(name: &str) -> bool {
    const FORBIDDEN: [&str; 6] = ["connection", "keep-alive", "proxy-connection", "transfer-encoding", "upgrade", "te"];
    FORBIDDEN.iter().any(|f| name.eq_ignore_ascii_case(f))
}

/// The request line's version picks the protocol: `HTTP/2` offers h2 (falling
/// back to HTTP/1.1 if the server doesn't support it), anything else is HTTP/1.1.
/// `force` overrides that (used to retry over HTTP/1.1 when h2 fails).
pub(crate) fn repeater_request(
    target: &str,
    raw: &str,
    auto_length: bool,
    force: Option<proxy::Proto>,
) -> CmdResult<(Request<body::Body>, proxy::Proto)> {
    let req = raw::parse_request(raw).map_err(|e| e.to_string())?;

    let url = if req.target.starts_with("http://") || req.target.starts_with("https://") {
        req.target.clone()
    } else {
        format!("{}{}", target.trim().trim_end_matches('/'), req.target)
    };
    let uri: Uri = url.parse().map_err(|e| format!("Invalid URL {url:?}: {e}"))?;
    let method = Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?;
    let proto = force.unwrap_or(if req.version.starts_with("HTTP/2") {
        proxy::Proto::PreferHttp2
    } else {
        proxy::Proto::Http1
    });
    let h2 = proto == proxy::Proto::PreferHttp2;
    let authority = uri.authority().map(|a| a.as_str().to_ascii_lowercase());

    // Bodies copied from History are shown decoded, and the editor only makes `\n`.
    let body = raw::restore_crlf(&req.body, None, &req.headers);
    let keep_encoding = raw::matches_encoding(&req.headers, &body);

    let mut builder = Request::builder().method(method).uri(uri);
    let mut had_length = false;
    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("content-length") {
            had_length = true;
            // With auto on we recompute below; with it off, keep it as written.
            if auto_length {
                continue;
            }
        }
        if !keep_encoding && k.eq_ignore_ascii_case("content-encoding") {
            continue;
        }
        // HTTP/2 rejects connection-specific headers with a protocol error.
        if h2 && h2_forbidden_header(k) {
            continue;
        }
        // On HTTP/2 the host travels as :authority; a matching Host is redundant.
        if h2 && k.eq_ignore_ascii_case("host") && authority.as_deref() == Some(v.trim().to_ascii_lowercase().as_str()) {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    // Recompute so edits to the body don't desync the length (unless the user
    // turned auto off, in which case their Content-Length is kept verbatim above).
    if auto_length && (had_length || !body.is_empty()) {
        builder = builder.header("content-length", body.len());
    }
    let request = builder
        .body(body::full(body))
        .map_err(|e| e.to_string())?;
    Ok((request, proto))
}

/// Send a request and collect the response into its parts + (possibly truncated)
/// body bytes. The raw response bytes are preserved so callers can both display
/// them and record the real Flow.
async fn exchange_parts(
    ctx: &proxy::Ctx,
    request: Request<body::Body>,
    proto: proxy::Proto,
) -> CmdResult<(http::response::Parts, bytes::Bytes, bool)> {
    let res = ctx.send(request, proto).await.map_err(|e| format!("{e:#}"))?;
    let (parts, body) = res.into_parts();
    let (bytes, truncated) = body::collect_limited(body, CAPTURE_LIMIT)
        .await
        .map_err(|e| e.to_string())?;
    Ok((parts, bytes, truncated))
}

/// Build the display-oriented RepeaterResponse from collected response parts.
fn build_response(
    parts: &http::response::Parts,
    bytes: &bytes::Bytes,
    truncated: bool,
    duration_ms: u64,
) -> RepeaterResponse {
    let start_line = format!(
        "{} {} {}",
        store::fmt_version(parts.version),
        parts.status.as_u16(),
        parts.status.canonical_reason().unwrap_or("")
    );
    let headers = store::header_vec(&parts.headers);
    let mut response_raw = raw::message_text(start_line.trim_end(), &headers, bytes);
    if truncated {
        response_raw.push_str(&format!("\n\n[{}: response over {} MB; the rest was not read]", brand::NAME, CAPTURE_LIMIT >> 20));
    }
    RepeaterResponse {
        status: parts.status.as_u16(),
        duration_ms,
        length: bytes.len(),
        response_raw,
        response_image: raw::image_data_url(&headers, bytes),
        request_final: None,
        response_original: None,
    }
}

fn header_map(pairs: &[(String, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            map.append(name, val);
        }
    }
    map
}

/// Repeater-scope extensions applied to the request text before it is sent. Returns
/// the transformed raw when an extension changed it.
async fn ext_request_repeater(ctx: &std::sync::Arc<proxy::Ctx>, raw: &str) -> Option<String> {
    if !ctx.extensions.active() {
        return None;
    }
    let ext = ctx.clone();
    let cur = raw.as_bytes().to_vec();
    let out = tauri::async_runtime::spawn_blocking(move || {
        ext.extensions.transform(crate::extensions::Phase::Request, &cur, crate::extensions::Surface::Repeater)
    })
    .await
    .ok()
    .flatten()?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Repeater-scope extensions applied to a response. Returns (new parts, new body, the
/// pre-transform raw text) when an extension changed it.
async fn ext_response_repeater(
    ctx: &std::sync::Arc<proxy::Ctx>,
    parts: &http::response::Parts,
    bytes: &bytes::Bytes,
) -> Option<(http::response::Parts, bytes::Bytes, String)> {
    if !ctx.extensions.active() {
        return None;
    }
    let start = format!(
        "{} {} {}",
        store::fmt_version(parts.version),
        parts.status.as_u16(),
        parts.status.canonical_reason().unwrap_or("")
    );
    let headers = store::header_vec(&parts.headers);
    let original = raw::message_text(start.trim_end(), &headers, bytes);
    let ext = ctx.clone();
    let cur = original.clone().into_bytes();
    let out = tauri::async_runtime::spawn_blocking(move || {
        ext.extensions.transform(crate::extensions::Phase::Response, &cur, crate::extensions::Surface::Repeater)
    })
    .await
    .ok()
    .flatten()?;
    let parsed = raw::parse_response(&String::from_utf8_lossy(&out)).ok()?;
    let mut new_parts = http::Response::new(()).into_parts().0;
    new_parts.status = http::StatusCode::from_u16(parsed.status).unwrap_or(parts.status);
    new_parts.version = parts.version;
    new_parts.headers = header_map(&parsed.headers);
    Some((new_parts, parsed.body, original))
}

async fn repeater_exchange(ctx: &std::sync::Arc<proxy::Ctx>, request: Request<body::Body>, proto: proxy::Proto, apply_ext: bool) -> CmdResult<RepeaterResponse> {
    let started = Instant::now();
    let (parts, bytes, truncated) = exchange_parts(ctx, request, proto).await?;
    let dur = started.elapsed().as_millis() as u64;
    // Repeater-scope response extensions (e.g. decrypt) before display; the arrived
    // response is kept as `response_original` so the UI can show Original vs Final.
    if apply_ext {
        if let Some((p, b, original)) = ext_response_repeater(ctx, &parts, &bytes).await {
            let mut res = build_response(&p, &b, truncated, dur);
            res.response_original = Some(original);
            return Ok(res);
        }
    }
    Ok(build_response(&parts, &bytes, truncated, dur))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_round_trips_and_is_zstd() {
        // Repetitive, SQLite-like bytes so compression actually kicks in.
        let raw: Vec<u8> = b"SQLite format 3\0".iter().copied().cycle().take(200_000).collect();
        let packed = compress_bytes(&raw).unwrap();
        assert!(is_compressed(&packed), "has the project magic");
        assert_eq!(packed[brand::MAGIC.len()], PROJECT_VERSION, "stamped as the current (zstd) version");
        assert!(packed.len() < raw.len() / 4, "clearly smaller: {} -> {}", raw.len(), packed.len());
        assert_eq!(decompress_bytes(&packed).unwrap(), raw, "round-trips");
    }

    #[test]
    fn stream_compress_file_round_trips() {
        // The streaming Save path must produce the same on-disk format as compress_bytes.
        let raw: Vec<u8> = b"SQLite format 3\0".iter().copied().cycle().take(300_000).collect();
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
        );
        let src = std::env::temp_dir().join(format!("ngintip-test-src-{stamp}"));
        let dst = std::env::temp_dir().join(format!("ngintip-test-dst-{stamp}"));
        std::fs::write(&src, &raw).unwrap();
        let (raw_len, comp_len) = compress_file_to(&src, &dst).unwrap();
        assert_eq!(raw_len as usize, raw.len(), "reports the uncompressed size");
        assert!(comp_len > 0 && (comp_len as usize) < raw.len() / 4, "clearly smaller");
        let packed = std::fs::read(&dst).unwrap();
        assert!(is_compressed(&packed), "has the project magic");
        assert_eq!(packed[brand::MAGIC.len()], PROJECT_VERSION, "stamped as the current version");
        assert_eq!(decompress_bytes(&packed).unwrap(), raw, "streamed output round-trips");
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    #[test]
    fn still_reads_legacy_gzip_projects() {
        use std::io::Write;
        // A project saved by an older version (version 1 = gzip) must still open.
        let raw = b"SQLite format 3\0 legacy project bytes".repeat(50);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        enc.write_all(&raw).unwrap();
        let gz = enc.finish().unwrap();
        let mut legacy = Vec::new();
        legacy.extend_from_slice(brand::MAGIC);
        legacy.push(PROJECT_VERSION_GZIP);
        legacy.extend_from_slice(&gz);
        assert_eq!(decompress_bytes(&legacy).unwrap(), raw);
    }

    #[tokio::test]
    async fn bind_proxy_rolls_to_the_next_free_port() {
        // Take an OS-assigned free port, then ask for it again: it is in use, so the
        // bind must roll forward to a different (nearby) port.
        let (l1, p1) = bind_proxy("127.0.0.1", 0).await.unwrap();
        let (l2, p2) = bind_proxy("127.0.0.1", p1).await.unwrap();
        assert_ne!(p1, p2, "second instance got a different port");
        assert!(p2 > p1 && p2 <= p1 + 19, "rolled to a nearby port: {p1} -> {p2}");
        drop((l1, l2));
    }

    #[test]
    fn rejects_unknown_version_and_non_project() {
        assert!(decompress_bytes(b"SQLite format 3\0 plain").is_err(), "not a project");
        let mut bad = Vec::new();
        bad.extend_from_slice(brand::MAGIC);
        bad.push(99);
        bad.extend_from_slice(b"garbage");
        assert!(decompress_bytes(&bad).is_err(), "unknown version");
    }
}
