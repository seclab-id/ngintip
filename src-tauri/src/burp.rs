//! Export history as a Burp Suite "items" XML file, which Burp, and tools that
//! read Burp exports, can import. Request and response are the raw bytes,
//! base64-encoded, exactly as Burp writes them.

use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::store::Flow;

pub fn build(flows: &[Flow]) -> String {
    let mut out = String::from("<?xml version=\"1.0\"?>\n");
    out.push_str(&format!(
        "<items burpVersion=\"{} {}\" exportTime=\"{}\">\n",
        crate::brand::NAME,
        env!("CARGO_PKG_VERSION"),
        crate::store::now_ms(),
    ));
    for f in flows {
        out.push_str(&item(f));
    }
    out.push_str("</items>\n");
    out
}

fn item(f: &Flow) -> String {
    let s = &f.summary;
    let (host, port) = split_host_port(&s.host, &s.scheme);
    let url = format!("{}://{}{}", s.scheme, s.host, s.path);

    let mut xml = String::from("  <item>\n");
    xml.push_str(&tag("time", &s.ts.to_string()));
    xml.push_str(&tag("url", &esc(&url)));
    xml.push_str(&format!("    <host ip=\"\">{}</host>\n", esc(&host)));
    xml.push_str(&tag("port", &port.to_string()));
    xml.push_str(&tag("protocol", &s.scheme));
    xml.push_str(&tag("method", &esc(&s.method)));
    xml.push_str(&tag("path", &esc(&s.path)));
    xml.push_str(&tag("extension", &extension(&s.path)));
    xml.push_str(&b64_tag("request", &raw_request(f)));
    xml.push_str(&tag("status", &s.status.to_string()));
    xml.push_str(&tag("responselength", &f.res_body.len().to_string()));
    xml.push_str(&tag("mimetype", &esc(&s.mime.to_uppercase())));
    xml.push_str(&b64_tag("response", &raw_response(f)));
    if !s.note.is_empty() {
        xml.push_str(&tag("comment", &esc(&s.note)));
    }
    xml.push_str("  </item>\n");
    xml
}

fn raw_request(f: &Flow) -> Vec<u8> {
    let start = format!("{} {} {}", f.summary.method, f.summary.path, f.version);
    raw_message(&start, &f.req_headers, &f.req_body)
}

fn raw_response(f: &Flow) -> Vec<u8> {
    if f.res_version.is_empty() {
        return Vec::new();
    }
    let start = format!("{} {} {}", f.res_version, f.summary.status, f.res_reason);
    raw_message(start.trim_end(), &f.res_headers, &f.res_body)
}

/// Rebuild the on-the-wire message bytes (CRLF line endings, original body).
fn raw_message(start_line: &str, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(start_line.len() + body.len() + 64);
    out.extend_from_slice(start_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    for (k, v) in headers {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

fn tag(name: &str, value: &str) -> String {
    format!("    <{name}>{value}</{name}>\n")
}

fn b64_tag(name: &str, bytes: &[u8]) -> String {
    format!("    <{name} base64=\"true\">{}</{name}>\n", B64.encode(bytes))
}

/// `host:port` -> (host, port); port defaults to the scheme's default.
fn split_host_port(host: &str, scheme: &str) -> (String, u16) {
    let default = if scheme == "https" { 443 } else { 80 };
    match host.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h.to_string(), port),
            Err(_) => (host.to_string(), default),
        },
        None => (host.to_string(), default),
    }
}

/// File extension of the path (Burp's `<extension>`), or "null".
fn extension(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    match path.rsplit('/').next().and_then(|seg| seg.rsplit_once('.')) {
        Some((_, ext)) if !ext.is_empty() => esc(ext),
        _ => "null".to_string(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, Version};

    #[test]
    fn exports_burp_items() {
        let uri: Uri = "https://example.com/api/login.php?x=1".parse().unwrap();
        let mut req_headers = HeaderMap::new();
        req_headers.insert("host", HeaderValue::from_static("example.com"));
        let mut flow = Flow::request(1, &Method::POST, &uri, Version::HTTP_11, &req_headers, Bytes::from_static(b"a=1&b=2"));
        let mut res_headers = HeaderMap::new();
        res_headers.insert("content-type", HeaderValue::from_static("text/html"));
        flow.set_response(StatusCode::OK, Version::HTTP_11, &res_headers, Bytes::from_static(b"<html>hi & bye</html>"));

        let xml = build(&[flow]);
        assert!(xml.starts_with("<?xml version=\"1.0\"?>"));
        assert!(xml.contains("<protocol>https</protocol>"));
        assert!(xml.contains("<port>443</port>"));
        assert!(xml.contains("<method>POST</method>"));
        assert!(xml.contains("<extension>php</extension>"));
        assert!(xml.contains("<mimetype>HTML</mimetype>"));
        // Request is base64 of the raw bytes.
        let enc = xml.split("<request base64=\"true\">").nth(1).unwrap().split("</request>").next().unwrap();
        let decoded = String::from_utf8(B64.decode(enc).unwrap()).unwrap();
        assert!(decoded.starts_with("POST /api/login.php?x=1 HTTP/1.1\r\n"));
        assert!(decoded.ends_with("\r\n\r\na=1&b=2"));
        assert!(xml.contains("</items>"));
    }
}
