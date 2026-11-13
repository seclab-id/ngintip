//! Tauri commands invoked from the UI (`src/lib/api.js`).

use std::{
    net::SocketAddr,
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
    id: String,
    port: u16,
    addr: SocketAddr,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// How far a bind address reaches, which decides whether the UI warns about it.
///
/// Only the surprising cases are worth a warning. `All` binds to every interface there
/// is, including ones that appear after the listener started (a VPN coming up, a hotspot
/// turning on), so the reach is not something the user chose and cannot see. `Tunnel`
/// means the address belongs to a VPN interface, where the other side is somebody
/// else's network - a client's, or an employer's.
///
/// `Lan` is deliberately NOT warned about: binding to one LAN address is exactly what
/// you do to drive the proxy from a phone, the address itself says where it is, and a
/// warning that fires on normal use is a warning people learn to click past.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    /// Reachable only from this machine.
    Local,
    /// One address on a local network.
    Lan,
    /// An address on a VPN / tunnel interface.
    Tunnel,
    /// Every interface, present and future.
    All,
}

/// Interface names that mean "tunnel" across the platforms this runs on: macOS `utun*`
/// (which is also what Tailscale and most VPN clients use), Linux `tun*`/`tap*`/`wg*`,
/// `ipsec*`, `ppp*`, ZeroTier `zt*`, and anything with "vpn" in the name (Windows
/// adapters are named freely, e.g. "OpenVPN TAP-Windows6").
fn is_tunnel_interface(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    const PREFIXES: [&str; 9] = ["utun", "tun", "tap", "wg", "ipsec", "ppp", "zt", "nebula", "wireguard"];
    n.contains("vpn") || n.contains("tailscale") || PREFIXES.iter().any(|p| n.starts_with(p))
}

/// Does this host string mean "every interface"? Covers the IPv4 and IPv6 wildcards and
/// an empty host, which `start_listeners` also treats as a wildcard bind.
fn is_wildcard_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.is_empty() || h == "*" {
        return true;
    }
    match h.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_unspecified(),
        Err(_) => false,
    }
}

/// Classify a bind address. `nics` is (interface name, address) for the machine, so the
/// caller can pass a fresh snapshot (a VPN may have come up since the listener started).
fn exposure_of(host: &str, nics: &[(String, String)]) -> Exposure {
    if is_wildcard_host(host) {
        return Exposure::All;
    }
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if h == "localhost" {
        return Exposure::Local;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        if ip.is_loopback() {
            return Exposure::Local;
        }
    }
    // An address on a tunnel interface is reachable from the far side of that tunnel.
    // An address we cannot place is treated as plain LAN: the bind would have failed if
    // it were not one of this machine's own addresses.
    if nics.iter().any(|(name, ip)| ip.eq_ignore_ascii_case(&h) && is_tunnel_interface(name)) {
        return Exposure::Tunnel;
    }
    Exposure::Lan
}

/// [`Exposure`] of an address a listener is actually bound to.
///
/// `exposure_of` works from the configured host string; this works from the live
/// `SocketAddr`, which is what the proxy itself remembers. An unspecified bind
/// (`0.0.0.0`, `::`) classifies as `All` and a loopback bind as `Local`, matching what
/// `status_list` reports from the config.
pub(crate) fn exposure_of_addr(addr: &SocketAddr) -> Exposure {
    if addr.ip().is_unspecified() {
        return Exposure::All;
    }
    exposure_of(&addr.ip().to_string(), &interface_addrs())
}

/// (interface name, address) for every interface on the machine.
fn interface_addrs() -> Vec<(String, String)> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| (i.name.clone(), i.ip().to_string()))
        .collect()
}

/// One listener's state for the UI: its saved config plus whether it is live.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerStatus {
    id: String,
    host: String,
    /// The port actually bound while running (may differ from the configured one if
    /// that was busy); the configured port otherwise.
    port: u16,
    enabled: bool,
    running: bool,
    /// How far this address reaches, so the UI can warn only where it is warranted.
    exposure: Exposure,
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
    /// The History flow this send was recorded as, when it was recorded.
    ///
    /// Set by `send_recorded`. The id is allocated before the request goes out, but the
    /// row is written by a background thread, so a reader can briefly be ahead of the
    /// write; `Store::peek_id` is how a caller tells "still being written" from "never
    /// existed". `flow_ts` pins the citation in time, because ids are reseeded when
    /// History is emptied and can be handed out again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_ts: Option<u64>,
}

/// Every configured listener merged with its live state, for the UI. Locks config
/// then server (keep that order everywhere to avoid deadlock); holds neither across
/// an await.
fn status_list(state: &AppState) -> Vec<ListenerStatus> {
    // Read the interfaces once per call, outside the locks: a VPN may have come up or
    // gone away since these listeners were started, and the warning has to follow that.
    let nics = interface_addrs();
    let config = state.config.lock().unwrap();
    let running = state.server.lock().unwrap();
    config
        .listeners
        .iter()
        .map(|l| {
            let live = running.iter().find(|r| r.id == l.id);
            ListenerStatus {
                id: l.id.clone(),
                host: l.host.clone(),
                port: live.map(|r| r.port).unwrap_or(l.port),
                enabled: l.enabled,
                running: live.is_some(),
                exposure: exposure_of(&l.host, &nics),
            }
        })
        .collect()
}

/// Stop and forget one listener by id (no-op if it isn't running).
async fn stop_one(state: &AppState, id: &str) {
    let found = {
        let mut running = state.server.lock().unwrap();
        running.iter().position(|r| r.id == id).map(|i| running.remove(i))
    };
    if let Some(r) = found {
        state.ctx.remove_listener(&r.addr);
        r.task.abort();
        // Wait so the socket is released before any re-bind on the same port.
        let _ = r.task.await;
    }
}

/// Stop and forget every running listener.
async fn stop_all(state: &AppState) {
    let running: Vec<Running> = std::mem::take(&mut state.server.lock().unwrap());
    for r in running {
        state.ctx.remove_listener(&r.addr);
        r.task.abort();
        let _ = r.task.await;
    }
}

/// Bind + spawn one listener and record it under `id` (restarting it if already up).
/// Does not touch config; the caller persists. Holds no lock across the bind await.
async fn start_one(state: &AppState, id: &str, host: &str, port: u16) -> Result<(), String> {
    stop_one(state, id).await;
    let (listener, bound) = bind_proxy(host, port).await?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    state.ctx.add_listener(addr);
    let task = tauri::async_runtime::spawn(proxy::serve(listener, state.ctx.clone()));
    if bound != port {
        state.ctx.log_info("proxy", format!("port {port} busy; using {host}:{bound} instead (another proxy?)"));
    }
    state.ctx.log_info("proxy", format!("listener started on {host}:{bound}"));
    state.server.lock().unwrap().push(Running {
        id: id.to_string(),
        port: bound,
        addr,
        task,
    });
    Ok(())
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

/// Project setting holding this project's listeners. Listeners belong to a project (one
/// target may need a LAN bind, another only loopback), so they are saved with it and do
/// not follow the user into the next project. `config.listeners` stays the live working
/// set that [`status_list`] reports; this is the copy that survives a project switch.
pub const LISTENERS_KEY: &str = "listeners";

/// Write the live listener set into the open project. Clones out of the config lock
/// first: the store takes its own lock, and holding both invites a deadlock.
fn persist_listeners(state: &AppState) {
    let listeners = state.config.lock().unwrap().listeners.clone();
    if let Ok(v) = serde_json::to_value(&listeners) {
        let _ = state.ctx.store.put_setting(LISTENERS_KEY, &v);
    }
}

/// The listener set a fresh project starts with: the app default, one loopback listener.
/// Written by [`blank_project`] so a new project does not inherit the previous one's.
pub fn default_listeners() -> Vec<config::Listener> {
    config::Config::default().listeners
}

/// Of the listeners currently running, the ids the open project does not want up: it
/// either has no such listener at all (it belonged to the project before this one) or
/// has it saved as disabled. Everything else keeps running untouched, so switching
/// between projects that share a listener does not drop the socket.
fn stale_listener_ids(running: &[String], wanted: &[config::Listener]) -> Vec<String> {
    running
        .iter()
        .filter(|id| !wanted.iter().any(|l| &l.id == *id && l.enabled))
        .cloned()
        .collect()
}

#[tauri::command]
pub fn list_listeners(state: State<AppState>) -> Vec<ListenerStatus> {
    status_list(&state)
}

/// Bind every enabled listener that is not already up, logging the ones that fail
/// instead of aborting: one unavailable port must not keep the others down.
async fn start_enabled(state: &AppState) {
    let pending: Vec<(String, String, u16)> = {
        let config = state.config.lock().unwrap();
        let running = state.server.lock().unwrap();
        config
            .listeners
            .iter()
            .filter(|l| l.enabled && !running.iter().any(|r| r.id == l.id))
            .map(|l| (l.id.clone(), l.host.clone(), l.port))
            .collect()
    };
    for (id, host, port) in pending {
        if let Err(e) = start_one(state, &id, &host, port).await {
            state.ctx.log_error("proxy", format!("listener {host}:{port} failed to start: {e}"));
        }
    }
}

/// Start every enabled listener not already running. Called once on load so the
/// configured listeners come up; idempotent, so it is safe to call again.
#[tauri::command]
pub async fn start_listeners(state: State<'_, AppState>) -> CmdResult<Vec<ListenerStatus>> {
    start_enabled(&state).await;
    Ok(status_list(&state))
}

/// Make the running listeners match the open project's saved set. Called after a project
/// is created, opened, or reopened at startup.
///
/// A project saved before listeners became per-project has no saved set; it inherits the
/// current one, once, and keeps it from then on. A project that does have one wins over
/// whatever the previous project left running.
#[tauri::command]
pub async fn sync_listeners(state: State<'_, AppState>) -> CmdResult<Vec<ListenerStatus>> {
    let saved: Option<Vec<config::Listener>> = state
        .ctx
        .store
        .get_setting(LISTENERS_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok());

    let wanted = match saved {
        Some(list) => list,
        None => {
            // First time for this project: adopt what is live and remember it.
            persist_listeners(&state);
            return start_listeners(state).await;
        }
    };

    // Drop anything running that this project does not ask to have up, so the previous
    // project's listeners never linger on ports this one did not choose.
    let stale = {
        let running = state.server.lock().unwrap();
        let ids: Vec<String> = running.iter().map(|r| r.id.clone()).collect();
        stale_listener_ids(&ids, &wanted)
    };
    for id in stale {
        stop_one(&state, &id).await;
    }

    {
        let mut config = state.config.lock().unwrap();
        config.listeners = wanted;
        config::save(&state.config_path, &config).map_err(|e| e.to_string())?;
    }
    start_enabled(&state).await;
    Ok(status_list(&state))
}

/// Stop every running listener (the global Stop). Their saved config is untouched.
#[tauri::command]
pub async fn stop_listeners(state: State<'_, AppState>) -> CmdResult<Vec<ListenerStatus>> {
    stop_all(&state).await;
    state.ctx.log_info("proxy", "all listeners stopped");
    Ok(status_list(&state))
}

/// Add a listener, persist it, and start it. Rolls the entry back on a bind failure.
#[tauri::command]
pub async fn add_listener(state: State<'_, AppState>, host: String, port: u16) -> CmdResult<Vec<ListenerStatus>> {
    let id = {
        let mut config = state.config.lock().unwrap();
        let id = format!("l{}-{}", crate::store::now_ms(), config.listeners.len());
        config.listeners.push(config::Listener { id: id.clone(), host: host.clone(), port, enabled: true });
        config::save(&state.config_path, &config).map_err(|e| e.to_string())?;
        id
    };
    if let Err(e) = start_one(&state, &id, &host, port).await {
        let mut config = state.config.lock().unwrap();
        config.listeners.retain(|l| l.id != id);
        let _ = config::save(&state.config_path, &config);
        return Err(e);
    }
    persist_listeners(&state);
    Ok(status_list(&state))
}

/// Stop and delete a listener.
#[tauri::command]
pub async fn remove_listener(state: State<'_, AppState>, id: String) -> CmdResult<Vec<ListenerStatus>> {
    stop_one(&state, &id).await;
    {
        let mut config = state.config.lock().unwrap();
        config.listeners.retain(|l| l.id != id);
        config::save(&state.config_path, &config).map_err(|e| e.to_string())?;
    }
    persist_listeners(&state);
    Ok(status_list(&state))
}

/// Enable (start) or disable (stop) one listener without deleting it.
#[tauri::command]
pub async fn set_listener_enabled(state: State<'_, AppState>, id: String, enabled: bool) -> CmdResult<Vec<ListenerStatus>> {
    let target = {
        let mut config = state.config.lock().unwrap();
        let Some(l) = config.listeners.iter_mut().find(|l| l.id == id) else {
            return Err("listener not found".into());
        };
        l.enabled = enabled;
        let hp = (l.host.clone(), l.port);
        config::save(&state.config_path, &config).map_err(|e| e.to_string())?;
        hp
    };
    if enabled {
        if let Err(e) = start_one(&state, &id, &target.0, target.1).await {
            let mut config = state.config.lock().unwrap();
            if let Some(l) = config.listeners.iter_mut().find(|l| l.id == id) {
                l.enabled = false;
            }
            let _ = config::save(&state.config_path, &config);
            return Err(e);
        }
    } else {
        stop_one(&state, &id).await;
    }
    persist_listeners(&state);
    Ok(status_list(&state))
}

/// Edit a listener's host/port. Rebinds on the new address when it was running.
#[tauri::command]
pub async fn update_listener(state: State<'_, AppState>, id: String, host: String, port: u16) -> CmdResult<Vec<ListenerStatus>> {
    let was_running = {
        let mut config = state.config.lock().unwrap();
        let Some(l) = config.listeners.iter_mut().find(|l| l.id == id) else {
            return Err("listener not found".into());
        };
        l.host = host.clone();
        l.port = port;
        config::save(&state.config_path, &config).map_err(|e| e.to_string())?;
        state.server.lock().unwrap().iter().any(|r| r.id == id)
    };
    if was_running {
        start_one(&state, &id, &host, port).await?;
    }
    persist_listeners(&state);
    Ok(status_list(&state))
}

/// The open project's Organizer (collections of saved requests).
#[tauri::command]
pub fn organizer_state(state: State<AppState>) -> CmdResult<crate::organizer::Organizer> {
    crate::organizer::load(&state.ctx.store)
}

/// Apply one Organizer edit. The backend owns the Organizer so the UI and the MCP server
/// can't overwrite each other; the result carries the new state for the UI to show.
#[tauri::command]
pub fn organizer_op(state: State<AppState>, action: crate::organizer::Op) -> CmdResult<crate::organizer::Applied> {
    crate::organizer::apply(&state.ctx.store, action)
}

/// Replace the hostname -> IP overrides (direct connections). Persisted + applied live.
#[tauri::command]
pub fn set_host_overrides(state: State<AppState>, overrides: Vec<config::HostOverride>) -> CmdResult<()> {
    let mut config = state.config.lock().unwrap();
    config.host_overrides = overrides;
    config::save(&state.config_path, &config).map_err(|e| e.to_string())?;
    state.ctx.set_host_overrides(&config.host_overrides);
    Ok(())
}

#[tauri::command]
pub fn list_flows(state: State<AppState>) -> CmdResult<Vec<FlowSummary>> {
    state.ctx.store.summaries().map_err(|e| e.to_string())
}

/// A recorded flow's message versions, including the lazy "decoded" one: when a
/// `decode_*` extension is active and this host is in scope, the readable form is
/// computed on demand and offered as the Extension version alongside the real (wire)
/// message. The wire bytes stay canonical and nothing is persisted; this only makes an
/// encrypted or encoded body readable.
///
/// Shared by the UI (`get_flow`) and the MCP server (`history_get`), so an agent reading
/// a flow sees exactly the versions the window shows. Without that, an encrypted API is
/// legible on screen and opaque over MCP.
pub(crate) fn flow_variants(ctx: &std::sync::Arc<proxy::Ctx>, f: &store::Flow) -> store::FlowVariants {
    let mut variants = f.variants.clone();
    if !ctx.extensions.active() || !ctx.scope.contains(&f.summary.host) {
        return variants;
    }
    let hc = || crate::extensions::HookCtx {
        host: f.summary.host.clone(),
        path: f.summary.path.clone(),
        flow_id: Some(f.summary.id),
        edited: false,
    };
    if variants.req_extension.is_none() {
        let txt = raw::request_text_for_processing(f);
        if let Some(out) = ctx.extensions.decode(crate::extensions::Phase::Request, txt.as_bytes(), crate::extensions::Surface::Proxy, &hc()) {
            variants.req_extension = Some(String::from_utf8_lossy(&out).replace("\r\n", "\n"));
        }
    }
    if variants.res_extension.is_none() && !f.res_headers.is_empty() {
        let txt = raw::response_text_for_processing(f);
        if let Some(out) = ctx.extensions.decode(crate::extensions::Phase::Response, txt.as_bytes(), crate::extensions::Surface::Proxy, &hc()) {
            variants.res_extension = Some(String::from_utf8_lossy(&out).replace("\r\n", "\n"));
        }
    }
    variants
}

#[tauri::command]
pub fn get_flow(state: State<AppState>, id: u64) -> CmdResult<Option<FlowDetail>> {
    let flow = state.ctx.store.get(id).map_err(|e| e.to_string())?;
    Ok(flow.map(|f| {
        let variants = flow_variants(&state.ctx, &f);
        FlowDetail {
            request_raw: raw::request_text(&f),
            response_raw: raw::response_text(&f),
            request_truncated_view: raw::view_truncated(&f.req_headers, &f.req_body),
            response_truncated_view: raw::view_truncated(&f.res_headers, &f.res_body),
            response_is_image: raw::is_image(&f.res_headers, &f.res_body),
            request_is_binary: raw::body_is_binary(&f.req_headers, &f.req_body),
            response_is_binary: raw::body_is_binary(&f.res_headers, &f.res_body),
            variants,
            summary: f.summary,
        }
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

/// Delete flows by id.
///
/// `async` on purpose: a non-async command compiles to `ExecutionContext::Blocking` and
/// runs on the IPC dispatch thread, so deleting a large selection froze the whole window.
/// On Windows that reads as a crash, because an app that stops pumping messages for a few
/// seconds gets the "(Not Responding)" title from the OS. Same reason `save_project` does
/// its snapshot off-thread.
#[tauri::command]
pub async fn delete_flows(state: State<'_, AppState>, ids: Vec<u64>) -> CmdResult<usize> {
    let ctx = state.ctx.clone();
    // Captured before the work starts: if the operator opens another project while this
    // runs, the remaining ids must not be deleted out of THAT project. The deleting is
    // chunked, so without this there are dozens of windows in which that can happen, and
    // a slow-but-responsive delete is exactly when someone clicks elsewhere.
    let gen = ctx.store.generation();
    let total = ids.len();
    let done = tauri::async_runtime::spawn_blocking(move || ctx.store.delete(&ids, gen))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if done < total {
        state.ctx.log_warn(
            "history",
            format!("deleted {done} of {total} selected request(s); the project changed before the rest were removed"),
        );
    }
    Ok(done)
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
pub async fn clear_flows(state: State<'_, AppState>) -> CmdResult<()> {
    let ctx = state.ctx.clone();
    // Off the IPC thread for the same reason as `delete_flows`: on a large History this
    // is seconds of SQLite work, and doing it inline freezes the window. And guarded for
    // the same reason too: making it async is what lets `open_project` interleave, so
    // without the generation this could empty the History of the project just opened.
    let gen = ctx.store.generation();
    tauri::async_runtime::spawn_blocking(move || ctx.store.clear(gen))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
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
/// projects keep working. A previous compressed format is not supported.
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

/// The document a working copy came from, recorded INSIDE the working copy.
///
/// `DOCUMENT_PTR` is one shared file in the app data dir that every instance rewrites on
/// every save, while `WORKING_PTR` is only rewritten when a project is opened. After a
/// two-instance session that pair routinely describes two different projects, and the
/// next startup then opens one project while believing it is another, which a later save
/// writes out over the wrong file. A working copy that carries its own document cannot
/// be mismatched with one: it travels with the data it describes.
const DOC_KEY: &str = "document_path";

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
    wd.join(format!("{}-{}.{}", std::process::id(), nanos, crate::brand::PROJECT_EXT))
}

fn remove_working_file(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suf in ["-wal", "-shm"] {
        let mut s = path.as_os_str().to_owned();
        s.push(suf);
        let _ = std::fs::remove_file(PathBuf::from(s));
    }
}

/// Whether a working file belongs to a process that is still running.
///
/// Working files are named `<pid>-<nanos>.<ext>` (see [`fresh_working_path`]), so the
/// owner is already in the name: the nanosecond stamp alone cannot collide inside one
/// process, which is the whole reason the pid is there. It names an owner, not a serial
/// number. [`crate::browser::cleanup_orphan_browsers`] does exactly this check for
/// throwaway Chrome profiles, and says why in its doc comment: several Ngintip
/// instances coexist, and one must never sweep another's live state.
///
/// Our own pid is NOT live for this purpose: our previous working file is ours to
/// delete. Anything unparseable is treated as live, because "existence is not
/// ownership" cuts both ways and a file we cannot prove is garbage is kept.
fn owner_is_live(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return true };
    let Some((pid, _)) = name.split_once('-') else { return true };
    let Ok(pid) = pid.parse::<i32>() else { return true };
    if pid as u32 == std::process::id() {
        return false;
    }
    pid_alive(pid)
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false; // 0 and negatives address process GROUPS, never a single process
    }
    // `kill(pid, 0) == 0` means the process exists and we may signal it. EPERM means it
    // exists and we may NOT (it belongs to another user, e.g. a root-owned daemon),
    // which is still very much alive. Only ESRCH means there is no such process. Reading
    // success alone would report every process we do not own as dead and delete its file.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows refuses to unlink a SQLite file another process holds open, so the
/// destructive case cannot arise there and the sweep stays as it was. Reporting every
/// owner as dead keeps crashed runs from leaving their working files behind forever.
#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

/// Delete working files left over from previous runs, keeping `keep` (and its
/// WAL sidecars, which share its path prefix).
///
/// "Previous runs" is the precondition this used to assert and never check: it deleted
/// every entry it was not keeping, including the live database of another running
/// instance, WAL and all. That instance then went on capturing into an unlinked inode
/// and lost everything the moment it exited.
fn clean_stray_working(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(working_dir(dir)) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if let Some(k) = keep {
            if p.to_string_lossy().starts_with(k.to_string_lossy().as_ref()) {
                continue;
            }
        }
        if owner_is_live(&p) {
            continue; // another live instance is using it
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
        // The pointer is shared, so `old` can name a file this process never created:
        // the same liveness rule as the startup sweep decides whether it is ours to
        // delete. Patching only the sweep would leave this site destroying live data.
        if old != wp && !owner_is_live(&old) {
            remove_working_file(&old);
        }
    }
    Ok(())
}

/// Let go of everything belonging to the project being closed, BEFORE the store is
/// swapped to the next one.
///
/// The order is the entire point, and getting it backwards is worse than not doing it.
/// Draining the intercept queue forwards the held messages, and the flows that produces
/// are recorded into whichever store is current: drain after the swap and the previous
/// client's traffic is filed in the new client's project file. The launched browser is
/// the same hazard with a longer fuse, since it stays authenticated to the previous
/// target and keeps fetching through the proxy until it is closed.
///
/// Held messages are DROPPED, not forwarded, and intercept is left off. A held request
/// has not reached the target yet, so dropping it costs the client's system nothing,
/// while forwarding would send a request to their production system after the operator
/// left that engagement and could not be filed anywhere afterwards. The browser that
/// would have seen the failure is closed on the next line anyway.
fn release_project(state: &AppState) {
    state.ctx.intercept.drain_dropped();
    state.ctx.ws_intercept.drain_dropped();
    state.kill_browsers();
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

/// Identity of the file at `path` as this process last saw it: path, length, mtime.
/// Cheap (one `stat`) and good enough to catch another writer; a content hash would
/// mean reading a 400 MB project on every save to protect against a change that kept
/// both the length and the mtime.
/// The document this working copy was loaded from or last saved to, if it still exists.
fn document_of(store: &store::Store) -> Option<PathBuf> {
    let v = store.get_setting(DOC_KEY).ok()??;
    let p = PathBuf::from(v.as_str()?);
    p.is_file().then_some(p)
}

pub fn doc_stamp(path: &Path) -> Option<(PathBuf, u64, std::time::SystemTime)> {
    let m = std::fs::metadata(path).ok()?;
    Some((path.to_path_buf(), m.len(), m.modified().ok()?))
}

/// Error prefix the UI matches on to offer Overwrite instead of just reporting a
/// failure. A sentinel rather than a typed error because a command returns a String.
pub const STALE_DOC: &str = "DOCUMENT_CHANGED";

/// Whether `path` is this session's document AND the file there is no longer the one
/// we wrote or opened.
///
/// The save path ends in an unconditional `std::fs::rename`, which is what makes a
/// stale pointer or a second instance able to replace a project wholesale with no
/// warning. A missing file is NOT stale: writing it back is what the user asked for.
fn document_is_stale(state: &AppState, path: &Path) -> bool {
    stale_against(state.doc_stamp.lock().unwrap().as_ref(), path)
}

fn stale_against(known: Option<&(PathBuf, u64, std::time::SystemTime)>, path: &Path) -> bool {
    let Some((known_path, len, mtime)) = known else { return false };
    if known_path != path {
        return false; // Save As somewhere else: the file dialog already asked
    }
    match std::fs::metadata(path) {
        Ok(m) => m.len() != *len || m.modified().ok().is_some_and(|t| t != *mtime),
        Err(_) => false,
    }
}

/// Record the current document path (or clear it) in memory and on disk.
fn set_document(dir: &Path, state: &AppState, doc: Option<&Path>) {
    *state.document.lock().unwrap() = doc.map(|p| p.to_path_buf());
    // Interpreter + venv are per-project now (in the project's settings): point the
    // manager at this project's folder and apply its saved interpreter/venv.
    apply_project_python_venv(&state.ctx, dir, doc);
    // The stamp is taken AFTER the file exists, so it describes what we just wrote or
    // opened; that is the thing a later save compares against.
    *state.doc_stamp.lock().unwrap() = doc.and_then(doc_stamp);
    let _ = match doc {
        Some(p) => state.ctx.store.put_setting(DOC_KEY, &serde_json::json!(p.to_string_lossy())),
        None => state.ctx.store.put_setting(DOC_KEY, &serde_json::Value::Null),
    };
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

    // Reuse the previous run's working file, but only when no live process owns it.
    // `is_file()` alone asked whether the file exists, not whose it is, so a second
    // instance attached to the first's live SQLite database: both then mint flow ids
    // from one `MAX(id)` seed and replace each other's rows through INSERT OR REPLACE,
    // with no delete and no rename needed. A live owner means we start our own copy
    // from the document instead, below.
    if let Some(wp) = read_ptr(dir, WORKING_PTR).filter(|p| p.is_file() && !owner_is_live(p)) {
        clean_stray_working(dir, Some(&wp));
        let store = store::Store::open(&wp)?;
        // The working copy names its own document, so a DOCUMENT_PTR another instance
        // rewrote cannot make us open this project under a different file's name and
        // then save over it. The pointer is only the fallback for working copies made
        // before this was recorded.
        let document = document_of(&store).or(document);
        return Ok((store, document));
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
    release_project(&state);
    let wp = fresh_working_path(&dir);
    switch_working(&dir, &state, &wp)?;
    // Listeners belong to the project. Write the default set into this brand-new store
    // so `sync_listeners` has something to find: without it the project would be treated
    // as a pre-per-project one and inherit whatever the last project left running.
    if let Ok(v) = serde_json::to_value(default_listeners()) {
        let _ = state.ctx.store.put_setting(LISTENERS_KEY, &v);
    }
    set_document(&dir, &state, None);
    // Goes through `adopt` like `open_project` does, rather than reimplementing it. The
    // inline copy is how the listeners default above had to be written by hand in the
    // first place: a per-project field added to one path and missed in the other.
    adopt(&state)?;
    Ok(project_info_of(&state))
}

/// Open a project file (compressed `.ngintip` or a legacy plain SQLite one) by
/// loading it into a fresh working copy.
#[tauri::command]
pub fn open_project(app: tauri::AppHandle, state: State<AppState>, path: String) -> CmdResult<ProjectInfo> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("Failed to read {path}: {e}"))?;
    let raw = if is_compressed(&bytes) { decompress_bytes(&bytes)? } else { bytes };

    // After the file has been read and decompressed, so an unreadable project fails
    // without having torn down the one that is still open.
    release_project(&state);
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
async fn write_document(dir: PathBuf, state: State<'_, AppState>, path: PathBuf, force: bool) -> CmdResult<SaveResult> {
    if !force && document_is_stale(&state, &path) {
        return Err(format!(
            "{STALE_DOC}: {} changed on disk since this window opened or last saved it. \
             Saving now would replace whatever is there. Another Ngintip instance, a sync \
             client or an editor may have written it.",
            path.display()
        ));
    }
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

/// Save (compress) the current project to a chosen `.ngintip` path. A missing parent
/// folder is created, so a new project can go straight into the default folder.
#[tauri::command]
pub async fn save_project_as(app: tauri::AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<SaveResult> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    // Save As is an explicit choice of destination, and the file dialog already asks
    // before replacing an existing file, so it does not re-ask here.
    write_document(dir, state, path, true).await
}

/// Folder offered for a new project when none was used before: `~/Documents/Ngintip`
/// (home if there is no Documents). Created on the first save, not here.
#[tauri::command]
pub fn default_project_dir(app: tauri::AppHandle) -> CmdResult<String> {
    let base = app.path().document_dir().or_else(|_| app.path().home_dir()).map_err(|e| e.to_string())?;
    Ok(base.join(brand::NAME).display().to_string())
}

/// Whether a file or folder exists (the New project dialog asks before replacing one).
#[tauri::command]
pub fn path_exists(path: String) -> bool {
    !path.is_empty() && Path::new(&path).exists()
}

/// Save (compress) the current project to its existing document path.
#[tauri::command]
pub async fn save_project(app: tauri::AppHandle, state: State<'_, AppState>, force: Option<bool>) -> CmdResult<SaveResult> {
    let force = force.unwrap_or(false);
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let doc = state
        .document
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "No project file yet. Use Save Project As first.".to_string())?;
    write_document(dir, state, doc, force).await
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

/// Record a log line raised by the UI, so something the frontend discovers (the startup
/// update check, for one) lands in the Logs view and is saved with the project like
/// every other log line. The level has to be one of the four the UI filters on, or the
/// entry would be invisible there; source and message are capped so a loop in the
/// frontend cannot grow the log table without bound.
#[tauri::command]
pub fn app_log(state: State<AppState>, level: String, source: String, message: String) -> CmdResult<()> {
    let level = log_level(&level).ok_or_else(|| format!("unknown log level {level:?}"))?;
    let source = truncate_chars(source.trim(), 32);
    let message = truncate_chars(message.trim(), 4096);
    if source.is_empty() || message.is_empty() {
        return Err("source and message are required".into());
    }
    state.ctx.log(level, &source, message);
    Ok(())
}

/// The log levels the Logs view can filter on, as the `&'static str` [`proxy::Ctx::log`]
/// wants. Anything else is rejected rather than quietly stored.
fn log_level(level: &str) -> Option<&'static str> {
    ["error", "warn", "info", "debug"].into_iter().find(|l| *l == level)
}

/// Cut a string to at most `max` chars (not bytes, so a multi-byte char is never split).
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => s[..cut].to_string(),
        None => s.to_string(),
    }
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
/// `bypass_ext` is the tab's raw mode: skip extensions entirely for this send, so the
/// request leaves exactly as typed and the response comes back exactly as it arrived.
pub async fn send_raw(
    state: State<'_, AppState>,
    target: String,
    raw: String,
    tab: Option<u64>,
    bypass_ext: Option<bool>,
) -> CmdResult<RepeaterResponse> {
    let auto_length = state.config.lock().unwrap().auto_content_length;
    let Some(tab) = tab else {
        return repeater_send(&state.ctx, &target, &raw, auto_length).await;
    };
    let ext = if bypass_ext.unwrap_or(false) { None } else { Some(crate::extensions::Surface::Repeater) };
    // Register a cancel channel for this tab; `cancel_send` fires it to abort a slow
    // send (dropping the send future cancels the in-flight upstream request).
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    state.sends.lock().unwrap().insert(tab, tx);
    let result = tokio::select! {
        r = send_and_record_ext(&state.ctx, tab, &target, &raw, auto_length, ext) => r,
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

/// Store an attack's results with the project. Replaces the tab's previous run.
#[tauri::command]
pub async fn intruder_save_results(state: State<'_, AppState>, tab: u64, rows: Vec<store::IntruderRow>) -> CmdResult<()> {
    let ctx = state.ctx.clone();
    // Off the IPC thread: a large run is thousands of rows with their bodies.
    tauri::async_runtime::spawn_blocking(move || ctx.store.intruder_save(tab, &rows))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// An attack's rows without their request/response text, for the results table.
#[tauri::command]
pub fn intruder_results(state: State<AppState>, tab: u64) -> CmdResult<Vec<store::IntruderRow>> {
    state.ctx.store.intruder_results(tab).map_err(|e| e.to_string())
}

/// One row in full, fetched when the operator opens it.
#[tauri::command]
pub fn intruder_result(state: State<AppState>, tab: u64, n: u64) -> CmdResult<Option<store::IntruderRow>> {
    state.ctx.store.intruder_result(tab, n).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_intruder_results(state: State<AppState>, tab: u64) -> CmdResult<()> {
    state.ctx.store.clear_intruder_results(tab).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_repeater_history(state: State<AppState>, tab: u64) -> CmdResult<()> {
    state.ctx.store.clear_repeater_history(tab).map_err(|e| e.to_string())
}

/// Build and send; if an HTTP/2 attempt fails (some servers mishandle h2), retry
/// once over HTTP/1.1 so a usable response still comes back.
///
/// `ext` runs the extensions scoped to that surface: the request is transformed before
/// it is sent (returned as `request_final`) and the response after it arrives
/// (`response_original` keeps what arrived), so Repeater / Intruder can show Final vs
/// Original like History does. Pass `Some(Surface::Repeater)` for interactive Repeater
/// sends and `Some(Surface::Intruder)` for attack requests; `None` skips the per-request
/// Python round-trip (programmatic one-offs that opted out).
async fn send_with_fallback(ctx: &std::sync::Arc<proxy::Ctx>, target: &str, raw: &str, auto_length: bool, ext: Option<crate::extensions::Surface>) -> CmdResult<RepeaterResponse> {
    let host = host_of_target(target);
    let sent = if let Some(s) = ext { ext_request(ctx, raw, &host, s).await } else { None };
    let sent_raw = sent.as_deref().unwrap_or(raw);
    let (request, proto) = repeater_request(target, sent_raw, auto_length, None)?;
    let mut result = repeater_exchange(ctx, request, proto, ext, &host).await;
    if result.is_err() && proto == proxy::Proto::PreferHttp2 {
        if let Ok((request, _)) = repeater_request(target, sent_raw, auto_length, Some(proxy::Proto::Http1)) {
            let retry = repeater_exchange(ctx, request, proxy::Proto::Http1, ext, &host).await;
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

/// A recorded Repeater-scoped send. Only the tests call this directly; `send_raw`
/// goes through `send_and_record_ext` so a tab in raw mode can skip extensions.
#[cfg(test)]
pub async fn send_and_record(ctx: &std::sync::Arc<proxy::Ctx>, tab: u64, target: &str, raw: &str, auto_length: bool) -> CmdResult<RepeaterResponse> {
    send_and_record_ext(ctx, tab, target, raw, auto_length, Some(crate::extensions::Surface::Repeater)).await
}

/// As `send_and_record`, but the caller picks the extension surface (or `None` to skip
/// extensions entirely, which is what a Repeater tab in raw mode asks for).
async fn send_and_record_ext(
    ctx: &std::sync::Arc<proxy::Ctx>,
    tab: u64,
    target: &str,
    raw: &str,
    auto_length: bool,
    ext: Option<crate::extensions::Surface>,
) -> CmdResult<RepeaterResponse> {
    // An unparseable request is rejected before anything is recorded; only real
    // sends (including failed ones) end up in the tab's history.
    repeater_request(target, raw, auto_length, None)?;
    let result = send_with_fallback(ctx, target, raw, auto_length, ext).await;
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

/// Build and send without recording and without extensions. Used for programmatic
/// one-offs that opt out of the per-request Python round-trip (see `send_and_record`
/// for Repeater and `intruder_send` for Intruder, which do apply extensions).
pub async fn repeater_send(ctx: &std::sync::Arc<proxy::Ctx>, target: &str, raw: &str, auto_length: bool) -> CmdResult<RepeaterResponse> {
    send_with_fallback(ctx, target, raw, auto_length, None).await
}

/// One Intruder attack request: no Repeater history, but intruder-scope extensions
/// apply (so an encrypt/transform extension affects every attack request). The sent
/// (transformed) request comes back as `request_final` and the arrived response as
/// `response_original`, so a saved result can keep Wire / Original / Extension views.
#[tauri::command]
/// `bypass_ext` is the tab's raw mode: skip extensions entirely for this attack, which
/// also removes the per-request Python round trip from a bulk run.
pub async fn intruder_send(
    state: State<'_, AppState>,
    target: String,
    raw: String,
    bypass_ext: Option<bool>,
) -> CmdResult<RepeaterResponse> {
    let auto_length = state.config.lock().unwrap().auto_content_length;
    let ext = if bypass_ext.unwrap_or(false) { None } else { Some(crate::extensions::Surface::Intruder) };
    send_with_fallback(&state.ctx, &target, &raw, auto_length, ext).await
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
    let (id, gen) = ctx.store.next_id_at();
    Ok(store::Flow::request(id, &method, &uri, version, &headers, body).at(gen))
}

/// Send a request AND record it in History (the flows table), so MCP/tool-driven
/// traffic shows up in the History tab alongside proxied traffic and persists in
/// the .ngintip. The real response bytes/headers are recorded (or the error). When
/// `tab` is given, the send is also appended to that Repeater tab's history.
pub async fn send_recorded(ctx: &std::sync::Arc<proxy::Ctx>, tab: Option<u64>, target: &str, raw: &str, auto_length: bool) -> CmdResult<RepeaterResponse> {
    // Repeater-scope extensions transform the request before it is sent; History
    // then shows the sent (transformed) request with the typed one under "Original".
    let host = host_of_target(target);
    let sent = ext_request(ctx, raw, &host, crate::extensions::Surface::Repeater).await;
    let sent_raw = sent.as_deref().unwrap_or(raw);
    let mut flow = request_flow(ctx, target, sent_raw)?;
    if sent.is_some() {
        flow.variants.req_original = Some(raw.to_string());
    }
    // Captured before `flow` is moved into the recorder below. The id exists from here
    // on, so the caller can be handed it and read the exchange back instead of guessing
    // which History row was its own.
    let (recorded_id, recorded_ts) = (flow.summary.id, flow.summary.ts);
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
            let (parts, bytes, res_original) = match ext_response(ctx, &parts, &bytes, &host, crate::extensions::Surface::Repeater).await {
                Some((p, b, original)) => {
                    flow.variants.res_original = Some(original.clone());
                    (p, b, Some(original))
                }
                None => (parts, bytes, None),
            };
            flow.set_response(parts.status, parts.version, &parts.headers, bytes.clone());
            flow.summary.duration_ms = dur;
            ctx.record_flow(flow);
            let mut res = build_response(&parts, &bytes, truncated, dur);
            // Hand back both versions, as `send_with_fallback` does for the UI: the
            // caller (the MCP server) otherwise only ever sees the transformed message
            // and cannot tell what actually crossed the wire.
            res.response_original = res_original;
            if sent.is_some() {
                res.request_final = Some(sent_raw.replace("\r\n", "\n"));
            }
            res.flow_id = Some(recorded_id);
            res.flow_ts = Some(recorded_ts);
            Ok(res)
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
        flow_id: None,
        flow_ts: None,
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

/// The authority (host[:port]) of a Repeater target URL, for the hook ctx.
fn host_of_target(target: &str) -> String {
    target
        .parse::<Uri>()
        .ok()
        .and_then(|u| u.authority().map(|a| a.as_str().to_string()))
        .unwrap_or_default()
}

/// Repeater-scope extensions applied to the request text before it is sent. Returns
/// the transformed raw when an extension changed it.
async fn ext_request(ctx: &std::sync::Arc<proxy::Ctx>, raw: &str, host: &str, surface: crate::extensions::Surface) -> Option<String> {
    if !ctx.extensions.active() {
        return None;
    }
    let ext = ctx.clone();
    let cur = raw.as_bytes().to_vec();
    let host = host.to_string();
    let out = tauri::async_runtime::spawn_blocking(move || {
        // Outgoing request: process (wire edits) then encode (e.g. encrypt the body you
        // typed as plaintext). A Repeater / Intruder send is always treated as edited.
        let mut buf = cur;
        let mut changed = false;
        if let Some(p) = ext.extensions.transform(crate::extensions::Phase::Request, &buf, surface) {
            buf = p;
            changed = true;
        }
        let hc = crate::extensions::HookCtx { host, edited: true, ..Default::default() };
        if let Some(e) = ext.extensions.encode(crate::extensions::Phase::Request, &buf, surface, &hc) {
            buf = e;
            changed = true;
        }
        changed.then_some(buf)
    })
    .await
    .ok()
    .flatten()?;
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Repeater-scope extensions applied to a response. Returns (new parts, new body, the
/// pre-transform raw text) when an extension changed it.
async fn ext_response(
    ctx: &std::sync::Arc<proxy::Ctx>,
    parts: &http::response::Parts,
    bytes: &bytes::Bytes,
    host: &str,
    surface: crate::extensions::Surface,
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
    let host = host.to_string();
    let out = tauri::async_runtime::spawn_blocking(move || {
        // Arrived response: process (wire edits) then decode (e.g. decrypt) for display.
        // The pre-transform text is kept as Original by the caller. A response has no
        // Host header, so the request's host is passed in ctx for the extension's scope.
        let mut buf = cur;
        let mut changed = false;
        if let Some(p) = ext.extensions.transform(crate::extensions::Phase::Response, &buf, surface) {
            buf = p;
            changed = true;
        }
        let hc = crate::extensions::HookCtx { host, ..Default::default() };
        if let Some(d) = ext.extensions.decode(crate::extensions::Phase::Response, &buf, surface, &hc) {
            buf = d;
            changed = true;
        }
        changed.then_some(buf)
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

async fn repeater_exchange(ctx: &std::sync::Arc<proxy::Ctx>, request: Request<body::Body>, proto: proxy::Proto, ext: Option<crate::extensions::Surface>, host: &str) -> CmdResult<RepeaterResponse> {
    let started = Instant::now();
    let (parts, bytes, truncated) = exchange_parts(ctx, request, proto).await?;
    let dur = started.elapsed().as_millis() as u64;
    // Surface-scoped response extensions (e.g. decrypt) before display; the arrived
    // response is kept as `response_original` so the UI can show Original vs Final.
    if let Some(s) = ext {
        if let Some((p, b, original)) = ext_response(ctx, &parts, &bytes, host, s).await {
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

    /// A working file belonging to another RUNNING instance must survive both sweeps.
    /// Before this, a second instance deleted the first's live SQLite database on
    /// startup; the victim kept capturing into an unlinked inode and lost everything
    /// when it exited. pid 1 always exists on unix, so it stands in for a live peer.
    #[test]
    #[cfg(unix)]
    fn the_working_sweep_spares_a_live_instance() {
        let dir = std::env::temp_dir().join(format!("ngintip-sweep-{}", std::process::id()));
        let wd = working_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&wd).unwrap();

        let mine = wd.join(format!("{}-1.ngintip", std::process::id()));
        let live = wd.join("1-2.ngintip"); // pid 1 is running on every unix
        let live_wal = wd.join("1-2.ngintip-wal");
        let dead = wd.join("999999999-3.ngintip"); // past any real pid
        let odd = wd.join("not-a-pid.ngintip");
        for p in [&mine, &live, &live_wal, &dead, &odd] {
            std::fs::write(p, b"x").unwrap();
        }

        clean_stray_working(&dir, None);

        assert!(live.exists(), "deleted the live database of another instance");
        assert!(live_wal.exists(), "deleted another instance's WAL, which loses committed rows");
        assert!(odd.exists(), "deleted a file whose owner could not be read; unproven is not garbage");
        assert!(!dead.exists(), "kept a dead run's leftover, so the sweep stopped sweeping");
        assert!(!mine.exists(), "kept our own stale working file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `owner_is_live` reads the pid out of `<pid>-<nanos>.<ext>`, treats our own as
    /// not-live (ours to delete) and anything unparseable as live.
    #[test]
    #[cfg(unix)]
    fn ownership_is_read_from_the_working_filename() {
        assert!(owner_is_live(Path::new("/w/1-123.ngintip")));
        assert!(owner_is_live(Path::new("/w/1-123.ngintip-shm")));
        assert!(owner_is_live(Path::new("/w/nonsense.ngintip")));
        assert!(owner_is_live(Path::new("/w/-5-123.ngintip")), "a name with no readable pid must be kept");
        assert!(!owner_is_live(Path::new("/w/999999999-123.ngintip")));
        assert!(!owner_is_live(&PathBuf::from(format!("/w/{}-123.ngintip", std::process::id()))));
    }

    /// A save refuses only when the file at THIS session's document path is no longer
    /// the one we wrote or opened. A different path, or no stamp at all, is not stale.
    #[test]
    fn a_document_changed_underneath_is_detected() {
        let path = std::env::temp_dir().join(format!("ngintip-stale-{}.ngintip", std::process::id()));
        std::fs::write(&path, b"original").unwrap();
        let stamp = doc_stamp(&path).expect("stamped");

        assert!(!stale_against(Some(&stamp), &path), "unchanged file reported as stale");
        assert!(!stale_against(None, &path), "no stamp means nothing to compare against");
        assert!(!stale_against(Some(&stamp), Path::new("/elsewhere.ngintip")), "another path is not this document");

        std::fs::write(&path, b"replaced by another instance").unwrap();
        assert!(stale_against(Some(&stamp), &path), "a rewritten document was not detected");

        // A file that is gone is not stale: writing it back is exactly what Save means.
        std::fs::remove_file(&path).unwrap();
        assert!(!stale_against(Some(&stamp), &path));
    }

    /// Startup must take the document from the working copy it is opening, not from the
    /// shared DOCUMENT_PTR, which any other instance rewrites on any save (including an
    /// auto-save). With the pointer believed, this project would open under another
    /// file's name and the next save would write it over that file.
    #[test]
    fn a_working_copy_names_its_own_document_not_the_shared_pointer() {
        let dir = std::env::temp_dir().join(format!("ngintip-ptr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(working_dir(&dir)).unwrap();

        let mine = format!("{}-1.{}", std::process::id(), brand::PROJECT_EXT);
        let wp = working_dir(&dir).join(mine);
        let ours = dir.join("ours.ngintip");
        let theirs = dir.join("theirs.ngintip");
        std::fs::write(&ours, b"a").unwrap();
        std::fs::write(&theirs, b"b").unwrap();

        {
            let store = store::Store::open(&wp).unwrap();
            store.put_setting(DOC_KEY, &serde_json::json!(ours.to_string_lossy())).unwrap();
        }
        write_ptr(&dir, WORKING_PTR, &wp);
        write_ptr(&dir, DOCUMENT_PTR, &theirs); // what another instance's save left behind

        let (_store, document) = open_startup(&dir).unwrap();
        assert_eq!(document.as_deref(), Some(ours.as_path()), "startup believed the shared pointer over the project it actually opened");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn nics() -> Vec<(String, String)> {
        vec![
            ("lo0".into(), "127.0.0.1".into()),
            ("en0".into(), "192.168.1.20".into()),
            ("bridge100".into(), "192.168.64.1".into()),
            ("utun4".into(), "10.8.0.6".into()),
            ("OpenVPN TAP-Windows6".into(), "10.9.0.2".into()),
        ]
    }

    #[test]
    fn only_a_wildcard_or_tunnel_bind_counts_as_surprising_exposure() {
        let n = nics();
        // Wildcards reach every interface, including ones not up yet.
        for host in ["0.0.0.0", "::", "[::]", "", "  ", "*", "0:0:0:0:0:0:0:0"] {
            assert_eq!(exposure_of(host, &n), Exposure::All, "{host:?}");
        }
        // A VPN address: the far side is someone else's network.
        assert_eq!(exposure_of("10.8.0.6", &n), Exposure::Tunnel);
        assert_eq!(exposure_of("10.9.0.2", &n), Exposure::Tunnel);
        // A deliberate LAN bind is normal work (driving the proxy from a phone).
        assert_eq!(exposure_of("192.168.1.20", &n), Exposure::Lan);
        assert_eq!(exposure_of("192.168.64.1", &n), Exposure::Lan);
        // Loopback reaches nobody.
        for host in ["127.0.0.1", "127.0.0.53", "::1", "[::1]", "localhost", "LocalHost"] {
            assert_eq!(exposure_of(host, &n), Exposure::Local, "{host:?}");
        }
        // An address that is on no interface here: the bind would fail, so do not
        // escalate it to a tunnel warning.
        assert_eq!(exposure_of("203.0.113.9", &n), Exposure::Lan);
    }

    #[test]
    fn tunnel_interface_names_are_recognised_without_catching_ethernet() {
        for name in ["utun0", "utun12", "tun0", "tap0", "wg0", "ipsec1", "ppp0", "zt5u4", "nebula1", "tailscale0", "OpenVPN TAP-Windows6", "Corp VPN"] {
            assert!(is_tunnel_interface(name), "{name} should be a tunnel");
        }
        for name in ["en0", "eth0", "wlan0", "bridge100", "lo0", "awdl0", "Ethernet", "Wi-Fi", "docker0"] {
            assert!(!is_tunnel_interface(name), "{name} should not be a tunnel");
        }
    }

    fn listener(id: &str, port: u16, enabled: bool) -> config::Listener {
        config::Listener { id: id.into(), host: "127.0.0.1".into(), port, enabled }
    }

    #[test]
    fn a_new_project_starts_with_one_loopback_listener() {
        // The whole point of the per-project move: a new project must not inherit the
        // previous one's extra listeners.
        let d = default_listeners();
        assert_eq!(d.len(), 1, "a fresh project gets exactly one listener");
        assert_eq!(d[0].host, "127.0.0.1", "and it is loopback, not exposed to the LAN");
        assert!(d[0].enabled);
    }

    #[test]
    fn switching_projects_stops_only_the_listeners_the_new_one_does_not_want() {
        let running = ["a".to_string(), "b".to_string(), "c".to_string()];
        let wanted = vec![
            listener("a", 8080, true),  // shared and enabled: keep the socket
            listener("b", 8081, false), // saved as disabled: take it down
            // "c" is absent: it belonged to the previous project
            listener("d", 8083, true),  // not running yet: started afterwards, not here
        ];
        assert_eq!(stale_listener_ids(&running, &wanted), vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn a_project_with_no_listeners_of_its_own_stops_everything() {
        let running = ["a".to_string(), "b".to_string()];
        assert_eq!(stale_listener_ids(&running, &[]), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn listeners_round_trip_through_a_project_setting() {
        // What makes them per-project: the set has to come back out of the project store
        // exactly as it went in, including a disabled one and a LAN bind.
        let path = std::env::temp_dir().join(format!("intip-listeners-{}.intip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = crate::store::Store::open(&path).unwrap();

        let saved = vec![
            listener("default", 8080, true),
            config::Listener { id: "lan".into(), host: "0.0.0.0".into(), port: 8081, enabled: false },
        ];
        store.put_setting(LISTENERS_KEY, &serde_json::to_value(&saved).unwrap()).unwrap();

        let back: Vec<config::Listener> =
            serde_json::from_value(store.get_setting(LISTENERS_KEY).unwrap().unwrap()).unwrap();
        assert_eq!(back, saved);

        // A project that never saved a set is distinguishable from one that saved none,
        // which is what decides between "inherit once" and "stop everything".
        assert!(store.get_setting("listeners_other").unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn app_log_accepts_only_the_levels_the_ui_filters_on() {
        for ok in ["error", "warn", "info", "debug"] {
            assert_eq!(log_level(ok), Some(ok));
        }
        // Anything else would be stored but invisible behind every level tab.
        for bad in ["trace", "INFO", "fatal", "", "info "] {
            assert_eq!(log_level(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn app_log_caps_its_inputs_without_splitting_a_char() {
        assert_eq!(truncate_chars("short", 32), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        // Multi-byte: cutting by bytes here would produce invalid UTF-8 and panic.
        assert_eq!(truncate_chars("ngintip -> cegat", 7), "ngintip");
        assert_eq!(truncate_chars("\u{e9}\u{e9}\u{e9}", 2), "\u{e9}\u{e9}");
        assert_eq!(truncate_chars("", 8), "");
    }

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
