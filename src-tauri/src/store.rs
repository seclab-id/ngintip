//! HTTP history persisted in SQLite. One `.ngintip` file = one project.

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
    /// Which project this flow belongs to: the store generation current when its id was
    /// minted, NOT when it was handed to the writer. Those differ by a whole upstream
    /// round trip, and the gap is exactly where a project switch lands.
    pub gen: u64,
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
    /// Tag this flow with the store generation its id was minted under.
    pub fn at(mut self, gen: u64) -> Self {
        self.gen = gen;
        self
    }

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
            gen: 0,
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

/// Totals for one flow's WebSocket conversation, so a caller can decide whether it is
/// worth paging at all. `bytes` sums the TRUE payload sizes, which can exceed what was
/// stored: capture keeps only the start of a huge message.
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsStats {
    pub count: u64,
    pub client: u64,
    pub server: u64,
    pub bytes: u64,
}

/// One message on a page, with the sizes that keep a shortened payload honest. Three
/// numbers can differ: `msg.length` is what the peer sent, `stored` is what capture
/// kept, and `msg.data.len()` is what this page asked for.
pub struct WsPageRow {
    pub msg: WsMessage,
    pub stored: u64,
}

/// Project-wide counters for a status answer, from two indexed aggregates rather than
/// loading every row the way `summaries` does.
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreStats {
    pub flows: u64,
    pub ws_messages: u64,
    /// Timestamp of the newest captured flow, 0 when there are none.
    pub newest_ts: u64,
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

/// The two search tables, kept separate because `clear` drops and recreates them and
/// must do it from the same definition SCHEMA uses. A test asserts SCHEMA contains this.
const FTS_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS flows_fts USING fts5 (text, tokenize = 'trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS ws_fts USING fts5 (text, tokenize = 'trigram');
";

/// One Intruder result row. The four text fields are empty in a list read and filled by
/// `intruder_result`, the same split `repeater_history` uses: a run can be thousands of
/// rows, and the table only shows number, status, length, time and payload.
#[derive(Clone, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntruderRow {
    pub n: u64,
    pub payloads: Vec<String>,
    pub status: u16,
    pub length: usize,
    pub time_ms: u64,
    pub error: Option<String>,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub request_raw: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub response_raw: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub request_final: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub response_original: String,
}

/// `null` means "no text for this row", not a type error.
///
/// Only the first `KEEP_RESPONSES` rows of a run carry bodies, and `requestFinal` /
/// `responseOriginal` exist only when an extension changed the message, so the window
/// sends JSON null for all of them routinely. `#[serde(default)]` alone does not cover
/// this: it fills a MISSING field, while a present null is still a type mismatch.
fn null_to_empty<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    use serde::Deserialize;
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
-- Wait for a lock instead of failing instantly. Without it a writer that meets any
-- contention gets SQLITE_BUSY straight away and a whole batch of captured flows is
-- lost (see Ctx::record_flow). Five seconds is far longer than any commit here.
PRAGMA busy_timeout = 5000;
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
-- Results of each Intruder attack, kept with the project the way Repeater sends are.
-- Its own table rather than a settings blob: a run is up to KEEP_RESPONSES bodies, which
-- has no business being loaded and rewritten whole on every unrelated settings write.
CREATE TABLE IF NOT EXISTS intruder_results (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    tab_id            INTEGER NOT NULL,
    n                 INTEGER NOT NULL,
    payloads          TEXT    NOT NULL,
    status            INTEGER NOT NULL,
    length            INTEGER NOT NULL,
    time_ms           INTEGER NOT NULL,
    error             TEXT,
    request_raw       TEXT    NOT NULL,
    response_raw      TEXT    NOT NULL,
    request_final     TEXT    NOT NULL,
    response_original TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS intruder_results_tab ON intruder_results (tab_id, n);
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
    /// Bumped every time this store is pointed at a different project.
    ///
    /// Capture is asynchronous: `Ctx::record_flow` queues a flow and a writer thread
    /// commits it later. Without a generation, a flow captured under project A that is
    /// still in the queue when the user opens project B is written into B's file, which
    /// for separate client engagements is a confidentiality problem, not a tidiness one.
    generation: AtomicU64,
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
            // Starts at 1, not 0, because `Flow::gen` defaults to 0: a future call site
            // that forgets `.at(gen)` then fails immediately, in development, instead of
            // working perfectly until the first project switch and silently dropping
            // every flow after it.
            generation: AtomicU64::new(1),
        })
    }

    /// Write a clean copy of the current project to `dest` (overwritten if present).
    pub fn save_as(&self, dest: &Path) -> Result<()> {
        let _ = std::fs::remove_file(dest);
        let db = self.db.lock().unwrap();
        db.conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// A clean, defragmented copy of the current project as raw `.ngintip` bytes
    /// (the SQLite file). Used for the compressed `.ngintipc` export.
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
        // Bumped while the lock is held, so nothing can read the old generation and then
        // write into the new connection.
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Which project this store is currently open on. Compare a value taken earlier to
    /// find out whether the project changed in between.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn path(&self) -> PathBuf {
        self.db.lock().unwrap().path.clone()
    }

    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// An id, with the generation it belongs to.
    ///
    /// The generation is read FIRST on purpose. If a switch slips in between the two
    /// reads, the flow carries the older generation and is dropped (loudly) rather than
    /// written into the project that just opened with an id minted from the previous
    /// project's counter, which `INSERT OR REPLACE` would use to overwrite a real row.
    pub fn next_id_at(&self) -> (u64, u64) {
        let gen = self.generation();
        (self.next_id(), gen)
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
        Ok(self.push_batch_at(flows, None)?.unwrap_or_default())
    }

    /// Persist a batch, but only while the store is still open on the project the flows
    /// were captured in.
    ///
    /// `Some(gen)` makes the write conditional and returns `None` when it was skipped.
    /// The comparison happens AFTER the connection lock is taken, which is the whole
    /// point: checking the generation first and inserting second is a race with a window
    /// wide enough to matter, and losing it writes one client's traffic into another
    /// client's project. Worse, the id was minted from the previous project's counter,
    /// and `INSERT OR REPLACE` would then overwrite whatever row holds that id here.
    pub fn push_batch_at(&self, flows: Vec<Flow>, gen: Option<u64>) -> Result<Option<Vec<FlowSummary>>> {
        if flows.is_empty() {
            return Ok(Some(Vec::new()));
        }
        // Decode bodies for the index outside the lock.
        let texts: Vec<String> = flows.iter().map(search_text).collect();
        let mut db = self.db.lock().unwrap();
        if let Some(gen) = gen {
            if self.generation.load(Ordering::SeqCst) != gen {
                return Ok(None);
            }
        }
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
        Ok(Some(
            flows
                .into_iter()
                .map(|mut f| {
                    f.summary.modified = !f.variants.is_empty();
                    f.summary
                })
                .collect(),
        ))
    }

    /// IDs of flows whose headers, (decoded) bodies or WebSocket messages
    /// contain `needle`, case-insensitive, oldest first.
    pub fn search(&self, needle: &str) -> Result<Vec<u64>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        // WebSocket messages first, before taking the lock, since `ws_search` takes it
        // itself. Splitting the old single UNION into two queries is behaviour
        // preserving: the ids are sorted and deduped at the end either way, and now
        // "a message matched" has exactly one definition, which `ws_search` owns.
        let mut ids = self.ws_search(needle)?;
        let (cond, param) = fts_condition(needle);
        let db = self.db.lock().unwrap();
        {
            let mut stmt = db.conn.prepare(&format!("SELECT rowid FROM flows_fts WHERE flows_fts.text {cond}"))?;
            let rows = stmt.query_map([&param], |r| r.get::<_, i64>(0))?;
            for id in rows {
                ids.push(id? as u64);
            }
        }

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

    /// Like `put_setting`, but writes nothing when the store moved to another project
    /// since `gen` was taken. Returns whether it wrote.
    ///
    /// For read-modify-write callers, which load a value, work on it, and write the whole
    /// thing back. If the project changes in between, that write replaces the NEW
    /// project's value with the previous project's wholesale. For the Organizer that is
    /// one client's saved requests and agent findings replacing another's, in the surface
    /// whose entire purpose is to feed a client-facing report.
    pub fn put_setting_at(&self, key: &str, value: &serde_json::Value, gen: u64) -> Result<bool> {
        let db = self.db.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != gen {
            return Ok(false);
        }
        db.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(true)
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

    /// Rows per delete transaction.
    ///
    /// Deleting is the expensive direction for `flows_fts`: it is a plain FTS5 table, so
    /// removing a row means reading the stored document back and writing a delete marker
    /// for every token, and the trigram tokenizer emits roughly one token per character
    /// of up to `INDEX_LIMIT` bytes per flow. One transaction over 16,000 flows therefore
    /// holds every dirty index page until the commit, which grows the WAL past the size
    /// of the database and starves everything else of the connection for minutes.
    ///
    /// Chunking bounds the WAL and, because the lock is taken per chunk, lets the flow
    /// writer thread interleave so capture keeps working while a big delete runs.
    const DELETE_CHUNK: usize = 500;

    /// Delete flows by id, with their search-index rows and WebSocket messages.
    ///
    /// Commits per chunk, so an error partway leaves the earlier chunks deleted. That is
    /// the right trade for this operation: the alternative is a single transaction that
    /// cannot finish at all on a large History, and a partial delete of rows the user
    /// asked to delete is recoverable by repeating it, while a hung window is not.
    /// Returns how many ids were actually processed. That is every id unless the store
    /// moved to another project partway, which the chunking makes reachable: the lock is
    /// released between chunks, so an `open_project` on another thread can swap the
    /// connection and the remaining chunks would otherwise delete those rowids out of the
    /// project the operator just opened.
    pub fn delete(&self, ids: &[u64], gen: u64) -> Result<usize> {
        let mut done = 0;
        for chunk in ids.chunks(Self::DELETE_CHUNK) {
            let mut db = self.db.lock().unwrap();
            if self.generation.load(Ordering::SeqCst) != gen {
                break; // another project is open now; these ids mean nothing in it
            }
            let tx = db.conn.transaction()?;
            {
                let mut flows = tx.prepare("DELETE FROM flows WHERE id = ?1")?;
                let mut fts = tx.prepare("DELETE FROM flows_fts WHERE rowid = ?1")?;
                let mut ws_fts =
                    tx.prepare("DELETE FROM ws_fts WHERE rowid IN (SELECT id FROM ws_messages WHERE flow_id = ?1)")?;
                let mut ws = tx.prepare("DELETE FROM ws_messages WHERE flow_id = ?1")?;
                for id in chunk {
                    flows.execute([*id as i64])?;
                    fts.execute([*id as i64])?;
                    ws_fts.execute([*id as i64])?;
                    ws.execute([*id as i64])?;
                }
            }
            tx.commit()?;
            done += chunk.len();
        }
        Ok(done)
    }

    /// Empty History: every flow, its search-index rows and every WebSocket message.
    ///
    /// The FTS tables are DROPped and recreated rather than deleted from. `flows_fts` is
    /// a virtual table, so `DELETE FROM flows_fts` removes rows one at a time through
    /// FTS5's update hook, paying the same per-trigram cost as deleting each flow by hand
    /// but over every row at once. That is the same stall the History "Clear" button sits
    /// one click away from, with no chunking to soften it. Dropping the index is constant
    /// time, and the cheap `'delete-all'` command is not available here because these are
    /// plain content tables, not contentless ones.
    pub fn clear(&self, gen: u64) -> Result<()> {
        let db = self.db.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != gen {
            return Ok(()); // another project is open now; its History is not ours to empty
        }
        // One transaction, so a failure cannot leave the project with no search tables:
        // every later capture would fail its whole batch and the project would only heal
        // on reopen. The recreate uses FTS_SCHEMA rather than re-running SCHEMA, because
        // SCHEMA carries `PRAGMA journal_mode`, which errors inside a transaction on any
        // connection not already in WAL.
        db.conn.execute_batch(&format!(
            "BEGIN;
             DROP TABLE IF EXISTS flows_fts;
             DROP TABLE IF EXISTS ws_fts;
             DELETE FROM flows;
             DELETE FROM ws_messages;
             {FTS_SCHEMA}
             COMMIT;"
        ))?;
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

    /// Totals for one flow's WebSocket messages. One row, served from the
    /// `ws_messages_flow` index.
    pub fn ws_stats(&self, flow_id: u64) -> Result<WsStats> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(direction = 'client'), 0),
                    COALESCE(SUM(direction = 'server'), 0),
                    COALESCE(SUM(length), 0)
             FROM ws_messages WHERE flow_id = ?1",
        )?;
        Ok(stmt.query_row([flow_id as i64], |r| {
            Ok(WsStats {
                count: r.get::<_, i64>(0)? as u64,
                client: r.get::<_, i64>(1)? as u64,
                server: r.get::<_, i64>(2)? as u64,
                bytes: r.get::<_, i64>(3)? as u64,
            })
        })?)
    }

    /// A page of one flow's WebSocket messages, oldest first, with ids above `after`.
    ///
    /// Only `cap` bytes of each payload leave SQLite (`SUBSTR` in the query), so a
    /// socket of 1 MB messages costs the cap per message rather than a megabyte. That
    /// is the difference between this and [`Store::ws_messages`], which loads whole
    /// payloads because the window opens one flow at a time; this one can be called in
    /// a loop by an agent.
    ///
    /// `contains` reuses the same FTS condition as [`Store::search`], so a match here
    /// means exactly what a match there meant. Note `ws_fts` only indexes text and
    /// binary messages, and only their first `INDEX_LIMIT` bytes.
    pub fn ws_page(
        &self,
        flow_id: u64,
        after: u64,
        direction: Option<&str>,
        contains: Option<&str>,
        limit: usize,
        cap: usize,
    ) -> Result<Vec<WsPageRow>> {
        let fts = contains.filter(|c| !c.is_empty()).map(fts_condition);
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        // `fts_condition` hardcodes ?1, so its parameter has to be the first one bound.
        if let Some((_, p)) = &fts {
            params.push(Box::new(p.clone()));
        }
        params.push(Box::new(cap as i64));
        let cap_n = params.len();
        params.push(Box::new(flow_id as i64));
        let flow_n = params.len();
        params.push(Box::new(after as i64));
        let after_n = params.len();

        let mut sql = format!(
            "SELECT m.id, m.flow_id, m.ts, m.direction, m.opcode, m.length,
                    SUBSTR(m.data, 1, ?{cap_n}), LENGTH(m.data)
             FROM ws_messages m"
        );
        if fts.is_some() {
            sql.push_str(" JOIN ws_fts ON ws_fts.rowid = m.id");
        }
        sql.push_str(&format!(" WHERE m.flow_id = ?{flow_n} AND m.id > ?{after_n}"));
        if let Some(d) = direction {
            params.push(Box::new(d.to_string()));
            sql.push_str(&format!(" AND m.direction = ?{}", params.len()));
        }
        if let Some((cond, _)) = &fts {
            sql.push_str(&format!(" AND ws_fts.text {cond}"));
        }
        params.push(Box::new(limit as i64));
        sql.push_str(&format!(" ORDER BY m.id LIMIT ?{}", params.len()));

        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter().map(|p| &**p)), |r| {
            Ok(WsPageRow { msg: ws_from_row(r)?, stored: r.get::<_, i64>(7)? as u64 })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Flow ids whose WebSocket messages match `needle`. This is the second arm of
    /// [`Store::search`]'s union, split out so "a message matched" has one definition
    /// and a caller can tell a message hit from a request/response hit.
    pub fn ws_search(&self, needle: &str) -> Result<Vec<u64>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let (cond, param) = fts_condition(needle);
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(&format!(
            "SELECT DISTINCT m.flow_id FROM ws_fts JOIN ws_messages m ON m.id = ws_fts.rowid
             WHERE ws_fts.text {cond}"
        ))?;
        let rows = stmt.query_map([&param], |r| r.get::<_, i64>(0))?;
        Ok(rows.map(|r| r.map(|id| id as u64)).collect::<rusqlite::Result<_>>()?)
    }

    /// Project-wide counters, without materialising any row.
    pub fn stats(&self) -> Result<StoreStats> {
        let db = self.db.lock().unwrap();
        let (flows, newest_ts) = db.conn.query_row("SELECT COUNT(*), COALESCE(MAX(ts), 0) FROM flows", [], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
        })?;
        let ws_messages = db.conn.query_row("SELECT COUNT(*) FROM ws_messages", [], |r| Ok(r.get::<_, i64>(0)? as u64))?;
        Ok(StoreStats { flows, ws_messages, newest_ts })
    }

    /// The highest flow id handed out so far, WITHOUT taking one. Lets a caller tell a
    /// flow that is still being written (id already allocated, row not committed yet,
    /// because `record_flow` writes on a background thread) from one that never existed.
    pub fn peek_id(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
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

    /// Replace every stored result for one Intruder tab, in one transaction. A run is
    /// atomic: half of an old run beside half of a new one would be unreadable.
    pub fn intruder_save(&self, tab_id: u64, rows: &[IntruderRow]) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.conn.transaction()?;
        tx.execute("DELETE FROM intruder_results WHERE tab_id = ?1", [tab_id as i64])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO intruder_results
                   (tab_id, n, payloads, status, length, time_ms, error, request_raw, response_raw, request_final, response_original)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for r in rows {
                stmt.execute(params![
                    tab_id as i64,
                    r.n as i64,
                    serde_json::to_string(&r.payloads)?,
                    r.status,
                    r.length as i64,
                    r.time_ms as i64,
                    r.error,
                    r.request_raw,
                    r.response_raw,
                    r.request_final,
                    r.response_original,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every row for a tab, WITHOUT the request/response text: that is what makes
    /// reopening a project with a 5,000-row attack cheap.
    pub fn intruder_results(&self, tab_id: u64) -> Result<Vec<IntruderRow>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.conn.prepare(
            "SELECT n, payloads, status, length, time_ms, error
             FROM intruder_results WHERE tab_id = ?1 ORDER BY n",
        )?;
        let rows = stmt.query_map([tab_id as i64], |r| {
            Ok(IntruderRow {
                n: r.get::<_, i64>(0)? as u64,
                payloads: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
                status: r.get(2)?,
                length: r.get::<_, i64>(3)? as usize,
                time_ms: r.get::<_, i64>(4)? as u64,
                error: r.get(5)?,
                ..Default::default()
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// One row in full, fetched when the operator actually opens it.
    pub fn intruder_result(&self, tab_id: u64, n: u64) -> Result<Option<IntruderRow>> {
        let db = self.db.lock().unwrap();
        Ok(db
            .conn
            .query_row(
                "SELECT n, payloads, status, length, time_ms, error, request_raw, response_raw, request_final, response_original
                 FROM intruder_results WHERE tab_id = ?1 AND n = ?2",
                params![tab_id as i64, n as i64],
                |r| {
                    Ok(IntruderRow {
                        n: r.get::<_, i64>(0)? as u64,
                        payloads: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
                        status: r.get(2)?,
                        length: r.get::<_, i64>(3)? as usize,
                        time_ms: r.get::<_, i64>(4)? as u64,
                        error: r.get(5)?,
                        request_raw: r.get(6)?,
                        response_raw: r.get(7)?,
                        request_final: r.get(8)?,
                        response_original: r.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn clear_intruder_results(&self, tab_id: u64) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .conn
            .execute("DELETE FROM intruder_results WHERE tab_id = ?1", [tab_id as i64])?;
        Ok(())
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

/// Marks a `.ngintip` file as a Ngintip project inside the SQLite header
/// (`PRAGMA application_id`, offset 68). Value spells "CGAT" in ASCII, so
/// `file`/`sqlite3` and other tools can recognise our projects.
pub const APPLICATION_ID: i32 = 0x4347_4154;
/// Project schema version stored in `PRAGMA user_version`.
pub const USER_VERSION: i32 = 1;

fn connect(path: &Path) -> Result<(Connection, u64)> {
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    // Stamp the Ngintip header markers (idempotent, cheap).
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
        // Rebuilt from a row that is already in this project's file.
        gen: 0,
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

    /// A delete larger than one chunk must still remove everything. The chunking exists
    /// because a single transaction over a large History holds every dirty FTS page until
    /// the commit; it must not turn into a delete that only half happens.
    #[test]
    fn delete_spans_more_than_one_chunk() {
        let path = std::env::temp_dir().join(format!("ngintip-chunk-{}.ngintip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let uri: Uri = "http://a.test/x".parse().unwrap();

        let n = Store::DELETE_CHUNK * 2 + 7; // deliberately not a multiple of the chunk
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            let id = store.next_id();
            ids.push(id);
            store
                .push(Flow::request(id, &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new()))
                .unwrap();
        }
        assert_eq!(store.stats().unwrap().flows, n as u64);

        assert_eq!(store.delete(&ids, store.generation()).unwrap(), n, "stopped short of the ids it was given");
        assert_eq!(store.stats().unwrap().flows, 0u64, "a delete crossing chunk boundaries left rows behind");

        // The guarded path: a generation that is not the store's deletes nothing at all.
        let keep = store.next_id();
        store
            .push(Flow::request(keep, &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new()))
            .unwrap();
        assert_eq!(store.delete(&[keep], store.generation() + 1).unwrap(), 0, "deleted rows for a project that is not open");
        assert_eq!(store.stats().unwrap().flows, 1u64, "a stale-generation delete removed rows anyway");
        let _ = std::fs::remove_file(&path);
    }

    /// An attack's results are kept with the project, bodies included, but the list read
    /// must not carry them: that split is what keeps reopening a project with a large run
    /// cheap, and it is the same one `repeater_history` uses.
    #[test]
    fn intruder_results_round_trip_without_loading_every_body() {
        let path = std::env::temp_dir().join(format!("ngintip-intr-{}.ngintip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();

        let rows: Vec<IntruderRow> = (1..=3)
            .map(|n| IntruderRow {
                n,
                payloads: vec![format!("p{n}")],
                status: 200,
                length: 10,
                time_ms: 5,
                error: None,
                request_raw: format!("GET /{n} HTTP/1.1\n\n"),
                response_raw: "HTTP/1.1 200 OK\n\nbody".into(),
                request_final: String::new(),
                response_original: String::new(),
            })
            .collect();
        store.intruder_save(7, &rows).unwrap();

        // The window sends JSON null for a row with no bodies (past KEEP_RESPONSES, or
        // with no extension). That crossed the serde boundary as a type error, and
        // neither the Rust test nor the mocked frontend test could see it.
        let from_window: IntruderRow = serde_json::from_str(
            r#"{"n":9,"payloads":[],"status":200,"length":3,"timeMs":7,"error":null,
                "requestRaw":null,"responseRaw":null,"requestFinal":null,"responseOriginal":null}"#,
        )
        .expect("a row with null bodies must deserialize");
        assert_eq!(from_window.n, 9);
        assert!(from_window.request_raw.is_empty());

        let listed = store.intruder_results(7).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].payloads, vec!["p1".to_string()], "the table columns did not survive");
        assert!(listed.iter().all(|r| r.response_raw.is_empty()), "the list read dragged every response body with it");

        let one = store.intruder_result(7, 2).unwrap().expect("row 2");
        assert_eq!(one.request_raw, "GET /2 HTTP/1.1\n\n");
        assert_eq!(one.response_raw, "HTTP/1.1 200 OK\n\nbody");
        assert!(store.intruder_result(7, 99).unwrap().is_none());

        // A new run replaces the old one rather than interleaving with it.
        store.intruder_save(7, &rows[..1]).unwrap();
        assert_eq!(store.intruder_results(7).unwrap().len(), 1, "the previous run was left behind");

        // Another tab is untouched, and clearing one tab clears only that tab.
        store.intruder_save(8, &rows).unwrap();
        store.clear_intruder_results(7).unwrap();
        assert!(store.intruder_results(7).unwrap().is_empty());
        assert_eq!(store.intruder_results(8).unwrap().len(), 3, "clearing one tab took another with it");
        let _ = std::fs::remove_file(&path);
    }

    /// Clear drops and recreates the FTS tables, so the thing to prove is that search
    /// still works afterwards: a dropped index that is never rebuilt would leave Clear
    /// looking fine and every later search silently returning nothing.
    #[test]
    fn search_still_works_after_clearing_history() {
        let path = std::env::temp_dir().join(format!("ngintip-clear-{}.ngintip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let uri: Uri = "http://a.test/x".parse().unwrap();

        let mut before = Flow::request(store.next_id(), &Method::POST, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::from_static(b"needle-before"));
        before.set_response(StatusCode::OK, Version::HTTP_11, &HeaderMap::new(), Bytes::new());
        store.push(before).unwrap();
        assert_eq!(store.search("needle-before").unwrap().len(), 1);

        store.clear(store.generation()).unwrap();
        assert_eq!(store.stats().unwrap().flows, 0u64);
        assert!(store.search("needle-before").unwrap().is_empty(), "cleared rows still match");
        assert!(SCHEMA.contains(FTS_SCHEMA.trim()), "clear() recreates tables SCHEMA no longer defines");

        let id = store.next_id();
        let mut after = Flow::request(id, &Method::POST, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::from_static(b"needle-after"));
        after.set_response(StatusCode::OK, Version::HTTP_11, &HeaderMap::new(), Bytes::new());
        store.push(after).unwrap();
        assert_eq!(store.search("needle-after").unwrap(), vec![id], "the index was not usable after Clear");
        let _ = std::fs::remove_file(&path);
    }

    /// A read-modify-write caller (the Organizer, the MCP setting writers) loads a value,
    /// works on it, then writes the whole thing back. If the project changed in between,
    /// that write must not land: it would replace the newly opened project's value with
    /// the previous project's in full.
    #[test]
    fn a_setting_write_for_a_closed_project_is_refused() {
        let a = std::env::temp_dir().join(format!("ngintip-seta-{}.ngintip", std::process::id()));
        let b = std::env::temp_dir().join(format!("ngintip-setb-{}.ngintip", std::process::id()));
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
        let store = Store::open(&a).unwrap();
        let gen = store.generation();
        store.put_setting("organizer", &serde_json::json!({ "client": "A" })).unwrap();

        // Still project A: the write lands.
        assert!(store.put_setting_at("organizer", &serde_json::json!({ "client": "A2" }), gen).unwrap());

        store.switch(&b).unwrap();
        store.put_setting("organizer", &serde_json::json!({ "client": "B" })).unwrap();

        // Project A's in-flight write must not reach project B.
        assert!(!store.put_setting_at("organizer", &serde_json::json!({ "client": "A3" }), gen).unwrap());
        assert_eq!(
            store.get_setting("organizer").unwrap().unwrap(),
            serde_json::json!({ "client": "B" }),
            "one project's Organizer replaced another's"
        );
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Capture is asynchronous, so a flow queued under one project can still be waiting
    /// when another is opened. The generation is what lets the writer tell them apart.
    #[test]
    fn switching_project_bumps_the_generation() {
        let a = std::env::temp_dir().join(format!("ngintip-gen-a-{}.ngintip", std::process::id()));
        let b = std::env::temp_dir().join(format!("ngintip-gen-b-{}.ngintip", std::process::id()));
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
        let store = Store::open(&a).unwrap();
        let before = store.generation();
        store.switch(&b).unwrap();
        assert_ne!(before, store.generation(), "opening another project left the generation unchanged");
        // Ordinary work must NOT move it: only opening another project does.
        let after = store.generation();
        let uri: Uri = "http://a.test/x".parse().unwrap();
        store
            .push(Flow::request(store.next_id(), &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new()))
            .unwrap();
        store.put_setting("k", &serde_json::json!(1)).unwrap();
        assert_eq!(after, store.generation(), "capturing a flow moved the generation");
        for p in [&a, &b] {
            let _ = std::fs::remove_file(p);
        }
    }

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
        let path = std::env::temp_dir().join(format!("ngintip-logs-{}.ngintip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            store.push_log(1000, "info", "proxy", "listening on 127.0.0.1:8080").unwrap();
            store.push_log(2000, "error", "tls", "handshake failed").unwrap();
            let logs = store.recent_logs(10).unwrap();
            assert_eq!(logs.len(), 2);
            assert_eq!((logs[0].level.as_str(), logs[1].level.as_str()), ("info", "error")); // oldest first
        }
        // Reopening the same file still has the logs (they live in the .ngintip).
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
    fn snapshot_is_a_valid_stamped_ngintip_file() {
        let path = std::env::temp_dir().join(format!("ngintip-snap-src-{}.ngintip", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        let uri: Uri = "https://example.com/x".parse().unwrap();
        let flow = Flow::request(store.next_id(), &Method::POST, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::from_static(b"role=admin"));
        store.push(flow).unwrap();

        // The live file carries the Ngintip header markers.
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
        let dest = std::env::temp_dir().join(format!("ngintip-snap-dst-{}.ngintip", std::process::id()));
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

        store.delete(&[a], store.generation()).unwrap();
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
