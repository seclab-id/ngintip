# Encrypt / decrypt extension that LEARNS which endpoints are encrypted.
#
# Read an encrypted body as plaintext, edit it, and have it leave re-encrypted. The
# wire is never changed behind your back: decode_* is display only, encode_* runs on
# send. There is no process_* here, which is the whole point.
#
# Why no endpoint list: in a real engagement you do not know every endpoint up front,
# and apps are rarely uniform - some endpoints are encrypted, some are plain JSON. So
# this works it out by WATCHING the traffic:
#
#   - decode_* tries to decrypt every body. If it decrypts, that (direction, path) is
#     an encrypted endpoint, so we remember it. If it does not, the body was plain, so
#     we leave it alone and remember nothing.
#   - encode_* re-encrypts ONLY a (direction, path) we have already seen decrypt. An
#     endpoint we have never seen encrypted is sent exactly as typed, so the plain
#     ones keep working.
#
# That is reliable because each extension runs as ONE long-lived worker, so `ctx.state`
# is shared across every flow and surface (History, Repeater, Intercept, Intruder). In
# practice you always see an endpoint in History first (that is how it reaches
# Repeater), so by the time you resend it, decode_* has already taught encode_*.
#
# Swap SECRET/HOSTS and the decrypt()/encrypt() pair for your target's scheme. As
# written it matches this wire format:
#     base64( IV(16) || AES-256-CBC( PKCS7(bytes) ) ),  key = SHA-256(SECRET)
#
# Needs pycryptodome in the extension venv:  pip install pycryptodome
import base64
import hashlib
import os

from Crypto.Cipher import AES
from Crypto.Util.Padding import pad, unpad

SECRET = "ngintip-lab-secret-key"
KEY = hashlib.sha256(SECRET.encode()).digest()  # 32 bytes, AES-256

# Only touch these hosts; everything else passes through untouched. In a real
# engagement this is the target host(s), not an endpoint list.
HOSTS = ("127.0.0.1", "localhost")


def decrypt(raw_bytes):
    """Wire ciphertext (base64 of IV || AES-CBC) -> plaintext bytes. Raises on plain."""
    data = base64.b64decode(raw_bytes, validate=True)
    iv, ct = data[:16], data[16:]
    return unpad(AES.new(KEY, AES.MODE_CBC, iv).decrypt(ct), 16)


def encrypt(raw_bytes):
    """Plaintext bytes -> wire ciphertext (base64 of IV || AES-CBC)."""
    iv = os.urandom(16)
    return base64.b64encode(iv + AES.new(KEY, AES.MODE_CBC, iv).encrypt(pad(raw_bytes, 16)))


def _host(msg, ctx):
    return (ctx.host if ctx and ctx.host else "") or msg.header("Host") or ""


def _in_scope(host):
    return any(h in host for h in HOSTS)


def _norm(path):
    return (path or "").split("?", 1)[0]


def _seen(ctx):
    # One worker per extension -> this set is shared across every flow and surface.
    if ctx is None:
        return set()
    return ctx.state.setdefault("encrypted", set())


def _looks_encrypted(body):
    try:
        decrypt(body)
        return True
    except Exception:
        return False


# ---- read: try to decrypt, and LEARN which endpoints are encrypted -----------------
def decode_request(req, ctx=None):
    host = _host(req, ctx)
    if not _in_scope(host) or not req.body:
        return
    try:
        plain = decrypt(req.body)
    except Exception:
        return  # a plain request: leave it, learn nothing
    req.body = plain                                  # shown as "Extension" in History
    _seen(ctx).add(("req", host, _norm(req.path)))


def decode_response(res, ctx=None):
    host = _host(res, ctx)
    if not _in_scope(host) or not res.body:
        return
    try:
        plain = decrypt(res.body)
    except Exception:
        return  # a plain response: leave it, learn nothing
    res.body = plain
    _seen(ctx).add(("res", host, _norm(ctx.path if ctx else "")))


# ---- send: re-encrypt ONLY endpoints we have seen encrypted ------------------------
def encode_request(req, ctx=None):
    host = _host(req, ctx)
    if not _in_scope(host) or not req.body:
        return
    if ("req", host, _norm(req.path)) in _seen(ctx) and not _looks_encrypted(req.body):
        req.body = encrypt(req.body)                  # re-encrypt the edited body on send


def encode_response(res, ctx=None):
    host = _host(res, ctx)
    if not _in_scope(host) or not res.body:
        return
    if ("res", host, _norm(ctx.path if ctx else "")) in _seen(ctx) and not _looks_encrypted(res.body):
        res.body = encrypt(res.body)
