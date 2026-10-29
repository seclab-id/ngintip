//! Read-only view of the project Vault (Settings > Vault) for the MCP server. The UI owns
//! and writes it (project setting `vault`); this only reads it, and understands both the
//! current shape (a list of typed fields per entry) and the first one (fixed username /
//! password / url).

use serde::Serialize;
use serde_json::Value;

use crate::store::Store;

/// Project setting key holding the vault JSON.
pub const SETTING: &str = "vault";

/// What a listing shows in place of a secret value.
pub const HIDDEN: &str = "[hidden, use vault_get]";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    /// username, email, password, apikey, token, url or text.
    pub kind: String,
    pub secret: bool,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub role: String,
    pub note: String,
    pub fields: Vec<Field>,
}

/// Every vault entry of the open project (none when the vault is empty).
pub fn entries(store: &Store) -> Result<Vec<Entry>, String> {
    let value = store.get_setting(SETTING).map_err(|e| e.to_string())?.unwrap_or(Value::Null);
    Ok(parse(&value))
}

pub fn parse(value: &Value) -> Vec<Entry> {
    value.get("entries").and_then(Value::as_array).map(|list| list.iter().map(entry).collect()).unwrap_or_default()
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn entry(e: &Value) -> Entry {
    let fields = match e.get("fields").and_then(Value::as_array) {
        Some(fields) => fields
            .iter()
            .map(|f| Field {
                name: text(f, "name"),
                kind: text(f, "kind"),
                secret: f.get("secret").and_then(Value::as_bool).unwrap_or(false),
                value: text(f, "value"),
            })
            .collect(),
        // The first shape: fixed username / password / url, kept only when set.
        None => [("username", "Username", false), ("password", "Password", true), ("url", "URL", false)]
            .into_iter()
            .filter_map(|(kind, name, secret)| {
                let value = text(e, kind);
                (!value.is_empty()).then(|| Field { name: name.into(), kind: kind.into(), secret, value })
            })
            .collect(),
    };
    Entry { id: text(e, "id"), label: text(e, "label"), role: text(e, "role"), note: text(e, "note"), fields }
}

impl Entry {
    /// The same entry with every non-empty secret value replaced by [`HIDDEN`].
    pub fn redacted(&self) -> Entry {
        let mut e = self.clone();
        for f in e.fields.iter_mut().filter(|f| f.secret && !f.value.is_empty()) {
            f.value = HIDDEN.into();
        }
        e
    }

    /// Case-insensitive match on the label, role, note, field names and non-secret
    /// values; a secret value never matches.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        let hit = |s: &str| s.to_lowercase().contains(&q);
        hit(&self.label)
            || hit(&self.role)
            || hit(&self.note)
            || self.fields.iter().any(|f| hit(&f.name) || (!f.secret && hit(&f.value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({ "entries": [
            { "id": "v1", "label": "Admin staging", "role": "admin", "note": "from leak", "fields": [
                { "id": "f1", "kind": "email", "name": "Email", "secret": false, "value": "admin@t.test" },
                { "id": "f2", "kind": "password", "name": "Password", "secret": true, "value": "hunter2" },
                { "id": "f3", "kind": "apikey", "name": "API key", "secret": true, "value": "" },
            ]},
            // The first, flat shape.
            { "id": "v2", "label": "Old", "username": "root", "password": "toor", "url": "" },
        ]})
    }

    #[test]
    fn reads_both_shapes() {
        let entries = parse(&sample());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].fields.len(), 3);
        assert_eq!(entries[0].fields[1], Field { name: "Password".into(), kind: "password".into(), secret: true, value: "hunter2".into() });
        // Flat entry: only the set fields, the password marked secret.
        let old = &entries[1];
        assert_eq!(old.fields.iter().map(|f| f.kind.as_str()).collect::<Vec<_>>(), ["username", "password"]);
        assert!(old.fields[1].secret);
        assert!(parse(&Value::Null).is_empty());
    }

    #[test]
    fn listings_hide_secrets_and_search_skips_them() {
        let entries = parse(&sample());
        let r = entries[0].redacted();
        assert_eq!(r.fields[0].value, "admin@t.test");
        assert_eq!(r.fields[1].value, HIDDEN);
        assert_eq!(r.fields[2].value, "", "an empty secret stays empty");
        assert!(entries[0].matches("ADMIN@"));
        assert!(entries[0].matches("leak"));
        assert!(entries[0].matches("api key"), "field names match");
        assert!(!entries[0].matches("hunter2"), "secret values never match");
        assert!(!entries[1].matches("toor"));
    }
}
