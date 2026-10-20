#!/usr/bin/env python3
"""Standalone test server for manual experiments.

Mirrors the app's built-in test server so you can poke at it directly or through
the proxy, without opening the GUI.

Endpoints:
  GET  /            index (this list)
  GET  /test-enc    page with a button that AES-CBC encrypts a JSON body in the
                    browser (key "ngintip-demo-key", random IV in the X-Enc-IV
                    header) and POSTs the base64 ciphertext here
  POST /test-enc    reflects what actually arrived, as JSON (so the flow is visible)
  GET  /secret      body "encrypted" with XOR+base64 (header X-Encrypted: xor-b64)
  POST /secret      XOR-decrypts your body and echoes it back (re-encrypted)
  ANY  /echo /json  reflect the request (method, headers, body)
  GET  /status/NNN  reply with that status code

Run:
  python3 scripts/test_server.py [port]      # default 8081

Try it:
  - Browser: open http://127.0.0.1:8081/test-enc and click the button.
  - CLI:     python3 scripts/aes_client.py                 (direct)
             python3 scripts/aes_client.py --proxy http://127.0.0.1:8080
"""
import base64
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

XOR_KEY = b"ngintip-demo-key"


def xor(data: bytes) -> bytes:
    return bytes(b ^ XOR_KEY[i % len(XOR_KEY)] for i, b in enumerate(data))


INDEX = """<!doctype html><meta charset=utf-8><title>test server</title>
<body style="font:15px/1.6 system-ui;max-width:640px;margin:8vh auto;padding:0 16px">
<h1>Standalone test server</h1>
<ul>
<li><a href="/test-enc"><b>/test-enc</b></a> - AES encrypt page (button)</li>
<li><b>/secret</b> - XOR+base64 encrypted body</li>
<li><b>/echo</b>, <b>/json</b> - reflect the request</li>
<li><b>/status/&lt;code&gt;</b> - reply with that status</li>
</ul></body>"""

PAGE = """<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width">
<title>/test-enc</title>
<body style="font:15px/1.6 system-ui;max-width:680px;margin:6vh auto;padding:0 16px;color:#222">
<h1>Encrypted POST demo</h1>
<p>The button AES-CBC encrypts a JSON body in your browser (key <code>ngintip-demo-key</code>,
random IV in the <code>X-Enc-IV</code> header) and POSTs the base64 ciphertext to
<code>/test-enc</code>. Send it through the proxy and watch it in History: without an
extension the body is ciphertext; with the AES decrypt extension on it becomes
readable and the original ciphertext is kept under the <b>Original</b> tab.</p>
<button id=go style="font:600 15px system-ui;padding:10px 18px;border:0;border-radius:8px;background:#5b4bff;color:#fff;cursor:pointer">POST encrypted body</button>
<pre id=out style="margin-top:16px;padding:12px;background:#f4f4f8;border-radius:8px;white-space:pre-wrap;word-break:break-word"></pre>
<script>
const KEY = new TextEncoder().encode('ngintip-demo-key');
const b64 = (buf) => btoa(String.fromCharCode.apply(null, new Uint8Array(buf)));
async function send() {
  const out = document.getElementById('out'); out.textContent = 'encrypting...';
  try {
    const key = await crypto.subtle.importKey('raw', KEY, {name:'AES-CBC'}, false, ['encrypt']);
    const iv = crypto.getRandomValues(new Uint8Array(16));
    const plaintext = JSON.stringify({user:'admin', action:'transfer', amount:1000, ts:Date.now()});
    const ct = await crypto.subtle.encrypt({name:'AES-CBC', iv}, key, new TextEncoder().encode(plaintext));
    const res = await fetch('/test-enc', {method:'POST',
      headers:{'Content-Type':'application/octet-stream','X-Enc-IV':b64(iv)}, body:b64(ct)});
    out.textContent = 'plaintext (before encryption):\\n' + plaintext +
      '\\n\\nX-Enc-IV: ' + b64(iv) + '\\n\\nserver response:\\n' + await res.text();
  } catch (e) { out.textContent = 'error: ' + e; }
}
document.getElementById('go').addEventListener('click', send);
</script></body>"""


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        # One concise line per request.
        sys.stderr.write("%s  %s\n" % (self.command, self.path))

    def _send(self, code, ctype, body, extra=None):
        if isinstance(body, str):
            body = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self):
        n = int(self.headers.get("Content-Length", 0) or 0)
        return self.rfile.read(n) if n else b""

    def _echo(self, body=b""):
        lines = ["%s %s %s" % (self.command, self.path, self.request_version)]
        for k, v in self.headers.items():
            lines.append("%s: %s" % (k, v))
        text = "\n".join(lines) + "\n\n" + body.decode("utf-8", "replace")
        self._send(200, "text/plain; charset=utf-8", text)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path == "/":
            return self._send(200, "text/html; charset=utf-8", INDEX)
        if path == "/test-enc":
            return self._send(200, "text/html; charset=utf-8", PAGE)
        if path == "/secret":
            plain = b'{"flag":"NGINTIP{extension_decrypt_works}","note":"decrypt me"}'
            return self._send(200, "application/octet-stream", base64.b64encode(xor(plain)), {"X-Encrypted": "xor-b64"})
        if path.startswith("/status/"):
            part = path.split("/")[2] if len(path.split("/")) > 2 else "200"
            code = int(part) if part.isdigit() else 200
            return self._send(code, "text/plain; charset=utf-8", "status %d\n" % code)
        return self._echo()

    def do_POST(self):
        path = self.path.split("?", 1)[0]
        body = self._read_body()
        if path == "/test-enc":
            txt = body.decode("utf-8", "replace")
            looks_json = txt.lstrip()[:1] in ("{", "[")
            out = json.dumps({
                "receivedIvHeader": self.headers.get("X-Enc-IV"),
                "receivedBody": txt,
                "bodyBytes": len(body),
                "looksDecrypted": looks_json,
                "note": "plaintext JSON here means an extension decrypted the request on the wire",
            }, indent=2)
            return self._send(200, "application/json", out)
        if path == "/secret":
            try:
                dec = xor(base64.b64decode(body)).decode("utf-8", "replace")
            except Exception:
                dec = "<decrypt failed: not our ciphertext>"
            out = json.dumps({"youSent": dec, "note": "decrypted by the test server"}, indent=2)
            return self._send(200, "application/octet-stream", base64.b64encode(xor(out.encode())), {"X-Encrypted": "xor-b64"})
        return self._echo(body)

    do_PUT = do_POST
    do_DELETE = do_POST
    do_PATCH = do_POST


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8081
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print("test server on http://127.0.0.1:%d" % port)
    print("  GET /test-enc  GET/POST /secret  /echo  /json  /status/NNN")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
