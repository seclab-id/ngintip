#!/usr/bin/env python3
"""Ngintip extension runner + HTTP message API.

Loads a user extension script and dispatches transform hooks over a
newline-delimited JSON protocol on stdin/stdout:

    <- {"hook": "request"|"response", "data": "<base64 raw message>"}
    -> {"ok": true,  "data": "<base64 transformed message>"}
    -> {"ok": false, "error": "<message>"}

A script defines any of these hooks, each receiving an editable message object
and an optional context (see Ctx below):

    # change the message on the wire (runs once per flow; side effects OK):
    def process_request(req, ctx=None):   ...
    def process_response(res, ctx=None):  ...

    # display only: wire -> readable, NEVER changes what is sent. Used to show a
    # decrypted/decoded body in the UI while the real encrypted bytes stay the wire:
    def decode_request(req, ctx=None):    ...
    def decode_response(res, ctx=None):   ...

    # on send: readable -> wire. Runs only when an edited (or composed) message is
    # sent (Repeater send, Intercept forward), to re-encrypt/re-encode it:
    def encode_request(req, ctx=None):    ...
    def encode_response(res, ctx=None):   ...

The second `ctx` argument is optional: a one-arg hook (e.g. `process_request(req)`)
still works. The hook may mutate the message in place and return None
(recommended), or return a new HttpMessage / bytes / str. Returning None with no
changes is a no-op (the message passes through unchanged). Anything printed goes
to stderr (the Debug console), so print() never corrupts the protocol on stdout.

The message API (see also examples/template_extension.py):

    req.method / req.path / req.version         # request line (get + set)
    res.status / res.reason / res.version        # status line (get + set)
    req.host                                      # Host header shortcut
    req.header(name, default=None)               # one header, case-insensitive
    req.headers.get / set / remove / get_all / items() / dict()
    req.body        # bytes (get + set)
    req.text        # str  (get + set)
    req.json()      # parse JSON body ; req.set_json(obj)
    req.form()      # parse x-www-form-urlencoded ; req.set_form(dict)
    req.query()     # parse the path's query string ; req.set_query(dict)
    req.raw         # the whole message as bytes (get + set)

Content-Length is recomputed automatically when the message is serialized.
"""

import sys
import json
import base64
import inspect
import importlib.util
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

# Keep the protocol channel clean: accidental prints go to stderr.
_real_stdout = sys.stdout
sys.stdout = sys.stderr


def _split(raw):
    for sep in (b"\r\n\r\n", b"\n\n"):
        i = raw.find(sep)
        if i != -1:
            return raw[:i], raw[i + len(sep):]
    return raw.rstrip(b"\r\n"), b""


class Headers:
    """Ordered, case-insensitive HTTP headers."""

    def __init__(self, lines=None):
        self._items = []  # list of [name, value]
        for ln in lines or []:
            ln = ln.rstrip("\r")
            if ":" in ln:
                k, v = ln.split(":", 1)
                self._items.append([k.strip(), v.strip()])

    def get(self, name, default=None):
        nl = name.lower()
        for k, v in self._items:
            if k.lower() == nl:
                return v
        return default

    def get_all(self, name):
        nl = name.lower()
        return [v for k, v in self._items if k.lower() == nl]

    def set(self, name, value):
        """Set a header, replacing any existing ones with that name."""
        nl = name.lower()
        value = str(value)
        replaced = False
        out = []
        for k, v in self._items:
            if k.lower() == nl:
                if not replaced:
                    out.append([name, value])
                    replaced = True
                # drop duplicates
            else:
                out.append([k, v])
        if not replaced:
            out.append([name, value])
        self._items = out

    def add(self, name, value):
        self._items.append([name, str(value)])

    def remove(self, name):
        nl = name.lower()
        self._items = [it for it in self._items if it[0].lower() != nl]

    def items(self):
        return [(k, v) for k, v in self._items]

    def dict(self):
        return {k: v for k, v in self._items}

    def __contains__(self, name):
        return self.get(name) is not None

    def __getitem__(self, name):
        return self.get(name)

    def __setitem__(self, name, value):
        self.set(name, value)

    def __delitem__(self, name):
        self.remove(name)


class HttpMessage:
    def __init__(self, raw, is_request):
        if isinstance(raw, str):
            raw = raw.encode("utf-8")
        head, body = _split(raw)
        lines = head.decode("latin-1").split("\n")
        start = lines[0].strip() if lines else ""
        self.is_request = is_request
        self.headers = Headers(lines[1:])
        self._body = body
        parts = start.split(" ")
        if is_request:
            self.method = parts[0] if parts else "GET"
            self.path = parts[1] if len(parts) > 1 else "/"
            self.version = parts[2] if len(parts) > 2 else "HTTP/1.1"
        else:
            self.version = parts[0] if parts else "HTTP/1.1"
            try:
                self.status = int(parts[1]) if len(parts) > 1 else 0
            except ValueError:
                self.status = 0
            self.reason = " ".join(parts[2:]) if len(parts) > 2 else ""

    # ---- Host shortcut ----
    @property
    def host(self):
        return self.headers.get("Host")

    @host.setter
    def host(self, value):
        self.headers.set("Host", value)

    # ---- headers shortcuts ----
    def header(self, name, default=None):
        return self.headers.get(name, default)

    def set_header(self, name, value):
        self.headers.set(name, value)

    def remove_header(self, name):
        self.headers.remove(name)

    # ---- body ----
    @property
    def body(self):
        return self._body

    @body.setter
    def body(self, value):
        self._body = value.encode("utf-8") if isinstance(value, str) else bytes(value)

    @property
    def text(self):
        return self._body.decode("utf-8", "replace")

    @text.setter
    def text(self, value):
        self._body = value.encode("utf-8")

    def json(self):
        return json.loads(self._body.decode("utf-8") or "null")

    def set_json(self, obj):
        self._body = json.dumps(obj).encode("utf-8")
        self.headers.set("Content-Type", "application/json")

    def form(self):
        return dict(parse_qsl(self.text, keep_blank_values=True))

    def set_form(self, data):
        self._body = urlencode(data).encode("utf-8")
        self.headers.set("Content-Type", "application/x-www-form-urlencoded")

    # ---- request path query ----
    def query(self):
        q = urlsplit(self.path).query if self.is_request else ""
        return dict(parse_qsl(q, keep_blank_values=True))

    def set_query(self, data):
        if not self.is_request:
            return
        s = urlsplit(self.path)
        self.path = urlunsplit(("", "", s.path, urlencode(data), s.fragment))

    # ---- whole message ----
    @property
    def raw(self):
        return self.to_bytes()

    @raw.setter
    def raw(self, value):
        other = HttpMessage(value, self.is_request)
        self.__dict__.update(other.__dict__)

    def to_bytes(self):
        if self.is_request:
            start = "%s %s %s" % (self.method, self.path, self.version)
        else:
            start = ("%s %d %s" % (self.version, self.status, self.reason)).rstrip()
        # Keep Content-Length correct after edits (only when a body is present or a
        # Content-Length was already there, so bodyless messages stay bodyless).
        if self._body or self.headers.get("Content-Length") is not None:
            self.headers.set("Content-Length", str(len(self._body)))
        head = start + "\r\n" + "".join("%s: %s\r\n" % (k, v) for k, v in self.headers.items())
        return head.encode("latin-1", "replace") + b"\r\n" + self._body


# State shared across every hook call in this long-lived worker, so an extension
# can carry data between flows (e.g. a session key harvested from a login response
# and reused to decrypt later flows). Survives for the worker's lifetime only.
_STATE = {}


class Ctx:
    """The optional second argument to a hook. Read-only facts about where this
    message is being handled, plus `state` (persistent dict) and `log()`."""

    def __init__(self, data):
        data = data or {}
        # "proxy" | "interceptor" | "repeater" | "intruder" | "test"
        self.surface = data.get("surface", "")
        self.host = data.get("host", "")
        self.path = data.get("path", "")
        self.flow_id = data.get("flowId")
        # True when the human actually changed the message before this send (encode
        # hooks); False for display (decode) and untouched pass-through.
        self.edited = bool(data.get("edited", False))
        self.state = _STATE

    def log(self, *args):
        print(*args, file=sys.stderr)


# Message "hook" name -> (python function name, is_request). The legacy
# "request"/"response" names map to process_* so older hosts keep working.
_HOOKS = {
    "request": ("process_request", True),
    "response": ("process_response", False),
    "process_request": ("process_request", True),
    "process_response": ("process_response", False),
    "decode_request": ("decode_request", True),
    "decode_response": ("decode_response", False),
    "encode_request": ("encode_request", True),
    "encode_response": ("encode_response", False),
}


def _call(fn, msg, ctx):
    """Call a hook with (msg, ctx) if it accepts a second argument, else (msg)."""
    try:
        nparams = len(inspect.signature(fn).parameters)
    except (TypeError, ValueError):
        nparams = 1
    return fn(msg, ctx) if nparams >= 2 else fn(msg)


def _load(path):
    spec = importlib.util.spec_from_file_location("ngintip_ext", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _result_bytes(result, msg):
    if result is None:
        return msg.to_bytes()
    if isinstance(result, HttpMessage):
        return result.to_bytes()
    if isinstance(result, str):
        return result.encode("utf-8")
    if isinstance(result, (bytes, bytearray)):
        return bytes(result)
    raise TypeError("hook must return None, an HttpMessage, bytes, or str")


def main():
    if len(sys.argv) < 2:
        print("usage: runner.py <script.py>", file=sys.stderr)
        return 2
    try:
        module = _load(sys.argv[1])
    except Exception as exc:  # import/syntax error
        print("failed to load extension: %r" % (exc,), file=sys.stderr)
        return 1

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            envelope = json.loads(line)
            raw = base64.b64decode(envelope["data"])
            func_name, is_request = _HOOKS.get(envelope.get("hook"), (None, True))
            fn = getattr(module, func_name, None) if func_name else None
            if fn is None:
                # Script does not define this hook: pass through unchanged.
                out = raw
            else:
                msg = HttpMessage(raw, is_request)
                ctx = Ctx(envelope.get("ctx"))
                out = _result_bytes(_call(fn, msg, ctx), msg)
            reply = {"ok": True, "data": base64.b64encode(out).decode("ascii")}
        except Exception as exc:
            reply = {"ok": False, "error": "%s: %s" % (type(exc).__name__, exc)}
        _real_stdout.write(json.dumps(reply) + "\n")
        _real_stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
