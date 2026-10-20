//! Raw HTTP text: rendering flows for the UI and parsing Repeater input.

use std::{borrow::Cow, io::Read};

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};

use crate::store::Flow;

/// Bodies larger than this are truncated in the viewer; the full body is still
/// stored and can be fetched with the full-text renderers below ("Show all").
/// Kept modest so opening a request stays instant even for big responses.
pub const VIEW_LIMIT: usize = 256 * 1024;

pub fn request_text(f: &Flow) -> String {
    request_text_impl(f, false)
}

/// Full request, no view cap (for the viewer's Show all).
pub fn request_text_full(f: &Flow) -> String {
    request_text_impl(f, true)
}

fn request_text_impl(f: &Flow, full: bool) -> String {
    let start = format!("{} {} {}", f.summary.method, f.summary.path, f.version);
    let mut out = render(&start, &f.req_headers, &f.req_body, full);
    if f.req_truncated {
        out.push_str(&truncation_note(f.req_body.len(), None));
    }
    out
}

/// Bytes shown as a hex dump for a binary body in the Pretty (hex) view.
const HEX_VIEW_LIMIT: usize = 64 * 1024;

/// Whether a body looks binary rather than text. A NUL byte in the sample is a strong
/// signal, but many binary formats (PDF, some images/fonts) start with ASCII and only
/// hit a NUL well past the first KB, so the Content-Type is the primary signal:
/// anything that is not a text-ish media type is binary. Missing/unknown Content-Type
/// falls back to the NUL sniff. Used only to OFFER a hex (Pretty) view; the Raw view
/// always shows the body as-is.
fn is_binary_body(headers: &[(String, String)], decoded: &[u8]) -> bool {
    if decoded[..decoded.len().min(4096)].contains(&0) {
        return true;
    }
    match header(headers, "content-type").map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase()) {
        Some(ct) if !ct.is_empty() => {
            let texty = ct.starts_with("text/")
                || ct.starts_with("multipart/")
                || ct.contains("json")
                || ct.contains("xml")
                || ct.contains("javascript")
                || ct.contains("ecmascript")
                || ct.contains("csv")
                || ct == "application/x-www-form-urlencoded"
                || ct == "application/graphql";
            !texty
        }
        // No Content-Type: the NUL sniff already said it looks like text.
        _ => false,
    }
}

/// Whether a body looks binary (so the UI can offer a hex Pretty view). Cheap: only a
/// small prefix is decoded for the sniff.
pub fn body_is_binary(headers: &[(String, String)], body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    let (decoded, _) = decode_body_capped(headers, body, 8192);
    is_binary_body(headers, &decoded)
}

/// A hex dump of the body (Content-Encoding undone), capped, for the Pretty view.
pub fn hex_view(headers: &[(String, String)], body: &[u8]) -> String {
    if body.is_empty() {
        return String::new();
    }
    let (decoded, _more) = decode_body_capped(headers, body, VIEW_LIMIT);
    hex_dump(&decoded, HEX_VIEW_LIMIT)
}

/// True when the viewer would cut this body, so a "Show all" is offered. Uses a
/// capped decode so a huge/compressed body isn't fully expanded just to decide.
pub fn view_truncated(headers: &[(String, String)], body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    let (decoded, more) = decode_body_capped(headers, body, VIEW_LIMIT);
    decoded.len() > VIEW_LIMIT || more
}

pub fn response_text(f: &Flow) -> String {
    response_text_impl(f, false)
}

/// Full response, no view cap (for the viewer's Show all).
pub fn response_text_full(f: &Flow) -> String {
    response_text_impl(f, true)
}

/// Full message text for machine processing by an extension: headers plus the body
/// with Content-Encoding undone, as lossy UTF-8, and WITHOUT the viewer's hex-dump /
/// placeholder / truncation rendering. An extension needs the real payload (e.g. a
/// base64 or encrypted body), not a human hex view.
pub fn request_text_for_processing(f: &Flow) -> String {
    let start = format!("{} {} {}", f.summary.method, f.summary.path, f.version);
    proc_message(&start, &f.req_headers, &f.req_body)
}

pub fn response_text_for_processing(f: &Flow) -> String {
    if f.res_version.is_empty() {
        return String::new();
    }
    let start = format!("{} {} {}", f.res_version, f.summary.status, f.res_reason);
    proc_message(start.trim_end(), &f.res_headers, &f.res_body)
}

fn proc_message(start_line: &str, headers: &[(String, String)], body: &[u8]) -> String {
    let decoded = decode_body(headers, body);
    let mut out = String::with_capacity(256 + decoded.len());
    out.push_str(start_line);
    out.push('\n');
    write_headers(&mut out, headers);
    out.push('\n');
    out.push_str(&String::from_utf8_lossy(&decoded));
    out
}

fn response_text_impl(f: &Flow, full: bool) -> String {
    // No status line means the request never got a response.
    if f.res_version.is_empty() {
        let err = f.summary.error.as_deref().unwrap_or("no response");
        return format!("-- upstream error --\n{err}");
    }
    let start = format!("{} {} {}", f.res_version, f.summary.status, f.res_reason);
    let mut out = render(start.trim_end(), &f.res_headers, &f.res_body, full);
    if f.res_truncated {
        out.push_str(&truncation_note(f.res_body.len(), Some(f.summary.length)));
    }
    if f.summary.streaming {
        out.push_str(&format!("\n\n[{}: response still streaming...]", crate::brand::NAME));
    } else if let Some(err) = &f.summary.error {
        out.push_str(&format!("\n\n[{}: {err}]", crate::brand::NAME));
    }
    out
}

fn truncation_note(stored: usize, total: Option<usize>) -> String {
    let app = crate::brand::NAME;
    match total {
        Some(total) => format!("\n\n[{app}: body is {total} bytes; only the first {stored} are shown]"),
        None => format!("\n\n[{app}: body larger than the view limit; only the first {stored} bytes are shown]"),
    }
}

pub fn message_text(start_line: &str, headers: &[(String, String)], body: &[u8]) -> String {
    render(start_line, headers, body, false)
}

/// Write the header lines, folding several request `Cookie` fields into a single
/// `Cookie:` line (joined by "; ") the way the request is actually sent upstream.
/// An HTTP/2 client splits its cookie jar across many `cookie` fields, which the
/// viewer/editor would otherwise show as an ugly stack. Response `Set-Cookie`
/// headers are left untouched: several of those are normal and each must stay on
/// its own line. Folding is lossless: every cookie is still shown.
fn write_headers(out: &mut String, headers: &[(String, String)]) {
    let mut cookie_done = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("cookie") {
            if cookie_done {
                continue; // already emitted the joined line at the first cookie
            }
            cookie_done = true;
            let joined = headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
                .map(|(_, v)| v.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            out.push_str(k);
            out.push_str(": ");
            out.push_str(&joined);
            out.push('\n');
            continue;
        }
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
        out.push('\n');
    }
}

fn render(start_line: &str, headers: &[(String, String)], body: &[u8], full: bool) -> String {
    let mut out = String::with_capacity(256 + body.len().min(if full { body.len() } else { VIEW_LIMIT }));
    out.push_str(start_line);
    out.push('\n');
    write_headers(&mut out, headers);
    out.push('\n');
    out.push_str(&body_text_impl(headers, body, full));
    out
}

/// Body with Content-Encoding (gzip/deflate/br) undone when possible.
pub fn decode_body<'a>(headers: &[(String, String)], body: &'a [u8]) -> Cow<'a, [u8]> {
    let encoding = header(headers, "content-encoding").map(|v| v.trim().to_ascii_lowercase());

    match encoding.as_deref() {
        Some("gzip" | "x-gzip") => read_all(GzDecoder::new(body))
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(body)),
        Some("deflate") => read_all(ZlibDecoder::new(body))
            .or_else(|_| read_all(DeflateDecoder::new(body)))
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(body)),
        Some("br") => read_all(brotli_decompressor::Decompressor::new(body, 4096))
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(body)),
        Some("zstd") => ruzstd::StreamingDecoder::new(body)
            .ok()
            .and_then(|dec| read_all(dec).ok())
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(body)),
        _ => Cow::Borrowed(body),
    }
}

/// Read at most `cap`+1 bytes from a reader, stopping early. Bounds the work when
/// only a capped view is needed, so a body that decompresses to hundreds of MB
/// (or a decompression bomb) doesn't get fully expanded just to show 512 KB.
fn read_capped(mut r: impl Read, cap: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(cap.min(1 << 16) + 1);
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > cap {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    out
}

/// Like [`decode_body`] but decodes at most `cap`+1 bytes. Returns the (possibly
/// partial) decoded bytes and whether there is more beyond the cap.
pub fn decode_body_capped(headers: &[(String, String)], body: &[u8], cap: usize) -> (Vec<u8>, bool) {
    let encoding = header(headers, "content-encoding").map(|v| v.trim().to_ascii_lowercase());
    let decoded: Option<Vec<u8>> = match encoding.as_deref() {
        Some("gzip" | "x-gzip") => Some(read_capped(GzDecoder::new(body), cap)),
        Some("deflate") => {
            let z = read_capped(ZlibDecoder::new(body), cap);
            if z.is_empty() && !body.is_empty() {
                Some(read_capped(DeflateDecoder::new(body), cap))
            } else {
                Some(z)
            }
        }
        Some("br") => Some(read_capped(brotli_decompressor::Decompressor::new(body, 4096), cap)),
        Some("zstd") => ruzstd::StreamingDecoder::new(body).ok().map(|dec| read_capped(dec, cap)),
        _ => None,
    };
    // Fall back to the raw (compressed) bytes when there's no encoding or decoding
    // produced nothing, mirroring decode_body's best-effort behaviour.
    let out = match decoded {
        Some(d) if !d.is_empty() || body.is_empty() => d,
        _ => body[..body.len().min(cap + 1)].to_vec(),
    };
    let more = out.len() > cap;
    (out, more)
}

/// Human-readable body: decompressed when possible, a hex dump for binary.
/// This is what the editors show, so an edit is compared against it.
pub fn body_text(headers: &[(String, String)], body: &[u8]) -> String {
    body_text_impl(headers, body, false)
}

fn body_text_impl(headers: &[(String, String)], body: &[u8], full: bool) -> String {
    if body.is_empty() {
        return String::new();
    }
    // Full "Show all": decode everything. Default view: decode at most VIEW_LIMIT
    // so a body that expands to hundreds of MB does not freeze the app.
    // The Raw view always shows the body as-is (Content-Encoding undone), never a hex
    // dump. Hex is a separate Pretty view (see hex_view), fetched on demand.
    if full {
        let decoded = decode_body(headers, body);
        return String::from_utf8_lossy(&decoded).into_owned();
    }

    let (decoded, more) = decode_body_capped(headers, body, VIEW_LIMIT);
    let text = String::from_utf8_lossy(&decoded);
    if !more && text.len() <= VIEW_LIMIT {
        return text.into_owned();
    }
    let mut cut = text.len().min(VIEW_LIMIT);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n\n[truncated: showing the first {} bytes; use Show all for the rest]", &text[..cut], cut)
}

/// Classic `offset  hex  |ascii|` dump, capped at `limit` bytes with a note.
fn hex_dump(data: &[u8], limit: usize) -> String {
    let shown = &data[..data.len().min(limit)];
    // 16 bytes per row -> ~78 chars; preallocate roughly.
    let mut out = String::with_capacity(shown.len() / 16 * 80 + 64);
    for (row, chunk) in shown.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", row * 16));
        for (i, b) in chunk.iter().enumerate() {
            out.push_str(&format!("{b:02x} "));
            if i == 7 {
                out.push(' ');
            }
        }
        for i in chunk.len()..16 {
            out.push_str("   ");
            if i == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for b in chunk {
            out.push(if b.is_ascii_graphic() || *b == b' ' { *b as char } else { '.' });
        }
        out.push_str("|\n");
    }
    if data.len() > shown.len() {
        out.push_str(&format!("\n[binary body: hex dump of the first {} of {} bytes]", shown.len(), data.len()));
    } else {
        out.push_str(&format!("\n[binary body: {} bytes]", data.len()));
    }
    out
}

/// Whatever decodes before an error still counts (e.g. a body truncated for storage).
fn read_all(mut r: impl Read) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    match r.read_to_end(&mut out) {
        Err(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

/// Largest image the viewer will preview inline.
const MAX_PREVIEW: usize = 8 * 1024 * 1024;

/// Whether the response looks like an image (cheap header check), so the UI can
/// offer a Preview tab without eagerly base64-encoding the whole image.
pub fn is_image(headers: &[(String, String)], body: &[u8]) -> bool {
    !body.is_empty()
        && header(headers, "content-type")
            .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase().starts_with("image/"))
            .unwrap_or(false)
}

/// `data:` URL for image bodies so the UI can render a preview.
pub fn image_data_url(headers: &[(String, String)], body: &[u8]) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let mime = header(headers, "content-type")
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())?;
    if !mime.starts_with("image/") || body.is_empty() {
        return None;
    }
    let decoded = decode_body(headers, body);
    if decoded.len() > MAX_PREVIEW {
        return None;
    }
    Some(format!("data:{mime};base64,{}", STANDARD.encode(&decoded)))
}

pub struct RawRequest {
    pub method: String,
    pub target: String,
    /// As written on the request line, e.g. `HTTP/1.1` or `HTTP/2` (default `HTTP/1.1`).
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// Lenient parser for hand-edited requests (accepts `\n` or `\r\n`).
pub fn parse_request(raw: &str) -> Result<RawRequest> {
    let (start, headers, body) = parse_message(raw, "request")?;
    let mut parts = start.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        bail!("invalid request line: {start:?}");
    };
    Ok(RawRequest {
        method: method.to_string(),
        target: target.to_string(),
        version: parts.next().unwrap_or("HTTP/1.1").to_ascii_uppercase(),
        headers,
        body,
    })
}

/// Lenient parser for an edited response: `HTTP/1.1 200 OK`, headers, body.
pub fn parse_response(raw: &str) -> Result<RawResponse> {
    let (start, headers, body) = parse_message(raw, "response")?;
    let status = start
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..1000).contains(s))
        .ok_or_else(|| anyhow!("invalid status line: {start:?}"))?;
    Ok(RawResponse { status, headers, body })
}

fn parse_message(raw: &str, what: &str) -> Result<(String, Vec<(String, String)>, Bytes)> {
    let norm = raw.replace("\r\n", "\n");
    let norm = norm.trim_start_matches('\n');
    let (head, body) = match norm.find("\n\n") {
        Some(i) => (&norm[..i], &norm[i + 2..]),
        None => (norm.trim_end_matches('\n'), ""),
    };

    let mut lines = head.lines();
    let start = lines.next().ok_or_else(|| anyhow!("{what} is empty"))?;

    let mut headers = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid header: {line:?}"))?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok((start.to_string(), headers, Bytes::copy_from_slice(body.as_bytes())))
}

/// Editors only produce `\n`. Use `\r\n` again when the original body did
/// (or, without an original, for multipart bodies, which require it).
pub fn restore_crlf(body: &[u8], original: Option<&[u8]>, headers: &[(String, String)]) -> Bytes {
    let wants_crlf = match original {
        Some(orig) => {
            let crlf = orig.windows(2).filter(|w| w == b"\r\n").count();
            crlf > 0 && crlf == orig.iter().filter(|&&b| b == b'\n').count()
        }
        None => header(headers, "content-type").is_some_and(|ct| ct.to_ascii_lowercase().starts_with("multipart/")),
    };
    if !wants_crlf || !body.contains(&b'\n') {
        return Bytes::copy_from_slice(body);
    }
    let text = String::from_utf8_lossy(body).replace("\r\n", "\n").replace('\n', "\r\n");
    Bytes::from(text)
}

/// False when the body isn't actually in its declared Content-Encoding
/// (e.g. an edited, now-plain body under a stale `gzip` header).
pub fn matches_encoding(headers: &[(String, String)], body: &[u8]) -> bool {
    let Some(enc) = header(headers, "content-encoding").map(|v| v.trim().to_ascii_lowercase()) else {
        return true;
    };
    if body.is_empty() {
        return true;
    }
    let complete = |mut r: Box<dyn Read + '_>| {
        let mut out = Vec::new();
        r.read_to_end(&mut out).is_ok()
    };
    match enc.as_str() {
        "gzip" | "x-gzip" => complete(Box::new(GzDecoder::new(body))),
        "deflate" => complete(Box::new(ZlibDecoder::new(body))) || complete(Box::new(DeflateDecoder::new(body))),
        "br" => complete(Box::new(brotli_decompressor::Decompressor::new(body, 4096))),
        // Unknown codings: leave the header alone.
        _ => true,
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_crlf_like_the_original() {
        let none: Vec<(String, String)> = vec![];
        assert_eq!(&restore_crlf(b"a\nb", Some(b"x\r\ny"), &none)[..], b"a\r\nb");
        assert_eq!(&restore_crlf(b"a\nb", Some(b"x\ny"), &none)[..], b"a\nb");
        assert_eq!(&restore_crlf(b"a\nb", None, &none)[..], b"a\nb");
        let multipart = vec![("Content-Type".to_string(), "multipart/form-data; boundary=x".to_string())];
        assert_eq!(&restore_crlf(b"--x\nA\n--x--", None, &multipart)[..], b"--x\r\nA\r\n--x--");
    }

    #[test]
    fn folds_request_cookies_but_keeps_set_cookie_separate() {
        // Several request Cookie fields (as an HTTP/2 client / a pre-fix capture
        // stores them) collapse into one tidy line, at the first cookie's position.
        let req_headers = vec![
            ("Host".to_string(), "x".to_string()),
            ("Cookie".to_string(), "a=1".to_string()),
            ("Cookie".to_string(), "b=2".to_string()),
            ("Cookie".to_string(), "c=3".to_string()),
            ("Accept".to_string(), "*/*".to_string()),
        ];
        let text = message_text("GET / HTTP/2", &req_headers, b"");
        let cookie_lines: Vec<&str> = text.lines().filter(|l| l.to_ascii_lowercase().starts_with("cookie:")).collect();
        assert_eq!(cookie_lines, vec!["Cookie: a=1; b=2; c=3"], "cookies fold into one line: {text}");
        assert!(text.find("Cookie:").unwrap() < text.find("Accept:").unwrap(), "cookie keeps its position: {text}");

        // Response Set-Cookie headers are NOT folded: several are normal and each
        // must stay on its own line.
        let res_headers = vec![
            ("Set-Cookie".to_string(), "a=1; Path=/".to_string()),
            ("Set-Cookie".to_string(), "b=2; HttpOnly".to_string()),
        ];
        let text = message_text("HTTP/1.1 200 OK", &res_headers, b"");
        let n = text.lines().filter(|l| l.to_ascii_lowercase().starts_with("set-cookie:")).count();
        assert_eq!(n, 2, "multiple Set-Cookie stay separate: {text}");
    }

    #[test]
    fn parses_edited_response() {
        let r = parse_response("HTTP/1.1 403 Forbidden\nX-A: 1\n\nnope").unwrap();
        assert_eq!((r.status, r.headers[0].1.as_str(), &r.body[..]), (403, "1", &b"nope"[..]));
        assert!(parse_response("HTTP/1.1 abc\n\n").is_err());
    }

    #[test]
    fn large_compressed_body_is_capped_in_the_view() {
        use std::io::Write;
        // 4 MB that expands well past VIEW_LIMIT but gzips to a few KB.
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&vec![b'a'; 4 * 1024 * 1024]).unwrap();
        let gz = enc.finish().unwrap();
        let headers = vec![("content-encoding".to_string(), "gzip".to_string())];
        // The view is capped near VIEW_LIMIT (not the full 4 MB) and flagged truncated.
        assert!(view_truncated(&headers, &gz));
        let view = body_text(&headers, &gz);
        assert!(view.contains("[truncated"), "expected a truncation note");
        assert!(view.len() < VIEW_LIMIT + 512, "view should be about VIEW_LIMIT, got {}", view.len());
        // The capped decode only reads a bit past the cap, not the whole 4 MB.
        let (decoded, more) = decode_body_capped(&headers, &gz, VIEW_LIMIT);
        assert!(more && decoded.len() <= VIEW_LIMIT + 8192);
    }

    #[test]
    fn decodes_truncated_gzip_partially() {
        use std::io::Write;
        let text = "abcdefghij".repeat(2000);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(text.as_bytes()).unwrap();
        let full = gz.finish().unwrap();
        let headers = vec![("Content-Encoding".to_string(), "gzip".to_string())];
        let partial = decode_body(&headers, &full[..full.len() / 2]);
        assert!(!partial.is_empty() && text.starts_with(std::str::from_utf8(&partial).unwrap()));
    }

    #[test]
    fn decodes_zstd_response_as_text() {
        // `printf '<html><body>hello zstd</body></html>' | zstd -c`
        let zstd = [
            0x28, 0xb5, 0x2f, 0xfd, 0x04, 0x58, 0x21, 0x01, 0x00, 0x3c, 0x68, 0x74, 0x6d, 0x6c, 0x3e, 0x3c,
            0x62, 0x6f, 0x64, 0x79, 0x3e, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x7a, 0x73, 0x74, 0x64, 0x3c,
            0x2f, 0x62, 0x6f, 0x64, 0x79, 0x3e, 0x3c, 0x2f, 0x68, 0x74, 0x6d, 0x6c, 0x3e, 0xee, 0x76, 0xb7,
            0xaf,
        ];
        let headers = vec![("Content-Encoding".to_string(), "zstd".to_string())];
        assert_eq!(&*decode_body(&headers, &zstd), b"<html><body>hello zstd</body></html>");
        // And it's shown as text, not a hex dump.
        assert_eq!(body_text(&headers, &zstd), "<html><body>hello zstd</body></html>");
    }

    #[test]
    fn raw_view_shows_bytes_as_is_hex_is_separate() {
        let none: Vec<(String, String)> = vec![];
        let body = b"\x00\x01\x02ABCz";
        // Raw view is the bytes as-is (lossy UTF-8), NOT a hex dump.
        let raw = body_text(&none, body);
        assert!(!raw.contains("00000000  "), "raw is not a hex dump: {raw:?}");
        assert!(raw.contains("ABCz"), "raw keeps the readable bytes: {raw:?}");
        // The hex dump is the separate Pretty (hex) view.
        let hex = hex_view(&none, body);
        assert!(hex.contains("00000000  "), "hex view has an offset column: {hex}");
        assert!(hex.contains("00 01 02 41 42 43 7a"), "hex bytes: {hex}");
        assert!(hex.contains("|...ABCz|"), "ascii column: {hex}");
        // Plain text is shown verbatim.
        assert_eq!(body_text(&none, b"hello world"), "hello world");
    }

    #[test]
    fn body_is_binary_uses_content_type_and_nul() {
        // PDF: ASCII header, no NUL in the first KB -> binary by Content-Type, so the
        // UI offers a hex Pretty view. But the Raw view is still the bytes as-is.
        let pdf = vec![("Content-Type".to_string(), "application/pdf".to_string())];
        let mut body = b"%PDF-1.4\n3 0 obj\n".to_vec();
        body.extend(std::iter::repeat(b'A').take(20_000));
        assert!(body_is_binary(&pdf, &body), "pdf is binary");
        assert!(!body_text(&pdf, &body).contains("00000000  "), "pdf Raw is as-is, not hex");
        assert!(hex_view(&pdf, &body).contains("00000000  "), "pdf hex view is a hex dump");
        // NUL bytes with no Content-Type -> binary.
        let none: Vec<(String, String)> = vec![];
        assert!(body_is_binary(&none, b"\x00\x01\x02abc"), "nul -> binary");
        // JSON / HTML / octet-stream-of-base64-text are not flagged binary.
        let json = vec![("Content-Type".to_string(), "application/json".to_string())];
        assert!(!body_is_binary(&json, b"{\"a\":1}"), "json is text");
        let html = vec![("Content-Type".to_string(), "text/html".to_string())];
        assert!(!body_is_binary(&html, b"<html>ok</html>"), "html is text");
        // A base64 ciphertext body (no NUL, no content-type) stays text/raw.
        assert!(!body_is_binary(&none, b"eyJ1c2VyIjoiYWRtaW4ifQ=="), "base64 text is not binary");
    }

    #[test]
    fn large_body_is_capped_with_a_note() {
        let none: Vec<(String, String)> = vec![];
        let body = vec![b'A'; VIEW_LIMIT + 5000];
        assert!(view_truncated(&none, &body), "offers Show all");
        let view = body_text(&none, &body);
        assert!(view.contains("[truncated"), "raw view notes truncation: ...{}", &view[view.len() - 80..]);
        // Show all returns the full bytes as-is (no hex).
        assert!(!body_text_impl(&none, &body, true).contains("00000000  "), "Show all is raw, not hex");
    }

    #[test]
    fn decodes_brotli() {
        // `printf 'hello brotli dari intip' | brotli -c`
        let br = [
            0x0f, 0x0b, 0x80, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x62, 0x72, 0x6f, 0x74, 0x6c, 0x69,
            0x20, 0x64, 0x61, 0x72, 0x69, 0x20, 0x69, 0x6e, 0x74, 0x69, 0x70, 0x03,
        ];
        let headers = vec![("Content-Encoding".to_string(), "br".to_string())];
        assert_eq!(&*decode_body(&headers, &br), b"hello brotli dari intip");
    }
}
