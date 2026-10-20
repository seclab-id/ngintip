#!/usr/bin/env python3
"""AES-encrypt a JSON body and POST it to /test-enc, optionally through the proxy.

Same cipher as the /test-enc page and examples/aes_decrypt_extension.py:
AES-128-CBC, key "ngintip-demo-key", random IV sent in the X-Enc-IV header, body is
the base64 ciphertext.

Examples:
  # Direct to the test server (server just echoes the ciphertext):
  python3 scripts/aes_client.py

  # Through the app's proxy (with the AES decrypt extension on, History shows plaintext):
  python3 scripts/aes_client.py --proxy http://127.0.0.1:8080

  # Custom target / payload:
  python3 scripts/aes_client.py --url http://127.0.0.1:8081/test-enc \\
      --plaintext '{"user":"admin","action":"delete"}'

Requires pycryptodome:  python3 -m pip install --user pycryptodome
"""
import argparse
import base64
import os
import sys
import urllib.request

try:
    from Crypto.Cipher import AES
    from Crypto.Util.Padding import pad
except ImportError:
    sys.exit("pycryptodome not installed. Run: python3 -m pip install --user pycryptodome")

KEY = b"ngintip-demo-key"  # 16 bytes -> AES-128


def main():
    ap = argparse.ArgumentParser(description="AES-encrypt a body and POST it to /test-enc.")
    ap.add_argument("--url", default="http://127.0.0.1:8081/test-enc", help="target URL")
    ap.add_argument("--proxy", default="", help="HTTP proxy, e.g. http://127.0.0.1:8080")
    ap.add_argument("--plaintext", default='{"user":"admin","action":"transfer","amount":1000}')
    args = ap.parse_args()

    iv = os.urandom(16)
    ct = AES.new(KEY, AES.MODE_CBC, iv).encrypt(pad(args.plaintext.encode(), 16))
    body = base64.b64encode(ct)

    req = urllib.request.Request(
        args.url,
        data=body,
        method="POST",
        headers={"Content-Type": "application/octet-stream", "X-Enc-IV": base64.b64encode(iv).decode()},
    )
    opener = (
        urllib.request.build_opener(urllib.request.ProxyHandler({"http": args.proxy, "https": args.proxy}))
        if args.proxy
        else urllib.request.build_opener()
    )

    print("POST", args.url, ("(via proxy %s)" % args.proxy) if args.proxy else "(direct)")
    print("plaintext :", args.plaintext)
    print("X-Enc-IV  :", base64.b64encode(iv).decode())
    print("ciphertext:", body.decode())
    try:
        with opener.open(req, timeout=10) as r:
            print("\nresponse  :\n" + r.read().decode("utf-8", "replace"))
    except Exception as exc:
        sys.exit("\nrequest failed: %s" % exc)


if __name__ == "__main__":
    main()
