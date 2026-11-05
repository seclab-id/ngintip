# Ngintip extension template.
#
# Fill in the hooks you need and delete the rest. As written this file is a safe
# no-op: enabling it changes nothing until you add code. Each hook receives an
# editable HttpMessage (and an optional `ctx`); mutate it in place and return None
# (recommended), or return a new HttpMessage / bytes / str. Content-Length is
# recomputed for you on serialize. print() goes to the Extensions Debug console.
#
# THREE KINDS OF HOOK
#
#   process_request(req[, ctx])  / process_response(res[, ctx])
#       Change the message ON THE WIRE (what is actually sent / received). Use it
#       only to alter live traffic (strip a header, flip a param). Do NOT use it to
#       decrypt a body just for reading: that would change the real wire, break the
#       app that expects ciphertext, and make History show something other than what
#       actually crossed the wire.
#
#   decode_request(req[, ctx])   / decode_response(res[, ctx])
#       DISPLAY ONLY: turn the wire form into a readable form for the UI (e.g.
#       decrypt a body). NEVER changes what crosses the wire; the readable text
#       shows as the "Extension" version in History, next to "Wire".
#
#   encode_request(req[, ctx])   / encode_response(res[, ctx])
#       ON SEND: turn the edited readable form back into the wire form (e.g.
#       re-encrypt) when you send from Repeater or forward an intercepted message.
#
# Which do I pick? To READ or EDIT an encrypted body, use decode_* (read) and
# encode_* (send) and leave process_* alone. To actually rewrite the live traffic,
# use process_*. For a fully encrypted endpoint the pattern is: decode_* to read,
# encode_* to send (see the ready-to-fill block at the bottom).
#
# You only define the hooks you need: an undefined hook is skipped and the message
# passes through untouched, so you pay nothing for the stages you leave out. (That
# is why an encrypt/decrypt extension defines decode_*/encode_* and no process_*.)
#
# THE MESSAGE API (req and res share it)
#   req.method / req.path / req.host / req.version      request line (get + set)
#   res.status / res.reason / res.version               status line (get + set)
#   m.header(name[, default]) / m.set_header / m.remove_header
#   m.headers.get_all(name) / m.headers.items() / m.headers.dict()
#   m.body (bytes) / m.text (str)                        get + set
#   m.json() ; m.set_json(obj)                           JSON body
#   m.form() ; m.set_form(dict)                          form body
#   m.query() ; m.set_query(dict)                        request ?query
#   m.raw                                                whole message bytes (get + set)
#
# THE ctx ARGUMENT (optional second parameter)
#   ctx.surface   "proxy" | "interceptor" | "repeater" | "intruder"
#   ctx.host / ctx.path / ctx.flow_id
#   ctx.edited    True on a send you actually changed (encode hooks)
#   ctx.state     a dict that persists across calls and flows (stash a session key)
#   ctx.log(...)  print to the Debug console


def process_request(req, ctx=None):
    """Change the outgoing request on the wire. Return None to leave it unchanged."""
    # Example (uncomment): tag every request so you can spot it in History.
    # req.set_header("X-Ngintip", "template")
    return None


def process_response(res, ctx=None):
    """Change the response on the wire before it reaches the client."""
    # Example (uncomment): drop a header on the way back.
    # res.remove_header("Content-Security-Policy")
    return None


# ----------------------------------------------------------------------------------
# Fully encrypted endpoint: read it in the UI, edit it, send it re-encrypted.
# Fill in decrypt()/encrypt() for your scheme, then uncomment the hooks you need.
# `decode_*` is display only (the wire stays encrypted); `encode_*` runs on send.
#
# def decrypt(raw_bytes):
#     ...   # wire ciphertext -> plaintext bytes
#
# def encrypt(raw_bytes):
#     ...   # plaintext bytes -> wire ciphertext
#
# def decode_request(req, ctx=None):
#     if req.body:
#         req.body = decrypt(req.body)      # shown as "Extension" in History
#
# def decode_response(res, ctx=None):
#     if res.body:
#         res.body = decrypt(res.body)
#
# def encode_request(req, ctx=None):
#     if req.body:
#         req.body = encrypt(req.body)      # re-encrypt the edited body on send
#
# def encode_response(res, ctx=None):
#     if res.body:
#         res.body = encrypt(res.body)
