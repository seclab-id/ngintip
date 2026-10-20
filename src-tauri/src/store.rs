//! HTTP history persisted in SQLite. One `.cegat` file = one project.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri, Version};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

/// One line in the in-app debug/error log, persisted per project.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: u64,
    pub ts: u64,
    /// "error" | "warn" | "info" | "debug".
    pub level: String,
    /// Where it came from: "tls", "proxy", "upstream", "store", "ws", "project", "ca".
    pub source: String,
    pub message: String,
}

/// What the history table needs; sent to the UI on every completed flow.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowSummary {
    pub id: u64,
    pub ts: u64,
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub path: String,
    /// 0 when the upstream request failed.
    pub status: u16,
    pub length: usize,
    /// Request body size in bytes (for the optional History "Req body" column).
    pub req_length: usize,
    pub mime: String,
    pub duration_ms: u64,
    pub error: Option<String>,
    /// User annotations (History notes / highlight colour).
    pub note: String,
    pub color: String,
    /// Response still streaming: a preview that a final version will replace.
    pub streaming: bool,
    /// The flow was changed from what arrived: edited in the interceptor or
    /// transformed by an extension (i.e. it has Original/Final variants).
    pub modified: bool,
}

#[derive(Clone)]
pub struct Flow {
    pub summary: FlowSummary,
    pub version: String,
    pub req_headers: Vec<(String, String)>,
    pub req_body: Bytes,
    pub res_version: String,
    pub res_reason: String,
    pub res_headers: Vec<(String, String)>,
    pub res_body: Bytes,
    /// Only the start of the body was kept (see `body::CAPTURE_LIMIT`).
    pub req_truncated: bool,
    pub res_truncated: bool,
    /// Earlier versions of the request/response, before intercept edits or an
    /// extension transformed them. Only set when they differ from the final.
    pub variants: FlowVariants,
}

/// Captured request/response text at earlier pipeline stages, so History can show
/// Original / Intercepted / Extension alongside the final (stored) message. Each is
/// the full raw message text; absent (None) when that stage did not change anything.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowVariants {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req_original: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req_extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub res_original: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub res_extension: Option<String>,
}

impl FlowVariants {
    pub fn is_empty(&self) -> bool {
        self.req_original.is_none()
            && self.req_extension.is_none()
            && self.res_original.is_none()
            && self.res_extension.is_none()
    }
}

impl Flow {
    pub fn request(
        id: u64,
        method: &Method,
        uri: &Uri,
        version: Version,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Self {
        Self {
            summary: FlowSummary {
                id,
                ts: now_ms(),
                method: method.to_string(),
                scheme: uri.scheme_str().unwrap_or("http").to_string(),
                host: display_host(uri),
                path: uri
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/")
                    .to_string(),
                status: 0,
                length: 0,
                req_length: body.len(),
                mime: String::new(),
                duration_ms: 0,
                error: None,
                note: String::new(),
                color: String::new(),
                streaming: false,
                modified: false,
            },
            version: fmt_version(version).to_string(),
            req_headers: header_vec(headers),
            req_body: body,
            res_version: String::new(),
            res_reason: String::new(),
            res_headers: Vec::new(),
            res_body: Bytes::new(),
            req_truncated: false,
            res_truncated: false,
            variants: FlowVariants::default(),
        }
    }

    pub fn set_response(
        &mut self,
        status: StatusCode,
        version: Version,
        headers: &HeaderMap,
        body: Bytes,
    ) {
        self.set_response_head(status, version, headers);
        self.summary.length = body.len();
        self.res_body = body;
    }

    /// Status line and headers; the body is filled in once it has streamed through.
    pub fn set_response_head(&mut self, status: StatusCode, version: Version, headers: &HeaderMap) {
        self.summary.status = status.as_u16();
        self.summary.mime = headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(short_mime)
            .unwrap_or_default();
        self.res_version = fmt_version(version).to_string();
        self.res_reason = status.canonical_reason().unwrap_or("").to_string();
        self.res_headers = header_vec(headers);
    }

    pub fn set_error(&mut self, error: String) {
        self.summary.error = Some(error);
    }
}

/// A WebSocket message on a 101 flow, as listed in the UI.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsMessage {
    pub id: u64,
    pub flow_id: u64,
    pub ts: u64,
    /// "client" (client → server) or "server" (server → client).
    pub direction: String,
    pub opcode: u8,
    /// Full payload size; only the start is stored for huge messages.
    pub length: u64,
    /// Payload rendered for display (text, close code, or hex dump).
    pub text: String,
    #[serde(skip)]
    pub data: Bytes,
}

/// Entries kept per Repeater tab; older ones are dropped.
pub const REPEATER_HISTORY_LIMIT: i64 = 100;

/// One request sent from a Repeater tab. `response_raw` is only filled by
/// [`Store::repeater_entry`]; lists leave it empty.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeaterEntry {
    pub id: u64,
    pub tab_id: u64,
    pub ts: u64,
    pub target: String,
    pub raw: String,
    /// 0 when the request failed (see `error`).
    pub status: u16,
    pub duration_ms: u64,
    pub length: usize,
    pub response_raw: String,
    pub error: Option<String>,
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS flows (
    id          INTEGER PRIMARY KEY,
    ts          INTEGER NOT NULL,
    method      TEXT    NOT NULL,
    scheme      TEXT    NOT NULL,
    host        TEXT    NOT NULL,
    path        TEXT    NOT NULL,
    status      INTEGER NOT NULL,
    length      INTEGER NOT NULL,
    mime        TEXT    NOT NULL,
    duration_ms INTEGER NOT NULL,
    error       TEXT,
    version     TEXT    NOT NULL,
    req_headers TEXT    NOT NULL,
    req_body    BLOB    NOT NULL,
    res_version TEXT    NOT NULL,
    res_reason  TEXT    NOT NULL,
    res_headers TEXT    NOT NULL,
    res_body    BLOB    NOT NULL,
    note        TEXT    NOT NULL DEFAULT '',
    color       TEXT    NOT NULL DEFAULT '',
    -- bit 1: request body truncated, bit 2: response body truncated
    truncated   INTEGER NOT NULL DEFAULT 0,
    -- JSON of earlier request/response versions (Original / Extension), when captured
    variants    TEXT
);
CREATE TABLE IF NOT EXISTS ws_messages (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id   INTEGER NOT NULL,
    ts        INTEGER NOT NULL,
    direction TEXT    NOT NULL,
    opcode    INTEGER NOT NULL,
    length    INTEGER NOT NULL,
    data      BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS ws_messages_flow ON ws_messages (flow_id);
-- Body search. Trigram tokens let LIKE '%needle%' use the index (3+ chars).
CREATE VIRTUAL TABLE IF NOT EXISTS flows_fts USING fts5 (text, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS ws_fts USING fts5 (text, tokenize = 'trigram');
-- Requests sent from each Repeater tab, for back/forward navigation.
CREATE TABLE IF NOT EXISTS repeater_history (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    tab_id       INTEGER NOT NULL,
    ts           INTEGER NOT NULL,
    target       TEXT    NOT NULL,
    raw          TEXT    NOT NULL,
    status       INTEGER NOT NULL,
    duration_ms  INTEGER NOT NULL,
    length       INTEGER NOT NULL,
    response_raw TEXT    NOT NULL,
    error        TEXT
);
CREATE INDEX IF NOT EXISTS repeater_history_tab ON repeater_history (tab_id, id);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Process log (proxy errors, upstream failures, activity), kept with the project.
CREATE TABLE IF NOT EXISTS logs (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      INTEGER NOT NULL,
    level   TEXT    NOT NULL,
    source  TEXT    NOT NULL,
    message TEXT    NOT NULL
);
";

// note/color sit at index 11 in SUMMARY_COLS and 18 in FULL_COLS (see summary_from_row).
const SUMMARY_COLS: &str =
    "id, ts, method, scheme, host, path, status, length, mime, duration_ms, error, note, color";

const FULL_COLS: &str = "id, ts, method, scheme, host, path, status, length, mime, duration_ms, error,
     version, req_headers, req_body, res_version, res_reason, res_headers, res_body, note, color, truncated, variants";

struct Db {
    conn: Connection,
    path: PathBuf,
}

pub struct Store {
    db: Mutex<Db>,
    next: AtomicU64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let (conn, max_id) = connect(path)?;
        Ok(Self {
            db: Mutex::new(Db {
                conn,
                path: path.to_path_buf(),
            }),
            next: AtomicU64::new(max_id),
        })
    }

    /// Write a clean copy of the current project to `dest` (overwritten if present).
    pub fn save_as(&self, dest: &Path) -> Result<()> {
        let _ = std::fs::remove_file(dest);
        let db = self.db.lock().unwrap();
        db.conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// A clean, defragmented copy of the current project as raw `.cegat` bytes
    /// (the SQLite file). Used for the compressed `.cegatc` export.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        let tmp = std::env::temp_dir().join(format!(
            "{}-snapshot-{}-{}.{}",
            crate::brand::ID,
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
            crate::brand::PROJECT_EXT,
        ));
        self.save_as(&tmp)?;
        let bytes = std::fs::read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        Ok(bytes?)
    }

    /// Switch to another project file (created if missing).
    pub fn switch(&self, path: &Path) -> Result<()> {
        let (conn, max_id) = connect(path)?;
        let mut db = self.db.lock().unwrap();
        *db = Db {
            conn,
            path: path.to_path_buf(),
        };
        self.next.store(max_id, Ordering::Relaxed);
        Ok(())
    }

    pub fn path(&self) -> PathBuf {
        self.db.lock().unwrap().path.clone()
    }

    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Reseed the flow-id counter to the current maximum id in the table, so the next
    /// captured flow continues from there (ids become compact again after a delete, e.g.
    /// dropping to 0 -> next id 1 when History is emptied). Existing rows are untouched.
    pub fn reset_ids(&self) -> Result<()> {
        let db = self.db.lock().unwrap();
        let max_id: i64 = db.conn.query_row("SELECT COALESCE(MAX(id), 0) FROM flows", [], |r| r.get(0))?;
        self.next.store(max_id as u64, Ordering::Relaxed);
        Ok(())
    }

    pub fn push(&self, flow: Flow) -> Result<FlowSummary> {
        Ok(self.push_batch(vec![flow])?.pop().expect("one flow in, one summary out"))
    }

    /// Persist many flows in a single transaction, so a burst of traffic commits
    /// once instead of once per request. Returns the summaries in input order.
    pub fn push_batch(&self, flows: Vec<Flow>) -> Result<Vec<FlowSummary>> {
        if flows.is_empty() {
            return Ok(Vec::new());
        }
        // Decode bodies for the index outside the lock.
        let texts: Vec<String> = flows.iter().map(search_text).collect();
        let mut db = self.db.lock().unwrap();
        let tx = db.conn.transaction()?;
        {
            let mut flow_stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO flows
                   (id, ts, method, scheme, host, path, status, length, mime, duration_ms, error,
                    version, req_headers, req_body, res_version, res_reason, res_headers, res_body, truncated, variants)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            )?;
            let mut fts_stmt = tx.prepare_cached("INSERT OR REPLACE INTO flows_fts (rowid, text) VALUES (?1, ?2)")?;
            for (flow, text) in flows.iter().zip(&texts) {
                let s = &flow.summary;
                flow_stmt.execute(params![
                    s.id as i64,
                    s.ts as i64,
                    s.method,
                    s.scheme,
                    s.host,
                    s.path,
                    s.status,
                    s.length as i64,
                    s.mime,
                    s.duration_ms as i64,
                    s.error,
                    flow.version,
                    serde_json::to_string(&flow.req_headers)?,
                    &flow.req_body[..],
                    flow.res_version,
                    flow.res_reason,
                    serde_json::to_string(&flow.res_headers)?,
                    &flow.res_body[..],
                    flow.req_truncated as i64 | (flow.res_truncated as i64) << 1,
                    if flow.variants.is_empty() { None } else { Some(serde_json::to_string(&flow.variants)?) },
                ])?;
                fts_stmt.execute(params![s.id as i64, text])?;
            }
        }
        tx.commit()?;
        // The emitted summary reflects modification (variants present), same as what
        // summaries() derives on reload, so the History "Modified" column is consistent.
        Ok(flows
            .into_iter()
            .map(|mut f| {
                f.summary.modified = !f.variants.is_empty();
                f.summary
            })
            .collect())
    }

    /// IDs of flows whose headers, (decoded) bodies or WebSocket messages
    /// contain `needle`, case-insensitive, oldest first.
    pub fn search(&self, needle: &str) -> Result<Vec<u64>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let (cond, param) = fts_condition(needle);
        let db = self.db.lock().unwrap();
        let mut ids: Vec<u64> = {
            let mut stmt = db.conn.prepare(&format!(
                "SELECT rowid FROM flows_fts WHERE flows_fts.text {cond}
                 UNION
                 SELECT m.flow_id FROM ws_fts JOIN ws_messages m ON m.id = ws_fts.rowid
                 WHERE ws_fts.text {cond}"
            ))?;
            let rows = stmt.query_map([&param], |r| r.get::<_, i64>(0))?;
            rows.map(|r| r.map(|id| id as u64)).collect::<rusqlite::Result<_>>()?
        };

        // Flows the background indexer hasn't reached yet: check them directly.
        let lower = needle.to_lowercase();
        let mut stmt = db.conn.prepare(&format!(
            "SELECT {FULL_COLS} FROM flows WHERE id NOT IN (SELECT rowid FROM flows_fts)"
        ))?;
        for flow in stmt.query_map([], flow_from_row)? {
            let flow = flow?;
            if search_text(&flow).to_lowercase().contains(&lower) {
                ids.push(flow.summary.id);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Index flows saved before search indexing existed, a batch at a time so
    /// the proxy isn't blocked. Stops if the project is switched meanwhile.
    pub fn backfill_search_index(&self) -> Result<usize> {
        let path = self.path();
        let mut done = 0;
        loop {
            let batch: Vec<Flow> = {
                let db = self.db.lock().unwrap();
                if db.path != path {
                    return Ok(done);
                }
                let mut stmt = db.conn.prepare(&format!(
                    "SELECT {FULL_COLS} FROM flows WHERE id NOT IN (SELECT rowid FROM flows_fts) LIMIT 200"
                ))?;
                let rows = stmt.query_map([], flow_from_row)?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            if batch.is_empty() {
                return Ok(done);
            }
            let texts: Vec<(u64, String)> = batch.iter().map(|f| (f.summary.id, search_text(f))).collect();
            let mut db = self.db.lock().unwrap();
            if db.path != path {
                return Ok(done);
            }
            let tx = db.conn.transaction()?;
            {
                let mut stmt = tx.prepare("INSERT INTO flows_fts (rowid, text) VALUES (?1, ?2)")?;
                for (id, text) in &texts {
                    stmt.execute(params![*id as i64, text])?;
                }
            }
            tx.commit()?;
            done += texts.len();
        }
    }

    pub fn summaries(&self) -> Result<Vec<FlowSummary>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .conn
            .prepare(&format!("SELECT {SUMMARY_COLS}, LENGTH(req_body), (variants IS NOT NULL) FROM flows ORDER BY id"))?;
        let rows = stmt.query_map([], |r| {
            let req_length = r.get::<_, i64>(13)? as usize;
            let modified = r.get::<_, i64>(14)? != 0;
            summary_from_row(r, 11, req_length, modified)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: u64) -> Result<Option<Flow>> {
        let db = self.db.lock().unwrap();
        let flow = db
            .conn
            .query_row(&format!("SELECT {FULL_COLS} FROM flows WHERE id = ?1"), [id as i64], flow_from_row)
            .optional()?;
        Ok(flow)
    }

    /// Every flow with bodies, oldest first (used for export).
    pub fn all(&self) -> Result<Vec<Flow>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .conn
            .prepare(&format!("SELECT {FULL_COLS} FROM flows ORDER BY id"))?;
        let rows = stmt.query_map([], flow_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_meta(&self, id: u64, note: &str, color: &str) -> Result<()> {
        self.db.lock().unwrap().conn.execute(
            "UPDATE flows SET note = ?1, color = ?2 WHERE id = ?3",
            params![note, color, id as i64],
        )?;
        Ok(())
    }

    /// Project-level setting (scope, Repeater tabs, …), stored as JSON.
    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let db = self.db.lock().unwrap();
        let value: Option<String> = db
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(value.map(|v| serde_json::from_str(&v)).transpose()?)
    }

    pub fn put_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.db.lock().unwrap().conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    pub fn get_list(&self, key: &str) -> Result<Vec<String>> {
        Ok(match self.get_setting(key)? {
            Some(v) => serde_json::from_value(v)?,
            None => Vec::new(),
        })
    }

    pub fn put_list(&self, key: &str, list: &[String]) -> Result<()> {
        self.put_setting(key, &serde_json::to_value(list)?)
    }

    pub fn delete(&self, ids: &[u64]) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.conn.transaction()?;
        {
            let mut flows = tx.prepare("DELETE FROM flows WHERE id = ?1")?;
            let mut fts = tx.prepare("DELETE FROM flows_fts WHERE rowid = ?1")?;
            let mut ws_fts =
                tx.prepare("DELETE FROM ws_fts WHERE rowid IN (SELECT id FROM ws_messages WHERE flow_id = ?1)")?;
            let mut ws = tx.prepare("DELETE FROM ws_messages WHERE flow_id = ?1")?;
            for id in ids {
                flows.execute([*id as i64])?;
                fts.execute([*id as i64])?;
                ws_fts.execute([*id as i64])?;
                ws.execute([*id as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .conn
            .execute_batch("DELETE FROM flows; DELETE FROM flows_fts; DELETE FROM ws_messages; DELETE FROM ws_fts;")?;
        Ok(())
    }

    /// Append a log line; returns its row id. Keeps at most `LOG_STORE_LIMIT` rows.
    pub fn push_log(&self, ts: u64, level: &str, source: &str, message: &str) -> Result<u64> {
        const LOG_STORE_LIMIT: i64 = 20_000;
        let db = self.db.lock().unwrap();
        db.conn.execute(
            "INSERT INTO logs (ts, level, source, message) VALUES (?1, ?2, ?3, ?4)",
            params![ts as i64, level, source, message],
        )?;
        let id = db.conn.last_insert_rowid();
        if id % 256 == 0 {
            db.conn.execute("DELETE FROM logs WHERE id <= ?1", [id - LOG_STORE_LIMIT])?;
        }
        Ok(id as u64)
    }

    /// The most recent `limit` log lines, oldest first.
    pub fn recent_logs(&self, limit: usize) -> Result<Vec<LogEntry>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(
            "SELECT id, ts, level, source, message FROM (SELECT id, ts, level, source, message FROM logs ORDER BY id DESC LIMIT ?1) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(LogEntry {
                id: r.get::<_, i64>(0)? as u64,
                ts: r.get::<_, i64>(1)? as u64,
                level: r.get(2)?,
                source: r.get(3)?,
                message: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn clear_logs(&self) -> Result<()> {
        self.db.lock().unwrap().conn.execute("DELETE FROM logs", [])?;
        Ok(())
    }

    /// Delete specific log lines by id.
    pub fn delete_logs(&self, ids: &[u64]) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.conn.transaction()?;
        {
            let mut stmt = tx.prepare("DELETE FROM logs WHERE id = ?1")?;
            for id in ids {
                stmt.execute([*id as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn push_ws(
        &self,
        flow_id: u64,
        ts: u64,
        direction: &str,
        opcode: u8,
        length: u64,
        data: &[u8],
    ) -> Result<WsMessage> {
        let db = self.db.lock().unwrap();
        db.conn.execute(
            "INSERT INTO ws_messages (flow_id, ts, direction, opcode, length, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![flow_id as i64, ts as i64, direction, opcode, length as i64, data],
        )?;
        let id = db.conn.last_insert_rowid();
        if opcode == 1 || opcode == 2 {
            let text = String::from_utf8_lossy(&data[..data.len().min(INDEX_LIMIT)]);
            db.conn.execute("INSERT INTO ws_fts (rowid, text) VALUES (?1, ?2)", params![id, text])?;
        }
        let data = Bytes::copy_from_slice(data);
        Ok(WsMessage {
            id: id as u64,
            flow_id,
            ts,
            direction: direction.to_string(),
            opcode,
            length,
            text: crate::ws::display(opcode, &data),
            data,
        })
    }

    /// Messages of one WebSocket flow, oldest first.
    pub fn ws_messages(&self, flow_id: u64) -> Result<Vec<WsMessage>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(
            "SELECT id, flow_id, ts, direction, opcode, length, data
             FROM ws_messages WHERE flow_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([flow_id as i64], ws_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a Repeater send, keeping the newest entries of that tab. Returns the entry id.
    pub fn push_repeater(&self, e: &RepeaterEntry) -> Result<u64> {
        let db = self.db.lock().unwrap();
        db.conn.execute(
            "INSERT INTO repeater_history
               (tab_id, ts, target, raw, status, duration_ms, length, response_raw, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.tab_id as i64,
                e.ts as i64,
                e.target,
                e.raw,
                e.status,
                e.duration_ms as i64,
                e.length as i64,
                e.response_raw,
                e.error
            ],
        )?;
        let id = db.conn.last_insert_rowid();
        db.conn.execute(
            "DELETE FROM repeater_history WHERE tab_id = ?1 AND id NOT IN
               (SELECT id FROM repeater_history WHERE tab_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![e.tab_id as i64, REPEATER_HISTORY_LIMIT],
        )?;
        Ok(id as u64)
    }

    /// A tab's sends, oldest first, without response bodies.
    pub fn repeater_history(&self, tab_id: u64) -> Result<Vec<RepeaterEntry>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(
            "SELECT id, tab_id, ts, target, raw, status, duration_ms, length, '', error
             FROM repeater_history WHERE tab_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([tab_id as i64], repeater_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn repeater_entry(&self, id: u64) -> Result<Option<RepeaterEntry>> {
        let db = self.db.lock().unwrap();
        Ok(db
            .conn
            .query_row(
                "SELECT id, tab_id, ts, target, raw, status, duration_ms, length, response_raw, error
                 FROM repeater_history WHERE id = ?1",
                [id as i64],
                repeater_from_row,
            )
            .optional()?)
    }

    pub fn clear_repeater_history(&self, tab_id: u64) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .conn
            .execute("DELETE FROM repeater_history WHERE tab_id = ?1", [tab_id as i64])?;
        Ok(())
    }

    /// Every WebSocket message, oldest first (used for export).
    pub fn all_ws(&self) -> Result<Vec<WsMessage>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(
            "SELECT id, flow_id, ts, direction, opcode, length, data FROM ws_messages ORDER BY id",
        )?;
        let rows = stmt.query_map([], ws_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Marks a `.cegat` file as a Cegat project inside the SQLite header
/// (`PRAGMA application_id`, offset 68). Value spells "CGAT" in ASCII, so
/// `file`/`sqlite3` and other tools can recognise our projects.
pub const APPLICATION_ID: i32 = 0x4347_4154;
/// Project schema version stored in `PRAGMA user_version`.
pub const USER_VERSION: i32 = 1;

fn connect(path: &Path) -> Result<(Connection, u64)> {
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    // Stamp the Cegat header markers (idempotent, cheap).
    conn.pragma_update(None, "application_id", APPLICATION_ID)?;
    conn.pragma_update(None, "user_version", USER_VERSION)?;
    let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM flows", [], |r| r.get(0))?;
    Ok((conn, max_id as u64))
}

/// Add columns introduced after a project file was first created.
fn migrate(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('flows')")?;
    let cols: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let added = [
        ("note", "TEXT NOT NULL DEFAULT ''"),
        ("color", "TEXT NOT NULL DEFAULT ''"),
        ("truncated", "INTEGER NOT NULL DEFAULT 0"),
        ("variants", "TEXT"),
    ];
    for (col, def) in added {
        if !cols.iter().any(|c| c == col) {
            conn.execute_batch(&format!("ALTER TABLE flows ADD COLUMN {col} {def}"))?;
        }
    }
    Ok(())
}

fn summary_from_row(r: &Row, meta_idx: usize, req_length: usize, modified: bool) -> rusqlite::Result<FlowSummary> {
    Ok(FlowSummary {
        id: r.get::<_, i64>(0)? as u64,
        ts: r.get::<_, i64>(1)? as u64,
        method: r.get(2)?,
        scheme: r.get(3)?,
        host: r.get(4)?,
        path: r.get(5)?,
        status: r.get(6)?,
        length: r.get::<_, i64>(7)? as usize,
        req_length,
        mime: r.get(8)?,
        duration_ms: r.get::<_, i64>(9)? as u64,
        error: r.get(10)?,
        note: r.get(meta_idx)?,
        color: r.get(meta_idx + 1)?,
        streaming: false,
        modified,
    })
}

fn flow_from_row(r: &Row) -> rusqlite::Result<Flow> {
    let truncated: i64 = r.get(20)?;
    let req_body = Bytes::from(r.get::<_, Vec<u8>>(13)?);
    let variants_json: Option<String> = r.get(21)?;
    let modified = variants_json.as_deref().is_some_and(|s| !s.is_empty());
    Ok(Flow {
        req_truncated: truncated & 1 != 0,
        res_truncated: truncated & 2 != 0,
        summary: summary_from_row(r, 18, req_body.len(), modified)?,
        version: r.get(11)?,
        req_headers: json_col(r, 12)?,
        req_body,
        res_version: r.get(14)?,
        res_reason: r.get(15)?,
        res_headers: json_col(r, 16)?,
        res_body: Bytes::from(r.get::<_, Vec<u8>>(17)?),
        variants: variants_json
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
    })
}

fn repeater_from_row(r: &Row) -> rusqlite::Result<RepeaterEntry> {
    Ok(RepeaterEntry {
        id: r.get::<_, i64>(0)? as u64,
        tab_id: r.get::<_, i64>(1)? as u64,
        ts: r.get::<_, i64>(2)? as u64,
        target: r.get(3)?,
        raw: r.get(4)?,
        status: r.get(5)?,
        duration_ms: r.get::<_, i64>(6)? as u64,
        length: r.get::<_, i64>(7)? as usize,
        response_raw: r.get(8)?,
        error: r.get(9)?,
    })
}

fn ws_from_row(r: &Row) -> rusqlite::Result<WsMessage> {
    let opcode: u8 = r.get(4)?;
    let data = Bytes::from(r.get::<_, Vec<u8>>(6)?);
    Ok(WsMessage {
        id: r.get::<_, i64>(0)? as u64,
        flow_id: r.get::<_, i64>(1)? as u64,
        ts: r.get::<_, i64>(2)? as u64,
        direction: r.get(3)?,
        opcode,
        length: r.get::<_, i64>(5)? as u64,
        text: crate::ws::display(opcode, &data),
        data,
    })
}

fn json_col(r: &Row, idx: usize) -> rusqlite::Result<Vec<(String, String)>> {
    let text: String = r.get(idx)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Substring condition on an FTS5 trigram column. A quoted phrase of 3+
/// characters is answered from the index; shorter needles need LIKE, which
/// scans (the index can't serve LIKE with an ESCAPE clause).
fn fts_condition(needle: &str) -> (&'static str, String) {
    if needle.chars().count() >= 3 {
        ("MATCH ?1", format!("\"{}\"", needle.replace('"', "\"\"")))
    } else {
        let escaped = needle.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        ("LIKE ?1 ESCAPE '\\'", format!("%{escaped}%"))
    }
}

/// Decoded text indexed per body; the rest of a huge body isn't searchable. Kept at
/// the viewer's cap (VIEW_LIMIT): the default view only shows this much anyway, so
/// indexing more just bloats the full-text index (which is the dominant cost in a
/// large project) without making anything you can actually see searchable.
const INDEX_LIMIT: usize = crate::raw::VIEW_LIMIT;

/// What body search looks through: URL, headers and decoded text bodies.
fn search_text(f: &Flow) -> String {
    let mut out = format!("{} {}://{}{}\n", f.summary.method, f.summary.scheme, f.summary.host, f.summary.path);
    for (headers, body) in [(&f.req_headers, &f.req_body), (&f.res_headers, &f.res_body)] {
        for (k, v) in headers {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push('\n');
        }
        // Skip binary bodies entirely (protobuf, fonts, images, octet-stream, anything
        // with NUL bytes): they are not meaningfully searchable as text and otherwise
        // bloat the full-text index (the dominant cost in a large project). Uses the same
        // classifier as the UI's hex view, so "binary" means the same thing everywhere.
        // Headers above are still indexed, so you can search a binary response's headers.
        if crate::raw::body_is_binary(headers, body) {
            continue;
        }
        // Only decompress up to what we actually index (INDEX_LIMIT). decode_body would
        // expand the WHOLE body first (a gzip'd 1 MB response can be 80 MB decoded) just
        // to throw away everything past the cap; the capped variant stops early.
        let (decoded, _more) = crate::raw::decode_body_capped(headers, body, INDEX_LIMIT);
        out.push_str(&String::from_utf8_lossy(&decoded[..decoded.len().min(INDEX_LIMIT)]));
        out.push('\n');
    }
    out
}

pub fn header_vec(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            (
                title_case(k.as_str()),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect()
}

pub fn fmt_version(v: Version) -> &'static str {
    match v {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/1.1",
    }
}

fn title_case(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn display_host(uri: &Uri) -> String {
    let host = uri.host().unwrap_or_default();
    let default_port = if uri.scheme_str() == Some("https") { 443 } else { 80 };
    match uri.port_u16() {
        Some(p) if p != default_port => format!("{host}:{p}"),
        _ => host.to_string(),
    }
}

/// "application/json; charset=utf-8" -> "json"
fn short_mime(ct: &str) -> String {
    let essence = ct.split(';').next().unwrap_or("").trim();
    essence
        .rsplit('/')
        .next()
        .unwrap_or(essence)
        .trim_start_matches("x-")
        .to_ascii_lowercase()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;

    #[test]
    fn migrates_old_project_and_saves_meta() {
        let path = std::env::temp_dir().join(format!("intip-test-{}.intip", std::process::id()));
        let _ = std::fs::remove_file(&path);

        // A project created before note/color/truncated existed.
        let old = Connection::open(&path).unwrap();
        let old_schema = SCHEMA.replace(
            ",\n    note        TEXT    NOT NULL DEFAULT '',\n    color       TEXT    NOT NULL DEFAULT '',\n    -- bit 1: request body truncated, bit 2: response body truncated\n    truncated   INTEGER NOT NULL DEFAULT 0",
            "",
        );
        assert!(!old_schema.contains("color"), "old schema really lacks the new columns");
        old.execute_batch(&old_schema).unwrap();
        drop(old);

        let store = Store::open(&path).unwrap();
        let uri: Uri = "https://example.com/x".parse().unwrap();
        let flow = Flow::request(store.next_id(), &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new());
        let id = store.push(flow).unwrap().id;

        store.set_meta(id, "cek IDOR di sini", "red").unwrap();
        let s = &store.summaries().unwrap()[0];
        assert_eq!((s.note.as_str(), s.color.as_str()), ("cek IDOR di sini", "red"));
        assert_eq!(store.get(id).unwrap().unwrap().summary.note, "cek IDOR di sini");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn flow_variants_round_trip() {
        let path = std::env::temp_dir().join(format!("intip-variants-{}.intip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let uri: Uri = "https://example.com/x".parse().unwrap();
        let mut flow = Flow::request(store.next_id(), &Method::POST, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new());
        flow.variants.req_original = Some("POST /x HTTP/1.1\n\noriginal".into());
        flow.variants.res_original = Some("HTTP/1.1 200 OK\n\noriginal-res".into());
        let id = store.push(flow).unwrap().id;

        let got = store.get(id).unwrap().unwrap();
        assert_eq!(got.variants.req_original.as_deref(), Some("POST /x HTTP/1.1\n\noriginal"));
        assert_eq!(got.variants.res_original.as_deref(), Some("HTTP/1.1 200 OK\n\noriginal-res"));
        assert!(got.variants.req_extension.is_none());

        // A flow with no variants stores NULL (not "{}") and reads back empty.
        let plain = Flow::request(store.next_id(), &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new());
        let pid = store.push(plain).unwrap().id;
        assert!(store.get(pid).unwrap().unwrap().variants.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn logs_persist_with_the_project() {
        let path = std::env::temp_dir().join(format!("cegat-logs-{}.cegat", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            store.push_log(1000, "info", "proxy", "listening on 127.0.0.1:8080").unwrap();
            store.push_log(2000, "error", "tls", "handshake failed").unwrap();
            let logs = store.recent_logs(10).unwrap();
            assert_eq!(logs.len(), 2);
            assert_eq!((logs[0].level.as_str(), logs[1].level.as_str()), ("info", "error")); // oldest first
        }
        // Reopening the same file still has the logs (they live in the .cegat).
        let store = Store::open(&path).unwrap();
        let logs = store.recent_logs(10).unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[1].message, "handshake failed");
        assert_eq!(logs[1].ts, 2000);
        // Deleting one line leaves the other.
        store.delete_logs(&[logs[0].id]).unwrap();
        let after = store.recent_logs(10).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].message, "handshake failed");
        store.clear_logs().unwrap();
        assert!(store.recent_logs(10).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn snapshot_is_a_valid_stamped_cegat_file() {
        let path = std::env::temp_dir().join(format!("cegat-snap-src-{}.cegat", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let uri: Uri = "https://example.com/x".parse().unwrap();
        let flow = Flow::request(store.next_id(), &Method::POST, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::from_static(b"role=admin"));
        store.push(flow).unwrap();

        // The live file carries the Cegat header markers.
        {
            let db = store.db.lock().unwrap();
            let app_id: i32 = db.conn.query_row("PRAGMA application_id", [], |r| r.get(0)).unwrap();
            let ver: i32 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(app_id, APPLICATION_ID);
            assert_eq!(ver, USER_VERSION);
        }

        // A snapshot is a self-standing SQLite file that reopens with the same data.
        let bytes = store.snapshot().unwrap();
        assert!(bytes.starts_with(b"SQLite format 3\0"), "snapshot is a SQLite file");
        let dest = std::env::temp_dir().join(format!("cegat-snap-dst-{}.cegat", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        std::fs::write(&dest, &bytes).unwrap();
        let reopened = Store::open(&dest).unwrap();
        assert_eq!(reopened.summaries().unwrap().len(), 1);
        assert_eq!(reopened.summaries().unwrap()[0].req_length, 10, "req body size survives");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn body_search_uses_index_and_backfills_old_flows() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("intip-search-{}.intip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let uri: Uri = "https://example.com/api".parse().unwrap();

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"{\"token\":\"Secret_42%\"}").unwrap();
        let mut res_headers = HeaderMap::new();
        res_headers.insert("content-encoding", "gzip".parse().unwrap());
        let mut flow = Flow::request(store.next_id(), &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new());
        flow.set_response(StatusCode::OK, Version::HTTP_11, &res_headers, Bytes::from(gz.finish().unwrap()));
        let a = store.push(flow).unwrap().id;

        let other = Flow::request(store.next_id(), &Method::POST, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::from_static(b"nothing here"));
        let b = store.push(other).unwrap().id;

        assert_eq!(store.search("secret_42%").unwrap(), vec![a], "decoded, case-insensitive, wildcards literal");
        assert_eq!(store.search("\"token\"").unwrap(), vec![a], "quotes in the needle");
        assert_eq!(store.search("_4").unwrap(), vec![a], "short needles work too");
        assert_eq!(store.search("NOTHING").unwrap(), vec![b]);
        assert!(store.search("secret_4x").unwrap().is_empty());

        // A flow missing from the index (older project) is still found, then indexed.
        store.db.lock().unwrap().conn.execute("DELETE FROM flows_fts WHERE rowid = ?1", [b as i64]).unwrap();
        assert_eq!(store.search("nothing").unwrap(), vec![b]);
        assert_eq!(store.backfill_search_index().unwrap(), 1);
        assert_eq!(store.search("nothing").unwrap(), vec![b]);

        store.push_ws(a, 1, "server", 1, 5, b"ws-hit").unwrap();
        assert_eq!(store.search("WS-HIT").unwrap(), vec![a], "WebSocket messages are searched");

        store.delete(&[a]).unwrap();
        assert!(store.search("ws-hit").unwrap().is_empty());

        let (cond, _) = fts_condition("needle");
        let plan: String = store
            .db
            .lock()
            .unwrap()
            .conn
            .query_row(&format!("EXPLAIN QUERY PLAN SELECT rowid FROM flows_fts WHERE text {cond}"), ["\"needle\""], |r| r.get(3))
            .unwrap();
        assert!(plan.contains("INDEX 0:M"), "search uses the FTS index: {plan}");
        let _ = std::fs::remove_file(&path);
    }
}
