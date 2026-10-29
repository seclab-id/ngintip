//! Organizer: named collections of saved requests (from History, Repeater, Intruder or
//! the MCP server), stored per project as JSON under the `organizer` setting.
//!
//! The backend owns this data. The UI and the MCP server both change it through
//! [`apply`], which serializes edits, bumps a revision and saves, so neither side can
//! overwrite the other's changes. The UI shows the state an edit returns; edits made
//! elsewhere (MCP) emit [`EVENT`] with the new revision so an open Organizer refreshes.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    mcp::HIGHLIGHT_COLORS,
    store::{self, Store},
};

/// Project setting key holding the organizer JSON.
pub const SETTING: &str = "organizer";
/// UI event `{ rev }`, sent after a change the UI did not make itself.
pub const EVENT: &str = "organizer";

/// Serializes load-edit-save cycles so two edits never interleave.
static LOCK: Mutex<()> = Mutex::new(());
static SEQ: AtomicU64 = AtomicU64::new(0);

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}-{}", store::now_ms(), SEQ.fetch_add(1, Ordering::Relaxed))
}

/// One saved request: what was sent and received, plus the operator's label, note and
/// highlight colour.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedRequest {
    pub id: String,
    /// Default title, also the Repeater / Intruder tab name when replayed.
    pub name: String,
    pub method: String,
    pub host: String,
    pub path: String,
    /// `scheme://host[:port]` the request is sent to.
    pub target: String,
    pub request: String,
    pub response: String,
    pub status: Option<u16>,
    pub label: String,
    pub note: String,
    pub color: String,
    /// Where it came from: history, repeater, intruder or mcp.
    pub source: String,
    pub saved_at: u64,
    /// The first version stored the request text here; folded into `request` on load.
    #[serde(skip_serializing)]
    raw: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub items: Vec<SavedRequest>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Organizer {
    /// Bumped by every edit, so the UI can ignore a state older than the one it shows.
    pub rev: u64,
    pub collections: Vec<Collection>,
    /// Where "Save to Organizer" lands; None = no active collection.
    pub active_id: Option<String>,
    /// Older projects kept the Organizer notes here; the UI moves them to their own
    /// setting. Kept untouched so that migration can still find them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Value>,
}

/// What a save provides; id, method, host, path and time are derived.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewItem {
    pub name: String,
    pub target: String,
    pub request: String,
    pub response: String,
    pub status: Option<u16>,
    pub label: String,
    pub note: String,
    pub color: String,
    pub source: String,
}

/// An edit, as sent by the UI (`{ "op": "save", ... }`) or built by the MCP server.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    /// A new collection; it becomes active when `activate`, or when none is active.
    #[serde(rename_all = "camelCase")]
    CreateCollection {
        name: String,
        #[serde(default)]
        activate: bool,
    },
    #[serde(rename_all = "camelCase")]
    RenameCollection { id: String, name: String },
    #[serde(rename_all = "camelCase")]
    DeleteCollection { id: String },
    /// None = no active collection.
    #[serde(rename_all = "camelCase")]
    SetActive {
        #[serde(default)]
        id: Option<String>,
    },
    /// Save into `collection` (an id or a name; an unknown name creates it), else the
    /// active collection, else "Saved" (created if missing), which then becomes active.
    #[serde(rename_all = "camelCase")]
    Save {
        #[serde(default)]
        collection: Option<String>,
        item: NewItem,
    },
    /// Change a saved request's label / note / colour; None leaves that field as is.
    #[serde(rename_all = "camelCase")]
    Update {
        item_id: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Remove { item_id: String },
    #[serde(rename_all = "camelCase")]
    Move { item_id: String, to: String },
}

/// The outcome of [`apply`]: the new state, plus the collection / item the edit made or
/// touched (when there is one).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub state: Organizer,
    pub collection_id: Option<String>,
    pub item_id: Option<String>,
}

/// The project's organizer. A missing setting is an empty organizer; data that can't be
/// read is an error, so an edit never overwrites what it didn't understand.
pub fn load(store: &Store) -> Result<Organizer, String> {
    let value = store.get_setting(SETTING).map_err(|e| e.to_string())?;
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Organizer::default());
    };
    let mut org: Organizer = serde_json::from_value(value).map_err(|e| format!("organizer data is unreadable: {e}"))?;
    for item in org.collections.iter_mut().flat_map(|c| c.items.iter_mut()) {
        if item.request.is_empty() {
            if let Some(raw) = item.raw.take() {
                item.request = raw;
            }
        }
    }
    Ok(org)
}

/// Apply one edit: load, change, bump the revision, save.
pub fn apply(store: &Store, op: Op) -> Result<Applied, String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut org = load(store)?;
    let (collection_id, item_id) = org.edit(op)?;
    org.rev += 1;
    let value = serde_json::to_value(&org).map_err(|e| e.to_string())?;
    store.put_setting(SETTING, &value).map_err(|e| e.to_string())?;
    Ok(Applied { state: org, collection_id, item_id })
}

/// An empty colour clears it; anything else must be a highlight colour.
fn check_color(color: &str) -> Result<(), String> {
    let c = color.trim();
    if c.is_empty() || HIGHLIGHT_COLORS.contains(&c) {
        Ok(())
    } else {
        Err(format!("invalid color '{c}'; use one of: {} (or \"\" to clear)", HIGHLIGHT_COLORS.join(", ")))
    }
}

/// Method and request-target from a raw request's first line (GET / when absent).
fn request_line(raw: &str) -> (String, String) {
    let mut parts = raw.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    (method, path)
}

/// `host[:port]` of a target like `https://user@host:8443/x`.
fn host_of(target: &str) -> String {
    let t = target.trim();
    let rest = t.split_once("://").map_or(t, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit_once('@').map_or(authority, |(_, h)| h).to_string()
}

impl SavedRequest {
    fn from_new(n: NewItem) -> Self {
        let (method, path) = request_line(&n.request);
        let host = host_of(&n.target);
        let name = match n.name.trim() {
            "" => format!("{method} {host}{path}"),
            name => name.to_string(),
        };
        Self {
            id: new_id("req"),
            name,
            method,
            host,
            path,
            target: n.target.trim().to_string(),
            request: n.request,
            response: n.response,
            status: n.status,
            label: n.label.trim().to_string(),
            note: n.note,
            color: n.color.trim().to_string(),
            source: n.source,
            saved_at: store::now_ms(),
            raw: None,
        }
    }
}

impl Organizer {
    fn has(&self, id: &str) -> bool {
        self.collections.iter().any(|c| c.id == id)
    }

    fn active_valid(&self) -> bool {
        self.active_id.as_deref().is_some_and(|id| self.has(id))
    }

    /// (collection index, item index) of a saved request; ids are unique project-wide.
    fn find_item(&self, item_id: &str) -> Option<(usize, usize)> {
        self.collections
            .iter()
            .enumerate()
            .find_map(|(ci, c)| c.items.iter().position(|it| it.id == item_id).map(|ii| (ci, ii)))
    }

    fn create(&mut self, name: &str, activate: bool) -> String {
        let id = new_id("col");
        let name = match name.trim() {
            "" => "Collection",
            n => n,
        };
        self.collections.push(Collection { id: id.clone(), name: name.to_string(), items: Vec::new() });
        if activate || !self.active_valid() {
            self.active_id = Some(id.clone());
        }
        id
    }

    /// The collection a save goes into (see [`Op::Save`]).
    fn target(&mut self, wanted: Option<&str>) -> String {
        if let Some(w) = wanted.map(str::trim).filter(|w| !w.is_empty()) {
            if let Some(c) = self.collections.iter().find(|c| c.id == w || c.name.eq_ignore_ascii_case(w)) {
                return c.id.clone();
            }
            return self.create(w, false);
        }
        if let Some(id) = self.active_id.clone().filter(|id| self.has(id)) {
            return id;
        }
        let id = match self.collections.iter().find(|c| c.name == "Saved") {
            Some(c) => c.id.clone(),
            None => self.create("Saved", false),
        };
        self.active_id = Some(id.clone());
        id
    }

    /// Apply `op` in memory; returns the (collection, item) it made or touched.
    pub fn edit(&mut self, op: Op) -> Result<(Option<String>, Option<String>), String> {
        match op {
            Op::CreateCollection { name, activate } => Ok((Some(self.create(&name, activate)), None)),
            Op::RenameCollection { id, name } => {
                let c = self.collections.iter_mut().find(|c| c.id == id).ok_or_else(|| format!("no collection {id}"))?;
                if !name.trim().is_empty() {
                    c.name = name.trim().to_string();
                }
                Ok((Some(id), None))
            }
            Op::DeleteCollection { id } => {
                let before = self.collections.len();
                self.collections.retain(|c| c.id != id);
                if self.collections.len() == before {
                    return Err(format!("no collection {id}"));
                }
                if self.active_id.as_deref() == Some(id.as_str()) {
                    self.active_id = None;
                }
                Ok((None, None))
            }
            Op::SetActive { id } => {
                if let Some(id) = &id {
                    if !self.has(id) {
                        return Err(format!("no collection {id}"));
                    }
                }
                self.active_id = id.clone();
                Ok((id, None))
            }
            Op::Save { collection, item } => {
                check_color(&item.color)?;
                let col = self.target(collection.as_deref());
                let saved = SavedRequest::from_new(item);
                let item_id = saved.id.clone();
                self.collections
                    .iter_mut()
                    .find(|c| c.id == col)
                    .ok_or("the target collection disappeared")?
                    .items
                    .push(saved);
                Ok((Some(col), Some(item_id)))
            }
            Op::Update { item_id, label, note, color } => {
                if let Some(c) = &color {
                    check_color(c)?;
                }
                let (ci, ii) = self.find_item(&item_id).ok_or_else(|| format!("no saved request {item_id}"))?;
                let it = &mut self.collections[ci].items[ii];
                if let Some(l) = label {
                    it.label = l.trim().to_string();
                }
                if let Some(n) = note {
                    it.note = n;
                }
                if let Some(c) = color {
                    it.color = c.trim().to_string();
                }
                Ok((Some(self.collections[ci].id.clone()), Some(item_id)))
            }
            Op::Remove { item_id } => {
                let (ci, ii) = self.find_item(&item_id).ok_or_else(|| format!("no saved request {item_id}"))?;
                self.collections[ci].items.remove(ii);
                Ok((Some(self.collections[ci].id.clone()), None))
            }
            Op::Move { item_id, to } => {
                let ti = self.collections.iter().position(|c| c.id == to).ok_or_else(|| format!("no collection {to}"))?;
                let (ci, ii) = self.find_item(&item_id).ok_or_else(|| format!("no saved request {item_id}"))?;
                if ci != ti {
                    let it = self.collections[ci].items.remove(ii);
                    self.collections[ti].items.push(it);
                }
                Ok((Some(to), Some(item_id)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(request: &str, target: &str) -> NewItem {
        NewItem { request: request.into(), target: target.into(), ..Default::default() }
    }
    fn save(org: &mut Organizer, collection: Option<&str>, it: NewItem) -> (String, String) {
        let (c, i) = org.edit(Op::Save { collection: collection.map(str::to_string), item: it }).unwrap();
        (c.unwrap(), i.unwrap())
    }

    #[test]
    fn collections_and_the_active_target() {
        let mut org = Organizer::default();
        // The first collection becomes active; a second one does not steal it.
        let a = org.edit(Op::CreateCollection { name: " Targets ".into(), activate: false }).unwrap().0.unwrap();
        let b = org.edit(Op::CreateCollection { name: "".into(), activate: false }).unwrap().0.unwrap();
        assert_eq!(org.active_id.as_deref(), Some(a.as_str()));
        assert_eq!((org.collections[0].name.as_str(), org.collections[1].name.as_str()), ("Targets", "Collection"));
        // Explicit activation, rename (blank keeps the name), unknown ids error.
        org.edit(Op::SetActive { id: Some(b.clone()) }).unwrap();
        assert_eq!(org.active_id.as_deref(), Some(b.as_str()));
        org.edit(Op::RenameCollection { id: b.clone(), name: "  ".into() }).unwrap();
        assert_eq!(org.collections[1].name, "Collection");
        org.edit(Op::RenameCollection { id: b.clone(), name: "IDOR".into() }).unwrap();
        assert_eq!(org.collections[1].name, "IDOR");
        assert!(org.edit(Op::SetActive { id: Some("nope".into()) }).is_err());
        assert!(org.edit(Op::RenameCollection { id: "nope".into(), name: "x".into() }).is_err());
        // Deleting the active collection leaves none active.
        org.edit(Op::DeleteCollection { id: b }).unwrap();
        assert_eq!(org.active_id, None);
        assert!(org.edit(Op::DeleteCollection { id: "nope".into() }).is_err());
    }

    #[test]
    fn save_targets_active_then_saved_and_named_collections() {
        let mut org = Organizer::default();
        // Nothing active: a "Saved" collection is created and becomes active.
        let (c1, i1) = save(&mut org, None, item("POST /login HTTP/1.1\r\nHost: a.test\r\n\r\nu=1", "https://user@a.test:8443"));
        assert_eq!(org.collections[0].name, "Saved");
        assert_eq!(org.active_id.as_deref(), Some(c1.as_str()));
        let it = &org.collections[0].items[0];
        assert_eq!((it.id.as_str(), it.method.as_str(), it.path.as_str(), it.host.as_str()), (i1.as_str(), "POST", "/login", "a.test:8443"));
        assert_eq!(it.name, "POST a.test:8443/login");
        // The next save without a collection goes to the active one.
        let (c2, _) = save(&mut org, None, item("", "https://b.test"));
        assert_eq!(c2, c1);
        assert_eq!(org.collections[0].items[1].method, "GET");
        // By name (case-insensitive) or id; an unknown name creates that collection
        // without taking over the active one.
        let (c3, _) = save(&mut org, Some("saved"), item("GET /x HTTP/1.1", "http://c.test"));
        assert_eq!(c3, c1);
        let (c4, _) = save(&mut org, Some("Findings"), item("GET /y HTTP/1.1", "http://c.test"));
        assert_ne!(c4, c1);
        assert_eq!(org.collections[1].name, "Findings");
        assert_eq!(org.active_id.as_deref(), Some(c1.as_str()));
        // An invalid colour is rejected before anything changes.
        let bad = NewItem { color: "chartreuse".into(), ..item("GET / HTTP/1.1", "http://x") };
        assert!(org.edit(Op::Save { collection: None, item: bad }).is_err());
        assert_eq!(org.collections.iter().map(|c| c.items.len()).sum::<usize>(), 4);
    }

    #[test]
    fn update_move_and_remove_items() {
        let mut org = Organizer::default();
        let (a, it) = save(&mut org, Some("A"), item("GET /a HTTP/1.1", "https://a.test"));
        let b = org.edit(Op::CreateCollection { name: "B".into(), activate: false }).unwrap().0.unwrap();
        org.edit(Op::Update { item_id: it.clone(), label: Some(" IDOR ".into()), note: Some("creds in body".into()), color: Some("red".into()) }).unwrap();
        let s = &org.collections[0].items[0];
        assert_eq!((s.label.as_str(), s.note.as_str(), s.color.as_str()), ("IDOR", "creds in body", "red"));
        // None leaves a field as is; "" clears the colour; a bad colour errors.
        org.edit(Op::Update { item_id: it.clone(), label: None, note: None, color: Some("".into()) }).unwrap();
        assert_eq!((org.collections[0].items[0].label.as_str(), org.collections[0].items[0].color.as_str()), ("IDOR", ""));
        assert!(org.edit(Op::Update { item_id: it.clone(), label: None, note: None, color: Some("teal".into()) }).is_err());
        assert!(org.edit(Op::Update { item_id: "nope".into(), label: None, note: None, color: None }).is_err());
        // Move keeps the item (and its edits); moving into its own collection is a no-op.
        org.edit(Op::Move { item_id: it.clone(), to: b.clone() }).unwrap();
        assert!(org.collections[0].items.is_empty());
        assert_eq!(org.collections[1].items[0].label, "IDOR");
        org.edit(Op::Move { item_id: it.clone(), to: b.clone() }).unwrap();
        assert_eq!(org.collections[1].items.len(), 1);
        assert!(org.edit(Op::Move { item_id: it.clone(), to: a.clone() + "x" }).is_err());
        org.edit(Op::Remove { item_id: it.clone() }).unwrap();
        assert!(org.collections[1].items.is_empty());
        assert!(org.edit(Op::Remove { item_id: it }).is_err());
    }

    #[test]
    fn ops_deserialize_from_the_ui_shape() {
        let op: Op = serde_json::from_value(json!({ "op": "update", "itemId": "req-1", "color": "blue" })).unwrap();
        assert!(matches!(op, Op::Update { ref item_id, label: None, note: None, color: Some(ref c) } if item_id == "req-1" && c == "blue"));
        let op: Op = serde_json::from_value(json!({ "op": "setActive", "id": null })).unwrap();
        assert!(matches!(op, Op::SetActive { id: None }));
        let op: Op = serde_json::from_value(json!({ "op": "save", "item": { "request": "GET / HTTP/1.1", "target": "https://x", "status": 200 } })).unwrap();
        assert!(matches!(op, Op::Save { collection: None, item } if item.status == Some(200)));
    }

    #[test]
    fn persists_with_a_revision_and_reads_older_data() {
        let dir = crate::e2e::temp_dir("organizer");
        let store = Store::open(&dir.join("p.intip")).unwrap();
        // Empty project: an empty organizer at revision 0.
        assert_eq!(load(&store).unwrap(), Organizer::default());
        // Data written by the first UI version: `raw` instead of `request`, a `viewId`
        // the backend no longer keeps, and notes that must survive for their migration.
        store
            .put_setting(SETTING, &json!({
                "collections": [{ "id": "c1", "name": "Old", "items": [{ "id": "r1", "method": "GET", "raw": "GET /old HTTP/1.1", "status": null }] }],
                "activeId": "c1",
                "viewId": "c1",
                "notes": [{ "id": "n1", "title": "keep me" }],
            }))
            .unwrap();
        let org = load(&store).unwrap();
        assert_eq!(org.collections[0].items[0].request, "GET /old HTTP/1.1");
        let applied = apply(&store, Op::CreateCollection { name: "New".into(), activate: false }).unwrap();
        assert_eq!(applied.state.rev, 1);
        let saved = store.get_setting(SETTING).unwrap().unwrap();
        assert_eq!(saved["notes"][0]["title"], "keep me", "legacy notes kept for the UI migration");
        assert_eq!(saved["collections"][0]["items"][0]["request"], "GET /old HTTP/1.1");
        assert!(saved["collections"][0]["items"][0].get("raw").is_none());
        assert!(saved.get("viewId").is_none());
        assert_eq!(apply(&store, Op::SetActive { id: None }).unwrap().state.rev, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_data_is_never_overwritten() {
        let dir = crate::e2e::temp_dir("organizer-bad");
        let store = Store::open(&dir.join("p.intip")).unwrap();
        let junk = json!({ "collections": "not a list" });
        store.put_setting(SETTING, &junk).unwrap();
        assert!(load(&store).is_err());
        assert!(apply(&store, Op::CreateCollection { name: "x".into(), activate: false }).is_err());
        assert_eq!(store.get_setting(SETTING).unwrap().unwrap(), junk, "left exactly as it was");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
