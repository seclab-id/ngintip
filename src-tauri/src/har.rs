//! HAR 1.2 export (http://www.softwareishard.com/blog/har-12-spec/).
//! Readable by Burp (via extension), Caido, ZAP, Chrome DevTools, etc.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use std::collections::HashMap;

use crate::{
    raw,
    store::{Flow, WsMessage},
};

pub fn build(flows: &[Flow], ws: &[WsMessage]) -> Value {
    let mut by_flow: HashMap<u64, Vec<&WsMessage>> = HashMap::new();
    for m in ws {
        by_flow.entry(m.flow_id).or_default().push(m);
    }
    json!({
        "log": {
            "version": "1.2",
            "creator": { "name": crate::brand::NAME, "version": env!("CARGO_PKG_VERSION") },
            "entries": flows
                .iter()
                .map(|f| entry(f, by_flow.get(&f.summary.id).map(Vec::as_slice).unwrap_or_default()))
                .collect::<Vec<_>>(),
        }
    })
}

/// Chrome DevTools' HAR extension for WebSocket frames.
fn ws_messages(messages: &[&WsMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let data = match std::str::from_utf8(&m.data) {
                Ok(text) if m.opcode != 2 => text.to_string(),
                _ => B64.encode(&m.data),
            };
            json!({
                "type": if m.direction == "client" { "send" } else { "receive" },
                "time": m.ts as f64 / 1000.0,
                "opcode": m.opcode,
                "data": data,
            })
        })
        .collect()
}

fn entry(f: &Flow, ws: &[&WsMessage]) -> Value {
    let s = &f.summary;
    let url = format!("{}://{}{}", s.scheme, s.host, s.path);

    let mut request = json!({
        "method": s.method,
        "url": url,
        "httpVersion": f.version,
        "cookies": [],
        "headers": headers(&f.req_headers),
        "queryString": query_string(&s.path),
        "headersSize": -1,
        "bodySize": f.req_body.len(),
    });
    if !f.req_body.is_empty() {
        request["postData"] = json!({
            "mimeType": header(&f.req_headers, "content-type").unwrap_or(""),
            "text": String::from_utf8_lossy(&f.req_body),
        });
    }

    let body = raw::decode_body(&f.res_headers, &f.res_body);
    let mut content = json!({
        "size": body.len(),
        "mimeType": header(&f.res_headers, "content-type").unwrap_or(""),
    });
    match std::str::from_utf8(&body) {
        Ok(text) => content["text"] = json!(text),
        Err(_) => {
            content["text"] = json!(B64.encode(&body));
            content["encoding"] = json!("base64");
        }
    }
    if f.res_truncated {
        content["size"] = json!(s.length);
        content["comment"] = json!(format!("truncated: only the first {} bytes were captured", f.res_body.len()));
    }
    if f.req_truncated {
        if let Some(post) = request.get_mut("postData") {
            post["comment"] = json!(format!("truncated: only the first {} bytes were captured", f.req_body.len()));
        }
    }

    let mut entry = json!({
        "startedDateTime": iso8601(s.ts),
        "time": s.duration_ms,
        "request": request,
        "response": {
            "status": s.status,
            "statusText": f.res_reason,
            "httpVersion": f.res_version,
            "cookies": [],
            "headers": headers(&f.res_headers),
            "content": content,
            "redirectURL": header(&f.res_headers, "location").unwrap_or(""),
            "headersSize": -1,
            "bodySize": f.res_body.len(),
        },
        "cache": {},
        "timings": { "send": 0, "wait": s.duration_ms, "receive": 0 },
    });
    let comment = match (&s.error, s.note.is_empty()) {
        (Some(err), true) => format!("upstream error: {err}"),
        (Some(err), false) => format!("{} (upstream error: {err})", s.note),
        (None, _) => s.note.clone(),
    };
    if !comment.is_empty() {
        entry["comment"] = json!(comment);
    }
    if !ws.is_empty() {
        entry["_resourceType"] = json!("websocket");
        entry["_webSocketMessages"] = json!(ws_messages(ws));
    }
    entry
}

fn headers(list: &[(String, String)]) -> Vec<Value> {
    list.iter()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect()
}

fn header<'a>(list: &'a [(String, String)], name: &str) -> Option<&'a str> {
    list.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn query_string(path: &str) -> Vec<Value> {
    let Some((_, query)) = path.split_once('?') else {
        return Vec::new();
    };
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            json!({ "name": name, "value": value })
        })
        .collect()
}

fn iso8601(ts_ms: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(ts_ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use bytes::Bytes;
    use flate2::{write::GzEncoder, Compression};
    use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, Version};

    use super::*;

    #[test]
    fn exports_decoded_entry() {
        let uri: Uri = "https://example.com/api?a=1&b=two".parse().unwrap();
        let mut req_headers = HeaderMap::new();
        req_headers.insert("content-type", HeaderValue::from_static("application/json"));
        let mut flow = Flow::request(
            7,
            &Method::POST,
            &uri,
            Version::HTTP_11,
            &req_headers,
            Bytes::from_static(br#"{"x":1}"#),
        );

        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(b"hello har").unwrap();
        let mut res_headers = HeaderMap::new();
        res_headers.insert("content-encoding", HeaderValue::from_static("gzip"));
        res_headers.insert("content-type", HeaderValue::from_static("text/plain"));
        flow.set_response(StatusCode::OK, Version::HTTP_11, &res_headers, Bytes::from(gz.finish().unwrap()));

        let har = build(&[flow], &[]);
        let e = &har["log"]["entries"][0];
        assert_eq!(har["log"]["version"], "1.2");
        assert_eq!(e["request"]["url"], "https://example.com/api?a=1&b=two");
        assert_eq!(e["request"]["queryString"][1]["value"], "two");
        assert_eq!(e["request"]["postData"]["text"], r#"{"x":1}"#);
        assert_eq!(e["response"]["status"], 200);
        assert_eq!(e["response"]["content"]["text"], "hello har");
        assert!(e["startedDateTime"].as_str().unwrap().contains('T'));
    }
}
