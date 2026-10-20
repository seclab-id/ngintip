# Sample Ngintip extension for the built-in test server's /secret endpoint.
#
# The test server "encrypts" bodies with XOR (key "ngintip-demo-key") then base64,
# and sends the header X-Encrypted: xor-b64. This extension decrypts responses so you
# can read them in History/Repeater; the original ciphertext stays under Original.
#
# A real target would use AES/RSA (see aes_decrypt_extension.py); the shape is the same.

import base64

KEY = b"ngintip-demo-key"


def _xor(data):
    return bytes(b ^ KEY[i % len(KEY)] for i, b in enumerate(data))


def process_response(res):
    if not res.body:
        return None
    try:
        res.body = _xor(base64.b64decode(res.body))
        print("decrypted %d bytes" % len(res.body))
    except Exception as exc:  # not our ciphertext: leave it alone
        print("skip decrypt:", exc)
    return None


def process_request(req):
    # Encrypt a plaintext body you write in Repeater before it goes to the server.
    if not req.body:
        return None
    req.body = base64.b64encode(_xor(req.body))
    print("encrypted request body")
    return None
