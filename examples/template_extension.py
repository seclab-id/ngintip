# Ngintip extension template.
#
# Define either or both hooks. Each receives an editable HttpMessage; mutate it in
# place and return None (recommended), or return a new HttpMessage / bytes / str.
# Content-Length is recomputed for you when the message is serialized.
#
# Anything you print() shows up in the Extensions Debug console (it goes to stderr,
# never to the protocol channel).


def process_request(req):
    """Called for each request (proxy and/or Repeater, per this extension's scope)."""

    # --- request line ---
    # req.method   e.g. "GET" / "POST"      (get + set)
    # req.path     e.g. "/api/login?x=1"    (get + set)
    # req.host     the Host header          (get + set)
    # req.version  e.g. "HTTP/1.1"          (get + set)

    # --- headers (case-insensitive) ---
    # req.header("Authorization")           -> value or None
    # req.set_header("X-Trace", "1")        -> add or replace
    # req.remove_header("Cookie")
    # req.headers.get_all("Set-Cookie")     -> list
    # req.headers.items()                   -> [(name, value), ...]
    # req.headers.dict()                    -> {name: value}

    # --- body ---
    # req.body       bytes   (get + set)
    # req.text       str     (get + set)
    # req.json()     parse JSON body  ; req.set_json({"a": 1})
    # req.form()     parse form body  ; req.set_form({"a": "1"})
    # req.query()    parse ?query     ; req.set_query({"page": "2"})

    # --- the whole message ---
    # req.raw        the entire request as bytes (get + set)

    # Example: tag every request and, if it is JSON, bump a field.
    req.set_header("X-Ngintip", "template")
    ctype = req.header("Content-Type", "")
    if "json" in ctype.lower():
        try:
            data = req.json()
            data["_seen"] = True
            req.set_json(data)
        except Exception as exc:
            print("not valid json:", exc)
    return None


def process_response(res):
    """Called for each response (per this extension's scope)."""

    # res.status / res.reason / res.version  (get + set)
    # res.header(...) / res.set_header(...) / res.remove_header(...)
    # res.body / res.text / res.json() / res.raw   (same as request)

    # Example: annotate the response so you can spot it in History.
    res.set_header("X-Ngintip", "seen")
    return None
