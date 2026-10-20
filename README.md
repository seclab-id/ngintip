# Ngintip

Intercepting HTTP(S) proxy in the style of Burp / Caido. Backend in Rust (tokio, hyper, rustls, rcgen); desktop GUI in Tauri 2 with Svelte 5.

## Run

```bash
./run.sh          # macOS/Linux: install deps if needed, start dev mode
./run.sh build    # bundle .app / .dmg / .deb / .AppImage
.\run.ps1         # Windows (PowerShell)
```

Needs Rust (stable) and Node.js 22.12+. On Linux also `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`.

The proxy listens on `127.0.0.1:8080`. Point your client at it and trust the root CA (generated on first run); Settings has copy-paste steps for every platform, or open `http://127.0.0.1:8080/` through the proxy to download it.

## Features

- **Proxy** - HTTP/HTTPS (MITM), HTTP/2 via ALPN, WebSocket (`ws`/`wss`), streaming bodies, upstream HTTP/SOCKS5 proxy, invisible mode for non-proxy-aware clients, optional Basic auth.
- **History** - virtualized table, scope, full-text body search (FTS5), sitemap tree, notes/colours, raw/pretty/preview viewers, export to HAR and Burp XML.
- **Intercept** - hold and edit requests and responses (forward / edit / drop), original-vs-edited compare, safe body rewriting (Content-Length and encoding handled).
- **Repeater** - tabbed, with groups, history, beautify, and request/response capture.
- **Intruder** - Sniper / Battering ram / Pitchfork / Cluster bomb with per-position wordlists.
- **Match & Replace** - regex rules on request/response lines, headers and bodies, with a standalone tester.
- **Passive Scan** - built-in rules flag issues on captured traffic.
- **Decoder** - Base64, URL, HTML entities, hex, JWT, and hashes (MD5, SHA-1/256/512).
- **Comparer** - word/byte diff of two requests or responses.
- **Extensions** - Python hooks that edit requests/responses in flight, with per-extension scope and a venv manager.
- **MCP server** - exposes History to an AI client (local, `127.0.0.1` only); read flows, set notes and highlight colours.
- **Projects** - one SQLite `.ngintip` file per project; history, scope, rules and Repeater tabs are saved and remembered.
- Light/dark themes, an in-app Logs view, a built-in test echo server, and a Guide page documenting every feature and shortcut.

## Test

```bash
cd src-tauri && cargo test   # Rust unit + end-to-end (real proxy in front of a local origin)
npm test                     # Vitest: lib units + full-UI smoke test (jsdom, fake backend)
```

CI runs both on Linux, macOS and Windows.
