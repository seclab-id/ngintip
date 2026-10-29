//! App-wide settings (shared by every project), stored as JSON in the app data dir.

use std::{fs, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub auth: ProxyAuth,
    /// Upstream proxy URL (`http://[user:pass@]host:port`); empty = connect directly.
    pub upstream: String,
    /// Invisible proxying for non-proxy-aware clients (routes by SNI/Host).
    pub invisible: bool,
    /// Auto-add / recompute Content-Length on Repeater & Intruder sends. Turn off
    /// to send the Content-Length exactly as written (e.g. request smuggling).
    pub auto_content_length: bool,
    /// MCP server (lets an LLM drive Cegat): the last on/off state, its port
    /// (0 = 8082) and the docs path served for the LLM. On/off is informational only:
    /// whether the server runs is a per-project switch (project setting `mcp`), never
    /// carried over to another or a new project.
    pub mcp: bool,
    pub mcp_port: u16,
    pub mcp_docs_path: String,
    /// Path to a Chromium/Chrome binary used by the "Open browser" button (empty =
    /// not configured). Set manually, auto-detected, or filled by a download.
    pub browser_path: String,
    /// Extra command-line flags to pass when launching the browser (space-split).
    pub browser_flags: String,
    /// LEGACY: the extension interpreter, custom venv, script list and master on/off
    /// used to be global. They now live per-project (in the project's settings). These
    /// fields are only read once at startup to migrate an old global config into the open
    /// project, then cleared. Kept for that migration.
    #[serde(default)]
    pub python_path: String,
    #[serde(default)]
    pub venv_path: String,
    #[serde(default)]
    pub extensions: Vec<crate::extensions::ExtensionSpec>,
    #[serde(default)]
    pub extensions_enabled: bool,
    /// Fixed tokio worker-thread count for the proxy runtime. 0 = auto (one per CPU core).
    /// Read at startup (see lib.rs), so a change takes effect on the next app launch.
    #[serde(default)]
    pub threads: u16,
    /// Proxy listeners (bind addresses). Several can run at once; each is started and
    /// stopped independently. A missing key (old config) falls back to one default
    /// listener via the container-level `default`; an explicit empty list means none.
    pub listeners: Vec<Listener>,
    /// Force a hostname to connect to a given IP for direct outbound connections, like
    /// an app-scoped hosts file. The TLS SNI and forged cert keep the real hostname.
    pub host_overrides: Vec<HostOverride>,
}

/// One proxy listener bind address. `id` is stable so the UI can start, stop, edit or
/// remove a specific one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Listener {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub enabled: bool,
}

impl Default for Listener {
    fn default() -> Self {
        Self { id: String::new(), host: "127.0.0.1".to_string(), port: 8080, enabled: true }
    }
}

/// Force `host` to resolve to `ip` on direct connections (like a hosts-file entry).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostOverride {
    pub host: String,
    pub ip: String,
    pub enabled: bool,
}

impl Default for HostOverride {
    fn default() -> Self {
        Self { host: String::new(), ip: String::new(), enabled: true }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auth: ProxyAuth::default(),
            upstream: String::new(),
            invisible: false,
            auto_content_length: true,
            mcp: false,
            mcp_port: 0,
            mcp_docs_path: "/docs".to_string(),
            browser_path: String::new(),
            browser_flags: String::new(),
            python_path: String::new(),
            venv_path: String::new(),
            extensions: Vec::new(),
            extensions_enabled: false,
            threads: 0,
            listeners: vec![Listener { id: "default".to_string(), host: "127.0.0.1".to_string(), port: 8080, enabled: true }],
            host_overrides: Vec::new(),
        }
    }
}

/// Basic credentials clients must send in `Proxy-Authorization`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyAuth {
    pub enabled: bool,
    pub username: String,
    pub password: String,
}

impl ProxyAuth {
    /// `user:pass` to compare against, or None when auth is off.
    pub fn credentials(&self) -> Option<String> {
        self.enabled.then(|| format!("{}:{}", self.username, self.password))
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.username.is_empty() || self.password.is_empty() {
            return Err("Username and password are required".into());
        }
        if self.username.contains(':') {
            return Err("Username cannot contain ':'".into());
        }
        Ok(())
    }
}

/// Missing or unreadable file means defaults.
pub fn load(path: &Path) -> Config {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Holds a password, so keep it private to the user on Unix.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(config)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
