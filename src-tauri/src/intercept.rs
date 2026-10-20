//! Intercept queue: while enabled, each request (and optionally its response)
//! is held until the UI forwards it (optionally edited) or drops it.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::{raw, store::Flow};

/// Event name the UI listens on for each newly held message.
pub const INTERCEPT_EVENT: &str = "intercept";

/// One advanced-filter rule: hold (include) or skip (exclude) messages whose
/// `field` contains `value`. Mirrors the History filters the UI already uses.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterRule {
    /// "host" | "path" | "url" | "method"
    pub field: String,
    /// "include" | "exclude"
    pub mode: String,
    pub value: String,
    #[serde(default)]
    pub sensitive: bool,
}

/// A set of intercept filter rules. Empty = hold everything (default behaviour).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct InterceptFilter {
    pub rules: Vec<FilterRule>,
}

impl InterceptFilter {
    /// Whether a message with these fields should be held. Precedence: any matching
    /// exclude wins (skip it); otherwise, if there are include rules it must match
    /// one; with no rules everything is held.
    pub fn allows(&self, method: &str, host: &str, path: &str, url: &str) -> bool {
        if self.rules.is_empty() {
            return true; // no filter: hold everything (common case, no allocation)
        }
        let field = |f: &str| match f {
            "host" => host,
            "path" => path,
            "method" => method,
            _ => url,
        };
        let matches = |r: &FilterRule| {
            if r.value.is_empty() {
                return false;
            }
            let hay = field(&r.field);
            if r.sensitive {
                hay.contains(&r.value)
            } else {
                hay.to_lowercase().contains(&r.value.to_lowercase())
            }
        };
        let active: Vec<&FilterRule> = self.rules.iter().filter(|r| !r.value.is_empty()).collect();
        if active.iter().any(|r| r.mode == "exclude" && matches(r)) {
            return false;
        }
        let mut includes = active.iter().filter(|r| r.mode == "include").peekable();
        if includes.peek().is_none() {
            return true;
        }
        includes.any(|r| matches(r))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Request,
    Response,
}

pub enum Decision {
    /// Send on, as-is (`raw: None`) or as edited. For a request,
    /// `hold_response` also holds the response that comes back for it.
    Forward { raw: Option<String>, hold_response: bool },
    Drop,
}

impl Decision {
    pub const UNCHANGED: Decision = Decision::Forward {
        raw: None,
        hold_response: false,
    };
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingItem {
    /// Unique per held message; a request and its response share `id`.
    pub key: u64,
    /// Flow id in History.
    pub id: u64,
    /// When the message was captured (unix millis), same as its History row.
    pub ts: u64,
    pub phase: Phase,
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub path: String,
    /// Status code of a held response.
    pub status: Option<u16>,
    /// What the editor shows and edits. When an interceptor-scope extension
    /// transformed the message, this is the transformed ("final") text.
    pub raw: String,
    /// The message as it arrived, kept only when an extension changed `raw` above,
    /// so the UI can offer an Original view alongside the Final one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
}

struct Pending {
    item: PendingItem,
    tx: oneshot::Sender<Decision>,
}

#[derive(Default)]
pub struct Intercept {
    enabled: AtomicBool,
    /// Hold every in-scope response too, not just the ones asked for.
    responses: AtomicBool,
    next_key: AtomicU64,
    queue: Mutex<Vec<Pending>>,
    /// Advanced filter: which in-scope messages actually get held.
    filter: Mutex<InterceptFilter>,
}

impl Intercept {
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Replace the advanced filter rules.
    pub fn set_filter(&self, filter: InterceptFilter) {
        *self.filter.lock().unwrap() = filter;
    }

    /// Whether the filter would hold a message with these fields (ignores on/off).
    pub fn allows(&self, method: &str, host: &str, path: &str, url: &str) -> bool {
        self.filter.lock().unwrap().allows(method, host, path, url)
    }

    /// Turning intercept off releases everything still held, unchanged.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if !on {
            for p in self.queue.lock().unwrap().drain(..) {
                let _ = p.tx.send(Decision::UNCHANGED);
            }
        }
    }

    pub fn holds_responses(&self) -> bool {
        self.responses.load(Ordering::Relaxed)
    }

    pub fn set_responses(&self, on: bool) {
        self.responses.store(on, Ordering::Relaxed);
    }

    pub fn pending(&self) -> Vec<PendingItem> {
        self.queue
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.item.clone())
            .collect()
    }

    /// Returns false if `key` is no longer held.
    pub fn resolve(&self, key: u64, decision: Decision) -> bool {
        let mut queue = self.queue.lock().unwrap();
        let Some(i) = queue.iter().position(|p| p.item.key == key) else {
            return false;
        };
        let _ = queue.remove(i).tx.send(decision);
        true
    }

    /// Park the message until the user decides. `notify` announces it to the UI.
    /// `transformed` is an interceptor-scope extension's output: when it differs from
    /// what arrived, the editor shows it as the "final" message and keeps the arrived
    /// text as `original`.
    pub async fn hold(
        &self,
        phase: Phase,
        flow: &Flow,
        transformed: Option<String>,
        notify: impl FnOnce(&PendingItem),
    ) -> Decision {
        let (tx, rx) = oneshot::channel();
        let s = &flow.summary;
        let arrived = match phase {
            Phase::Request => raw::request_text(flow),
            Phase::Response => raw::response_text(flow),
        };
        let (raw, original) = match transformed {
            Some(t) if t != arrived => (t, Some(arrived)),
            _ => (arrived, None),
        };
        let item = PendingItem {
            key: self.next_key.fetch_add(1, Ordering::Relaxed) + 1,
            id: s.id,
            ts: s.ts,
            phase,
            method: s.method.clone(),
            scheme: s.scheme.clone(),
            host: s.host.clone(),
            path: s.path.clone(),
            status: (phase == Phase::Response).then_some(s.status),
            raw,
            original,
        };
        self.queue.lock().unwrap().push(Pending {
            item: item.clone(),
            tx,
        });
        // Intercept may have been switched off between the caller's check and the push.
        if !self.is_enabled() {
            self.resolve(item.key, Decision::UNCHANGED);
        } else {
            notify(&item);
        }
        rx.await.unwrap_or(Decision::UNCHANGED)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use http::{HeaderMap, Method, StatusCode, Uri, Version};

    use super::*;

    fn flow(id: u64) -> Flow {
        let uri: Uri = "https://example.com/a".parse().unwrap();
        Flow::request(id, &Method::GET, &uri, Version::HTTP_11, &HeaderMap::new(), Bytes::new())
    }

    async fn wait_held(ic: &Intercept) -> PendingItem {
        loop {
            if let Some(item) = ic.pending().pop() {
                return item;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn forward_edit_and_drop() {
        let ic = Arc::new(Intercept::default());
        ic.set_enabled(true);

        let held = tokio::spawn({
            let ic = ic.clone();
            async move { ic.hold(Phase::Request, &flow(1), None, |_| {}).await }
        });
        let item = wait_held(&ic).await;
        assert!(item.raw.starts_with("GET /a HTTP/1.1"));
        assert_eq!((item.id, item.phase, item.status), (1, Phase::Request, None));
        let edit = Decision::Forward { raw: Some("edited".into()), hold_response: true };
        assert!(ic.resolve(item.key, edit));
        assert!(matches!(
            held.await.unwrap(),
            Decision::Forward { raw: Some(r), hold_response: true } if r == "edited"
        ));

        let mut res = flow(2);
        res.set_response(StatusCode::NOT_FOUND, Version::HTTP_11, &HeaderMap::new(), Bytes::from_static(b"gone"));
        let held = tokio::spawn({
            let ic = ic.clone();
            async move { ic.hold(Phase::Response, &res, None, |_| {}).await }
        });
        let item = wait_held(&ic).await;
        assert_eq!((item.phase, item.status), (Phase::Response, Some(404)));
        assert!(item.raw.starts_with("HTTP/1.1 404"), "{}", item.raw);
        assert!(ic.resolve(item.key, Decision::Drop));
        assert!(matches!(held.await.unwrap(), Decision::Drop));
        assert!(!ic.resolve(item.key, Decision::Drop), "already resolved");
    }

    #[test]
    fn filter_include_exclude_precedence() {
        let rule = |field: &str, mode: &str, value: &str| FilterRule {
            field: field.into(),
            mode: mode.into(),
            value: value.into(),
            sensitive: false,
        };
        // No rules: hold everything.
        let f = InterceptFilter::default();
        assert!(f.allows("GET", "a.com", "/x", "http://a.com/x"));
        // Include: only matching hosts are held.
        let f = InterceptFilter { rules: vec![rule("host", "include", "api.")] };
        assert!(f.allows("GET", "api.a.com", "/x", "http://api.a.com/x"));
        assert!(!f.allows("GET", "cdn.a.com", "/x", "http://cdn.a.com/x"));
        // Exclude wins even when an include also matches.
        let f = InterceptFilter {
            rules: vec![rule("host", "include", "a.com"), rule("path", "exclude", "/static/")],
        };
        assert!(f.allows("GET", "a.com", "/api/v1", "http://a.com/api/v1"));
        assert!(!f.allows("GET", "a.com", "/static/app.js", "http://a.com/static/app.js"));
        // Case-sensitive rule.
        let f = InterceptFilter { rules: vec![FilterRule { field: "path".into(), mode: "include".into(), value: "/API".into(), sensitive: true }] };
        assert!(!f.allows("GET", "a.com", "/api", "http://a.com/api"));
        assert!(f.allows("GET", "a.com", "/API", "http://a.com/API"));
        // Method field.
        let f = InterceptFilter { rules: vec![rule("method", "include", "POST")] };
        assert!(f.allows("POST", "a.com", "/x", "http://a.com/x"));
        assert!(!f.allows("GET", "a.com", "/x", "http://a.com/x"));
    }

    #[tokio::test]
    async fn disabling_releases_queue() {
        let ic = Arc::new(Intercept::default());
        ic.set_enabled(true);
        let held = tokio::spawn({
            let ic = ic.clone();
            async move { ic.hold(Phase::Request, &flow(3), None, |_| {}).await }
        });
        wait_held(&ic).await;
        ic.set_enabled(false);
        assert!(matches!(held.await.unwrap(), Decision::Forward { raw: None, .. }));
        assert!(ic.pending().is_empty());
    }
}
