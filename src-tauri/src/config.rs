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
    /// MCP server (lets an LLM drive Cegat) on/off, its port (0 = 8082) and the
    /// docs path served for the LLM.
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
