mod body;
mod brand;
mod browser;
mod burp;
mod ca;
mod commands;
mod config;
mod crash;
mod extensions;
mod usage;
#[cfg(test)]
mod e2e;
mod har;
mod intercept;
mod mcp;
mod scope;
mod proxy;
mod raw;
mod rules;
mod store;
// Echo server used only by tests (the user-facing Test server feature was removed).
#[cfg(test)]
mod testserver;
mod upstream;
mod ws;

use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

pub struct AppState {
    pub ctx: Arc<proxy::Ctx>,
    pub server: Mutex<Option<commands::Running>>,
    /// App-wide settings and the file they're saved to.
    pub config: Mutex<config::Config>,
    pub config_path: std::path::PathBuf,
    /// The `.cegat` document the project is saved to/from (None until saved).
    pub document: Mutex<Option<std::path::PathBuf>>,
    /// Running MCP server (lets an LLM drive Cegat), if enabled.
    pub mcp: Mutex<Option<commands::McpRunning>>,
    /// Process/system resource stats for the Usage panel.
    pub usage: Mutex<usage::Usage>,
    /// Browsers this app launched (wired to the proxy), so they can be closed when
    /// the app quits instead of being left running.
    pub browsers: Mutex<Vec<std::process::Child>>,
    /// In-flight Repeater sends, by tab id, so a slow send can be cancelled. Sending
    /// on the channel (or dropping it) cancels the awaiting send in `send_raw`.
    pub sends: Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<()>>>,
}

impl AppState {
    /// Kill every browser this app launched (called on window close / app exit).
    /// Terminates the tracked children AND anything using our throwaway profile, so
    /// orphans (from a crash or an older build) are cleaned up too.
    pub fn kill_browsers(&self) {
        let tracked = if let Ok(mut list) = self.browsers.lock() {
            let n = list.len();
            for mut child in list.drain(..) {
                crate::browser::terminate(&mut child);
            }
            n
        } else {
            0
        };
        // Reliable sweep by profile marker (covers untracked/orphaned instances).
        crate::browser::kill_profile_browsers();
        if tracked > 0 {
            eprintln!("[browser] closed {tracked} launched browser(s) on app close");
        }
    }
}

/// Proxy events go straight to the webview.
impl proxy::Events for tauri::AppHandle {
    fn emit_json(&self, event: &str, payload: serde_json::Value) {
        let _ = self.emit(event, payload);
    }
}

/// Native application menu (top bar on macOS). File items emit a `menu` event
/// that the webview turns into the matching action.
fn build_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};

    let h = app.handle().clone();
    let sep = || PredefinedMenuItem::separator(&h);

    let app_menu = SubmenuBuilder::new(&h, brand::NAME)
        .item(&PredefinedMenuItem::about(&h, Some(&format!("About {}", brand::NAME)), None)?)
        .item(&sep()?)
        .item(&PredefinedMenuItem::services(&h, None)?)
        .item(&sep()?)
        .item(&PredefinedMenuItem::hide(&h, None)?)
        .item(&PredefinedMenuItem::hide_others(&h, None)?)
        .item(&sep()?)
        .item(&PredefinedMenuItem::quit(&h, None)?)
        .build()?;

    // No "Close Window" item on purpose: its Cmd+W accelerator made an accidental
    // keypress quit the whole app (one window) and lose the session. The window still
    // closes from its red traffic-light button, and Cmd+Q still quits. "Close Project"
    // (Shift+Cmd+W, in the Project menu) is the deliberate way to drop the open project.
    let file_menu = SubmenuBuilder::new(&h, "File")
        .item(&MenuItemBuilder::with_id("new", "New Project\u{2026}").accelerator("CmdOrCtrl+N").build(&h)?)
        .item(&MenuItemBuilder::with_id("open", "Open Project\u{2026}").accelerator("CmdOrCtrl+O").build(&h)?)
        .item(&sep()?)
        .item(&MenuItemBuilder::with_id("export_har", "Export HAR\u{2026}").build(&h)?)
        .item(&MenuItemBuilder::with_id("export_burp", "Export Burp XML\u{2026}").build(&h)?)
        .build()?;

    // Project: save the whole project. `.cegat` files are compressed on save.
    let project_menu = SubmenuBuilder::new(&h, "Project")
        .item(&MenuItemBuilder::with_id("save", "Save Project").accelerator("CmdOrCtrl+S").build(&h)?)
        .item(&MenuItemBuilder::with_id("save_as", "Save Project As\u{2026}").accelerator("Shift+CmdOrCtrl+S").build(&h)?)
        .item(&sep()?)
        .item(&MenuItemBuilder::with_id("close_project", "Close Project").accelerator("Shift+CmdOrCtrl+W").build(&h)?)
        .build()?;

    // So Cmd+C/V and friends work in the webview on macOS.
    let edit_menu = SubmenuBuilder::new(&h, "Edit")
        .item(&PredefinedMenuItem::undo(&h, None)?)
        .item(&PredefinedMenuItem::redo(&h, None)?)
        .item(&sep()?)
        .item(&PredefinedMenuItem::cut(&h, None)?)
        .item(&PredefinedMenuItem::copy(&h, None)?)
        .item(&PredefinedMenuItem::paste(&h, None)?)
        .item(&PredefinedMenuItem::select_all(&h, None)?)
        .build()?;

    let menu = MenuBuilder::new(&h).item(&app_menu).item(&file_menu).item(&project_menu).item(&edit_menu).build()?;
    app.set_menu(menu)?;
    Ok(())
}

/// Read the saved worker-thread override from config.json BEFORE the async runtime is
/// built (Tauri only exposes the data dir later, inside setup). Mirrors Tauri's
/// app_data_dir for this bundle per platform. Returns None (= auto, one worker per CPU
/// core) when unset, zero, or unreadable. The identifier must match tauri.conf.json.
fn configured_worker_threads() -> Option<usize> {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))
    };
    let path = base?.join("dev.intip.proxy").join("config.json");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let n = json.get("threads").and_then(|v| v.as_u64()).unwrap_or(0);
    (n > 0).then_some(n as usize)
}

pub fn run() {
    // One process-wide crypto provider for every rustls config.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Worker-thread count for the proxy/MCP runtime. Default (unset / 0) = Tauri's default
    // of one worker per CPU core; a value saved in config.json overrides it (read here
    // because the runtime is fixed at process start, so changes need a relaunch). Blocking
    // work (project Save, etc.) uses tokio's separate on-demand blocking pool, unaffected.
    if let Some(n) = configured_worker_threads() {
        if let Ok(rt) = tokio::runtime::Builder::new_multi_thread().worker_threads(n).enable_all().build() {
            tauri::async_runtime::set(rt.handle().clone());
            std::mem::forget(rt); // Tauri keeps only the handle; the runtime must outlive the process.
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Closing the window (its X) should also close any browser this app launched.
        // On macOS a window close does not emit RunEvent::Exit (the app can stay
        // alive), so the browser must be killed here, not only on full app exit.
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed) {
                eprintln!("[browser] window {event:?} -> closing launched browsers");
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.kill_browsers();
                }
            }
        })
        .setup(|app| {
            // Clean up browsers orphaned by PAST instances that are no longer running,
            // without touching browsers of other live Ngintip instances (multi-instance).
            crate::browser::cleanup_orphan_browsers();

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            // Write a crash dump for any panic on any thread.
            crash::install(&data_dir);

            let ca = Arc::new(ca::CertAuthority::load_or_create(&data_dir)?);
            let (store, document) = commands::open_startup(&data_dir)?;
            let ctx = Arc::new(proxy::Ctx::new(Box::new(app.handle().clone()), ca, store)?);
            ctx.scope.set(ctx.store.get_list(commands::SCOPE_KEY)?);
            commands::load_rules(&ctx);
            commands::index_in_background(&ctx);

            let config_path = data_dir.join("config.json");
            let mut config = config::load(&config_path);
            ctx.set_auth(config.auth.credentials());
            ctx.set_invisible(config.invisible);
            match upstream::UpstreamProxy::parse(&config.upstream) {
                Ok(proxy) => ctx.set_upstream(proxy)?,
                Err(e) => eprintln!("[config] upstream proxy ignored: {e:#}"),
            }

            // Extensions are fully per-project now: the script list, master on/off,
            // interpreter, custom venv and debug console all live in the project. Migrate
            // any old global config (extensions, interpreter, venv) into the open project
            // once, then point the manager at the project folder + its saved
            // interpreter/venv and load that project's extension set + saved console.
            {
                commands::migrate_config_extensions(&ctx, &config_path, &mut config);
                commands::apply_project_python_venv(&ctx, &data_dir, document.as_deref());
                commands::load_extensions_into(&ctx);
            }

            let mcp_boot = config.mcp;
            let mcp_port = if config.mcp_port == 0 { 8082 } else { config.mcp_port };
            let mcp_docs = config.mcp_docs_path.clone();
            let ctx_for_mcp = ctx.clone();

            app.manage(AppState {
                ctx,
                server: Mutex::new(None),
                config: Mutex::new(config),
                config_path,
                document: Mutex::new(document),
                mcp: Mutex::new(None),
                usage: Mutex::new(usage::Usage::new()),
                browsers: Mutex::new(Vec::new()),
                sends: Mutex::new(std::collections::HashMap::new()),
            });

            // Start the MCP server on boot when it was left enabled.
            if mcp_boot {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match commands::start_mcp(ctx_for_mcp, mcp_port, mcp_docs).await {
                        Ok(r) => {
                            if let Some(state) = handle.try_state::<AppState>() {
                                *state.mcp.lock().unwrap() = Some(r);
                            }
                        }
                        Err(e) => eprintln!("[mcp] {e}"),
                    }
                });
            }

            build_menu(app)?;
            app.on_menu_event(|app, event| {
                let id = event.id().0.as_str();
                if matches!(id, "new" | "open" | "save" | "save_as" | "close_project" | "export_har" | "export_burp") {
                    // The action (dialogs, commands) runs in the webview.
                    let _ = app.emit("menu", id);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_proxy,
            commands::stop_proxy,
            commands::proxy_status,
            commands::list_flows,
            commands::get_flow,
            commands::flow_image,
            commands::flow_hex,
            commands::ws_messages,
            commands::flow_body,
            commands::clear_flows,
            commands::set_flow_meta,
            commands::delete_flows,
            commands::reset_flow_ids,
            commands::search_flows,
            commands::get_setting,
            commands::set_setting,
            commands::get_scope,
            commands::set_scope,
            commands::get_rules,
            commands::test_rules,
            commands::set_rules,
            commands::list_interfaces,
            commands::ca_info,
            commands::regenerate_ca,
            commands::export_ca,
            commands::open_url,
            commands::logs,
            commands::clear_logs,
            commands::delete_logs,
            commands::get_config,
            commands::set_auth,
            commands::set_invisible,
            commands::set_threads,
            commands::set_mcp,
            commands::mcp_status,
            commands::browser_status,
            commands::set_browser,
            commands::detect_browser,
            commands::download_browser,
            commands::launch_browser,
            commands::close_launched_browsers,
            commands::browser_running,
            commands::cancel_send,
            commands::set_auto_content_length,
            commands::set_upstream,
            commands::send_raw,
            commands::repeater_history,
            commands::repeater_entry,
            commands::clear_repeater_history,
            commands::project_info,
            commands::open_project,
            commands::blank_project,
            commands::export_har,
            commands::export_burp,
            commands::save_binary,
            commands::copy_image,
            commands::save_project_as,
            commands::save_project,
            commands::intercept_status,
            commands::set_intercept,
            commands::set_intercept_responses,
            commands::intercept_forward,
            commands::intercept_drop,
            commands::set_intercept_filter,
            commands::ws_intercept_status,
            commands::set_ws_intercept,
            commands::ws_intercept_forward,
            commands::ws_intercept_drop,
            commands::usage_stats,
            commands::crash_info,
            commands::extensions_status,
            commands::detect_python,
            commands::set_python_path,
            commands::set_extensions,
            commands::set_extensions_enabled,
            commands::setup_extension_venv,
            commands::set_extension_venv_path,
            commands::install_extension_package,
            commands::extension_packages,
            commands::extension_template,
            commands::extension_logs,
            commands::clear_extension_logs,
            commands::test_extension,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|handle, event| {
            // Close any browser this app launched on window close or app quit, so it
            // never lingers. Window events are handled here too (not only in
            // on_window_event) as belt-and-suspenders across platforms.
            let close = match &event {
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => true,
                tauri::RunEvent::WindowEvent { event, .. } => {
                    matches!(event, tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed)
                }
                _ => false,
            };
            if close {
                if let Some(state) = handle.try_state::<AppState>() {
                    state.kill_browsers();
                }
            }
        });
}
