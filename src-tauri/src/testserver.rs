//! A small local echo server for trying features by hand: point Repeater, the
//! browser, or Intercept at it and it reflects exactly what it received, so you
//! can confirm edited headers/bodies (and Content-Length) actually arrived.

use std::convert::Infallible;

use bytes::Bytes;
use http::{header, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

pub async fn serve(listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service_fn(handle))
                .await;
        });
    }
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let target = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    let version = format!("{:?}", req.version());

    // Declared Content-Length (what the client/proxy claimed).
    let declared: Option<String> = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string());

    let mut headers_text = String::new();
    for (k, v) in req.headers() {
        headers_text.push_str(&format!("{k}: {}\n", v.to_str().unwrap_or("<non-utf8>")));
    }

    // /status/NNN -> reply with that status code.
    if let Some(rest) = path.strip_prefix("/status/") {
        let _ = req.into_body().collect().await;
        let code = rest.split('/').next().unwrap_or("").parse::<u16>().unwrap_or(200);
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
        return Ok(text(status, format!("status {code}\n")));
    }

    if path == "/" {
        return Ok(html(INDEX));
    }
    // A page that AES-CBC encrypts a JSON body in the browser (key "ngintip-demo-key",
    // random IV sent in the X-Enc-IV header) and POSTs it here, to watch an encrypted
    // request flow through the proxy and a decrypt extension.
    if path == "/test-enc" && method == hyper::Method::GET {
        return Ok(html(TEST_ENC_PAGE));
    }

    // Capture the IV header before the body is consumed.
    let enc_iv = req.headers().get("x-enc-iv").and_then(|v| v.to_str().ok()).map(str::to_string);

    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let actual = body.len();
    let matches = match &declared {
        Some(d) => d == &actual.to_string(),
        None => actual == 0,
    };

    // /json -> reflect as a small JSON object.
    if path == "/json" {
        let json = serde_json::json!({
            "method": method.as_str(),
            "path": target,
            "bodyBytes": actual,
            "contentLengthHeader": declared,
            "contentLengthMatches": matches,
            "body": String::from_utf8_lossy(&body),
        });
        return Ok(Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(serde_json::to_vec_pretty(&json).unwrap_or_default())))
            .unwrap());
    }

    // /secret -> a body "encrypted" with XOR + base64, to exercise a decrypt/encrypt
    // extension end to end. GET returns ciphertext; POST decrypts your ciphertext,
    // wraps it, and returns the result re-encrypted. Header X-Encrypted: xor-b64.
    if path == "/secret" {
        let plain = if method == hyper::Method::POST {
            let decoded = xor_b64_decode(&body);
            format!("{{\"youSent\":{:?},\"note\":\"decrypted by the test server\"}}", String::from_utf8_lossy(&decoded))
        } else {
            "{\"flag\":\"NGINTIP{extension_decrypt_works}\",\"note\":\"decrypt me with your extension\"}".to_string()
        };
        return Ok(Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("X-Encrypted", "xor-b64")
            .body(Full::new(Bytes::from(xor_b64_encode(plain.as_bytes()))))
            .unwrap());
    }

    // POST /test-enc: reflect what actually arrived. With no extension you see the
    // base64 ciphertext; with a decrypt extension you see the plaintext JSON here,
    // proving the extension transformed the request on the wire.
    if path == "/test-enc" {
        let body_text = String::from_utf8_lossy(&body);
        let looks_json = body_text.trim_start().starts_with('{') || body_text.trim_start().starts_with('[');
        let json = serde_json::json!({
            "receivedIvHeader": enc_iv,
            "receivedBody": body_text,
            "bodyBytes": actual,
            "looksDecrypted": looks_json,
            "note": "If receivedBody is plaintext JSON, an extension decrypted the request before it reached the server.",
        });
        return Ok(Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(serde_json::to_vec_pretty(&json).unwrap_or_default())))
            .unwrap());
    }

    // Default (/echo and anything else): reflect the whole request as text.
    let out = format!(
        "{method} {target} {version}\n{headers_text}\n{}\n\n--- {app} test server ---\n\
         Content-Length header: {cl}\n\
         Actual body bytes: {actual}\n\
         Content-Length matches body: {ok}\n",
        String::from_utf8_lossy(&body),
        app = crate::brand::NAME,
        cl = declared.as_deref().unwrap_or("(none)"),
        ok = if matches { "yes" } else { "NO — mismatch" },
    );
    Ok(text(StatusCode::OK, out))
}

/// Demo cipher for the /secret endpoint: XOR with a fixed key, then base64. Not
/// real crypto, just a reversible transform for testing decrypt/encrypt extensions.
const XOR_KEY: &[u8] = b"ngintip-demo-key";

fn xor_b64_encode(plain: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let x: Vec<u8> = plain.iter().enumerate().map(|(i, b)| b ^ XOR_KEY[i % XOR_KEY.len()]).collect();
    STANDARD.encode(x)
}

fn xor_b64_decode(b64: &[u8]) -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let trimmed: Vec<u8> = b64.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    let raw = STANDARD.decode(trimmed).unwrap_or_default();
    raw.iter().enumerate().map(|(i, b)| b ^ XOR_KEY[i % XOR_KEY.len()]).collect()
}

fn text(status: StatusCode, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(body.into()))
        .unwrap()
}

fn html(body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .unwrap()
}

const INDEX: &str = r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Ngintip test server</title>
<body style="font:15px/1.6 system-ui;max-width:620px;margin:8vh auto;padding:0 16px;color:#222">
<h1>Ngintip test server</h1>
<p>Reflects your request so you can check what actually arrived (handy for verifying edited headers/bodies and Content-Length through Intercept or Repeater).</p>
<ul>
<li><b>/echo</b> — echoes the method, headers and body, and whether Content-Length matched the body</li>
<li><b>/json</b> — the same, as JSON</li>
<li><b>/secret</b> — a body "encrypted" with XOR+base64 (header <code>X-Encrypted: xor-b64</code>), to test a decrypt/encrypt extension end to end</li>
<li><b>/test-enc</b> — a page with a button that AES-CBC encrypts a JSON body in the browser and POSTs it (IV in <code>X-Enc-IV</code>), to watch an encrypted request flow through the proxy</li>
<li><b>/status/&lt;code&gt;</b> — replies with that status code</li>
</ul>
<p>Send any method (GET/POST/PUT/…) with any headers and body.</p></body>"#;

// Page that AES-CBC encrypts a JSON body in the browser and POSTs it to /test-enc.
// Key is the 16-byte "ngintip-demo-key" (AES-128); the random IV goes in X-Enc-IV.
// The matching decrypt extension is examples/aes_decrypt_extension.py.
const TEST_ENC_PAGE: &str = r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Ngintip /test-enc</title>
<body style="font:15px/1.6 system-ui;max-width:680px;margin:6vh auto;padding:0 16px;color:#222">
<h1>Encrypted POST demo</h1>
<p>Clicking the button AES-CBC encrypts a JSON body in your browser (key <code>ngintip-demo-key</code>, random IV sent in the <code>X-Enc-IV</code> header) and POSTs the base64 ciphertext to <code>/test-enc</code>. Watch it in History: without an extension the request body is ciphertext; with the AES decrypt extension on, it becomes readable and the original ciphertext is kept under the <b>Original</b> tab.</p>
<button id="go" style="font:600 15px system-ui;padding:10px 18px;border:0;border-radius:8px;background:#5b4bff;color:#fff;cursor:pointer">POST encrypted body</button>
<pre id="out" style="margin-top:16px;padding:12px;background:#f4f4f8;border-radius:8px;white-space:pre-wrap;word-break:break-word"></pre>
<script>
const KEY = new TextEncoder().encode('ngintip-demo-key');
const b64 = (buf) => btoa(String.fromCharCode.apply(null, new Uint8Array(buf)));
async function send() {
  const out = document.getElementById('out');
  out.textContent = 'encrypting...';
  try {
    const key = await crypto.subtle.importKey('raw', KEY, {name:'AES-CBC'}, false, ['encrypt']);
    const iv = crypto.getRandomValues(new Uint8Array(16));
    const plaintext = JSON.stringify({user:'admin', action:'transfer', amount:1000, ts:Date.now()});
    const ct = await crypto.subtle.encrypt({name:'AES-CBC', iv}, key, new TextEncoder().encode(plaintext));
    const res = await fetch('/test-enc', {
      method:'POST',
      headers:{'Content-Type':'application/octet-stream','X-Enc-IV':b64(iv)},
      body: b64(ct),
    });
    out.textContent = 'plaintext (before encryption):\n' + plaintext +
      '\n\nX-Enc-IV: ' + b64(iv) +
      '\n\nserver response:\n' + await res.text();
  } catch (e) { out.textContent = 'error: ' + e; }
}
document.getElementById('go').addEventListener('click', send);
</script></body>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn roundtrip(addr: SocketAddr, req: &str) -> String {
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut out)).await.unwrap().unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn echoes_request_and_flags_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener));

        // Correct Content-Length.
        let res = roundtrip(addr, "POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        assert!(res.contains("POST /echo"), "{res}");
        assert!(res.contains("Content-Length matches body: yes"), "{res}");
        assert!(res.contains("Actual body bytes: 5"), "{res}");

        // Wrong Content-Length is flagged (the server reads what actually arrived).
        let res = roundtrip(addr, "POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhello").await;
        assert!(res.contains("NO — mismatch") || res.contains("Actual body bytes: 2"), "{res}");

        // Status route.
        let res = roundtrip(addr, "GET /status/418 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
        assert!(res.starts_with("HTTP/1.1 418"), "{res}");
    }

    #[tokio::test]
    async fn test_enc_page_and_echo() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener));
        // GET serves the encrypt page with the button.
        let page = roundtrip(addr, "GET /test-enc HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
        assert!(page.contains("POST encrypted body") && page.contains("AES-CBC"), "{page}");
        // POST reflects the body and the IV header as JSON (so the flow is observable).
        let res = roundtrip(addr, "POST /test-enc HTTP/1.1\r\nHost: x\r\nX-Enc-IV: aXYtaGVhZGVy\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await;
        assert!(res.contains("\"receivedIvHeader\": \"aXYtaGVhZGVy\""), "{res}");
        assert!(res.contains("\"receivedBody\": \"hello\""), "{res}");
    }

    #[test]
    fn xor_b64_is_reversible() {
        let plain = b"NGINTIP{extension_decrypt_works}";
        let enc = xor_b64_encode(plain);
        assert_ne!(enc.as_bytes(), &plain[..], "ciphertext differs from plaintext");
        assert_eq!(xor_b64_decode(enc.as_bytes()), plain);
    }

    #[tokio::test]
    async fn secret_endpoint_encrypts_and_decrypts() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener));

        // GET returns an encrypted body that decodes to the flag.
        let res = roundtrip(addr, "GET /secret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await;
        assert!(res.to_lowercase().contains("x-encrypted: xor-b64"), "{res}");
        let body = res.rsplit("\r\n\r\n").next().unwrap();
        let plain = String::from_utf8(xor_b64_decode(body.as_bytes())).unwrap();
        assert!(plain.contains("NGINTIP{extension_decrypt_works}"), "decoded: {plain}");

        // POST an encrypted body; the server decrypts it and echoes it back.
        let payload = xor_b64_encode(b"hello-secret");
        let req = format!(
            "POST /secret HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let res = roundtrip(addr, &req).await;
        let body = res.rsplit("\r\n\r\n").next().unwrap();
        let plain = String::from_utf8(xor_b64_decode(body.as_bytes())).unwrap();
        assert!(plain.contains("hello-secret"), "server saw: {plain}");
    }
}
