# AES decrypt extension for the test server's /test-enc page.
#
# The page AES-CBC encrypts a JSON body in the browser with the 16-byte key
# "ngintip-demo-key" (AES-128) and a random IV sent in the X-Enc-IV header. This
# decrypts the request on the wire so History/Repeater show the plaintext, while the
# original ciphertext is kept under the request's "Original" tab.
#
# Requires pycryptodome: in the Extensions view, add "pycryptodome" as a venv package
# and install it, then enable this extension.

import base64
from Crypto.Cipher import AES
from Crypto.Util.Padding import unpad

KEY = b"ngintip-demo-key"  # 16 bytes -> AES-128


def process_request(req):
    iv_b64 = req.header("X-Enc-IV")
    if not iv_b64 or not req.body.strip():
        return None
    try:
        print("Original body: ", req.body)
        iv = base64.b64decode(iv_b64)
        ct = base64.b64decode(req.body.strip())
        req.set_header("X-Ngintip", "set-spesific-headers")
        req.body = unpad(AES.new(KEY, AES.MODE_CBC, iv).decrypt(ct), AES.block_size)
    except Exception as exc:  # not our ciphertext / wrong key: leave it untouched
        print("AES decrypt failed:", exc)
        return None
    print("decrypted request body (%d bytes)" % len(req.body))
    return None
