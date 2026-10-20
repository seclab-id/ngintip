//! Match & Replace: regex rules applied automatically to proxied traffic.

use std::sync::RwLock;

use bytes::Bytes;
use http::{header, HeaderMap, HeaderName, HeaderValue, Method, Uri};
use regex::bytes::Regex as BytesRegex;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{raw, store::header_vec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Target {
    /// `METHOD /path?query` (the version is left alone).
    RequestLine,
    RequestHeader,
    RequestBody,
    ResponseHeader,
    ResponseBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    pub enabled: bool,
    pub target: Target,
    /// Regex. For header targets an empty match adds `replace` as a new header.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Replacement (`$1`, `${name}` for groups). A header line that ends up
    /// empty is removed.
    pub replace: String,
    pub comment: String,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            enabled: true,
            target: Target::RequestHeader,
            pattern: String::new(),
            replace: String::new(),
            comment: String::new(),
        }
    }
}

struct Compiled {
    target: Target,
    text: Option<Regex>,
    bytes: Option<BytesRegex>,
    replace: String,
}

/// The active rules, compiled.
#[derive(Default)]
pub struct Rules {
    compiled: RwLock<Vec<Compiled>>,
}

impl Rules {
    /// Replace the rule set; every regex must compile. Returns the rules as saved.
    pub fn set(&self, rules: &[Rule]) -> Result<(), String> {
        let mut compiled = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            if !rule.enabled {
                continue;
            }
            let body = matches!(rule.target, Target::RequestBody | Target::ResponseBody);
            let header = matches!(rule.target, Target::RequestHeader | Target::ResponseHeader);
            if rule.pattern.is_empty() && !header {
                return Err(format!("Rule #{}: match cannot be empty", i + 1));
            }
            let err = |e: regex::Error| format!("Rule #{}: invalid regex: {e}", i + 1);
            let (text, bytes) = match (rule.pattern.is_empty(), body) {
                (true, _) => (None, None),
                (false, true) => (None, Some(BytesRegex::new(&rule.pattern).map_err(err)?)),
                (false, false) => (Some(Regex::new(&rule.pattern).map_err(err)?), None),
            };
            compiled.push(Compiled {
                target: rule.target,
                text,
                bytes,
                replace: rule.replace.clone(),
            });
        }
        *self.compiled.write().unwrap() = compiled;
        Ok(())
    }

    pub fn has(&self, target: Target) -> bool {
        self.compiled.read().unwrap().iter().any(|c| c.target == target)
    }

    /// Rewrite the request line; `uri` must be absolute (scheme + authority are kept
    /// unless the result is an absolute URL itself).
    pub fn apply_request_line(&self, method: &mut Method, uri: &mut Uri) {
        let rules = self.compiled.read().unwrap();
        let mut any = false;
        let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let mut line = format!("{method} {path}");
        for rule in rules.iter().filter(|c| c.target == Target::RequestLine) {
            if let Some(re) = &rule.text {
                let next = re.replace_all(&line, rule.replace.as_str());
                if next != line {
                    line = next.into_owned();
                    any = true;
                }
            }
        }
        if !any {
            return;
        }
        let Some((m, target)) = line.split_once(' ') else {
            return;
        };
        if let Ok(m) = Method::from_bytes(m.trim().as_bytes()) {
            *method = m;
        }
        let target = target.trim();
        let parsed = if target.starts_with("http://") || target.starts_with("https://") {
            target.parse().ok()
        } else {
            let mut parts = uri.clone().into_parts();
            parts.path_and_query = target.parse().ok();
            Uri::from_parts(parts).ok()
        };
        if let Some(u) = parsed {
            *uri = u;
        }
    }

    pub fn apply_headers(&self, target: Target, headers: &mut HeaderMap) {
        let rules = self.compiled.read().unwrap();
        let mut rules = rules.iter().filter(|c| c.target == target).peekable();
        if rules.peek().is_none() {
            return;
        }
        let mut lines: Vec<String> = header_vec(headers).into_iter().map(|(k, v)| format!("{k}: {v}")).collect();
        for rule in rules {
            match &rule.text {
                None => lines.push(rule.replace.clone()),
                Some(re) => {
                    for line in lines.iter_mut() {
                        if re.is_match(line) {
                            *line = re.replace_all(line, rule.replace.as_str()).into_owned();
                        }
                    }
                }
            }
        }
        let mut out = HeaderMap::new();
        for line in lines {
            let Some((k, v)) = line.split_once(':') else { continue };
            if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.trim().as_bytes()), HeaderValue::from_str(v.trim())) {
                out.append(k, v);
            }
        }
        *headers = out;
    }

    /// Rewrite a body. Compressed bodies are matched decoded; when a rule
    /// changes something the plain body is returned and Content-Encoding
    /// dropped. Content-Length is kept in sync.
    pub fn apply_body(&self, target: Target, headers: &mut HeaderMap, body: Bytes) -> Bytes {
        let rules = self.compiled.read().unwrap();
        let mut rules = rules.iter().filter(|c| c.target == target).peekable();
        if rules.peek().is_none() {
            return body;
        }
        let decoded = raw::decode_body(&header_vec(headers), &body).into_owned();
        let mut current = decoded.clone();
        for rule in rules {
            if let Some(re) = &rule.bytes {
                current = re.replace_all(&current, rule.replace.as_bytes()).into_owned();
            }
        }
        if current == decoded {
            return body;
        }
        headers.remove(header::CONTENT_ENCODING);
        if headers.contains_key(header::CONTENT_LENGTH) || !current.is_empty() {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(current.len()));
        }
        Bytes::from(current)
    }

    /// Apply the applicable rules to a pasted raw request or response and
    /// return the transformed text. Used by the standalone tester so rules can
    /// be tried without touching live traffic.
    pub fn apply_to_raw(&self, raw_text: &str, is_response: bool) -> Result<String, String> {
        if is_response {
            let r = raw::parse_response(raw_text).map_err(|e| e.to_string())?;
            let mut headers = to_headers(&r.headers);
            self.apply_headers(Target::ResponseHeader, &mut headers);
            let body = self.apply_body(Target::ResponseBody, &mut headers, r.body);
            let reason = http::StatusCode::from_u16(r.status)
                .ok()
                .and_then(|s| s.canonical_reason())
                .unwrap_or("");
            let start = format!("HTTP/1.1 {} {}", r.status, reason);
            Ok(raw::message_text(start.trim_end(), &header_vec(&headers), &body))
        } else {
            let r = raw::parse_request(raw_text).map_err(|e| e.to_string())?;
            let mut method = Method::from_bytes(r.method.as_bytes()).map_err(|e| e.to_string())?;
            let mut headers = to_headers(&r.headers);
            let host = header_vec(&headers)
                .into_iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("host"))
                .map(|(_, v)| v)
                .unwrap_or_else(|| "example.com".to_string());
            let path = if r.target.starts_with('/') || r.target.starts_with("http") {
                r.target.clone()
            } else {
                format!("/{}", r.target)
            };
            let url = if path.starts_with("http") { path.clone() } else { format!("http://{host}{path}") };
            let mut uri: Uri = url.parse().map_err(|e: http::uri::InvalidUri| e.to_string())?;
            self.apply_request_line(&mut method, &mut uri);
            self.apply_headers(Target::RequestHeader, &mut headers);
            let body = self.apply_body(Target::RequestBody, &mut headers, r.body);
            let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
            let start = format!("{method} {path} HTTP/1.1");
            Ok(raw::message_text(&start, &header_vec(&headers), &body))
        }
    }
}

fn to_headers(list: &[(String, String)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in list {
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            h.append(k, v);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(target: Target, pattern: &str, replace: &str) -> Rule {
        Rule {
            target,
            pattern: pattern.into(),
            replace: replace.into(),
            ..Rule::default()
        }
    }

    #[test]
    fn rewrites_adds_and_removes_headers() {
        let rules = Rules::default();
        rules
            .set(&[
                rule(Target::RequestHeader, "^User-Agent: .*$", "User-Agent: Ngintip"),
                rule(Target::RequestHeader, "", "X-Pentest: 1"),
                rule(Target::RequestHeader, "^Cookie: .*$", ""),
                Rule { enabled: false, ..rule(Target::RequestHeader, "", "X-Off: 1") },
            ])
            .unwrap();
        let mut h = HeaderMap::new();
        h.insert("user-agent", "curl/8".parse().unwrap());
        h.insert("cookie", "sid=1".parse().unwrap());
        rules.apply_headers(Target::RequestHeader, &mut h);
        assert_eq!(h["user-agent"], "Ngintip");
        assert_eq!(h["x-pentest"], "1");
        assert!(h.get("cookie").is_none() && h.get("x-off").is_none());

        // Other targets are untouched.
        let mut res = HeaderMap::new();
        res.insert("user-agent", "x".parse().unwrap());
        rules.apply_headers(Target::ResponseHeader, &mut res);
        assert_eq!(res["user-agent"], "x");
    }

    #[test]
    fn rewrites_compressed_body_and_fixes_headers() {
        use std::io::Write;
        let rules = Rules::default();
        rules.set(&[rule(Target::ResponseBody, r#""admin":\s*false"#, r#""admin":true"#)]).unwrap();

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(br#"{"admin": false}"#).unwrap();
        let original = Bytes::from(gz.finish().unwrap());
        let mut h = HeaderMap::new();
        h.insert("content-encoding", "gzip".parse().unwrap());
        h.insert("content-length", original.len().into());

        let out = rules.apply_body(Target::ResponseBody, &mut h, original);
        assert_eq!(&out[..], br#"{"admin":true}"#);
        assert!(h.get("content-encoding").is_none());
        assert_eq!(h["content-length"], "14");

        // No match: original bytes and headers untouched.
        let mut h = HeaderMap::new();
        h.insert("content-length", "3".parse().unwrap());
        assert_eq!(&rules.apply_body(Target::ResponseBody, &mut h, Bytes::from_static(b"abc"))[..], b"abc");
        assert_eq!(h["content-length"], "3");
    }

    #[test]
    fn rewrites_request_line_with_groups() {
        let rules = Rules::default();
        rules.set(&[rule(Target::RequestLine, r"^GET /api/v(\d)/", "POST /api/v${1}0/")]).unwrap();
        let (mut m, mut uri) = (Method::GET, "https://example.com/api/v1/users?id=2".parse::<Uri>().unwrap());
        rules.apply_request_line(&mut m, &mut uri);
        assert_eq!((m, uri.to_string()), (Method::POST, "https://example.com/api/v10/users?id=2".to_string()));
    }

    #[test]
    fn tester_applies_rules_to_a_sample() {
        let rules = Rules::default();
        rules
            .set(&[
                rule(Target::RequestLine, "^GET ", "POST "),
                rule(Target::RequestHeader, "", "X-Test: 1"),
                rule(Target::RequestBody, "role=user", "role=admin"),
            ])
            .unwrap();
        let out = rules
            .apply_to_raw("GET /a HTTP/1.1\nHost: h\nContent-Length: 9\n\nrole=user", false)
            .unwrap();
        assert!(out.starts_with("POST /a HTTP/1.1"), "{out}");
        assert!(out.contains("X-Test: 1"), "{out}");
        assert!(out.contains("role=admin"), "{out}");
    }

    #[test]
    fn rejects_bad_rules() {
        let rules = Rules::default();
        assert!(rules.set(&[rule(Target::RequestBody, "(", "x")]).unwrap_err().contains("#1"));
        assert!(rules.set(&[rule(Target::ResponseBody, "", "x")]).is_err(), "body rules need a match");
    }
}
