# Ngintip 0.2.0

Everything that changed since 0.1.0.

This is a feature release, so the minor version moves: the extension system gained a
whole new capability, and Intruder and the Organizer grew around it.

The headline is extensions: they are no longer only able to rewrite traffic on the
wire. A new decode/encode pass lets an extension show an encrypted body as readable
plaintext, let you edit that plaintext, and re-encrypt it on send, without ever lying
about what actually crossed the wire. Intruder gained the same support, plus export
and much deeper Organizer integration.

---

## Extensions

**Three kinds of hook.** An extension can now define up to six hooks:

| Hook | What it does |
| --- | --- |
| `process_request` / `process_response` | Change the message **on the wire** (what is really sent / received). |
| `decode_request` / `decode_response` | Produce the **readable** form shown in the UI. Display only: the wire is untouched. |
| `encode_request` / `encode_response` | Turn the edited readable form **back into the wire form** on send. |

This is what makes an encrypt/decrypt extension possible: define `decode_*` + `encode_*`
and no `process_*`, and you read and edit plaintext while the app keeps speaking
ciphertext. History keeps showing the real bytes.

- **Hook context.** Hooks receive an optional `ctx` carrying `host`, `path`, `flow_id`
  and `edited`, so a hook can scope itself to one host or behave differently for an
  edited message. The request host is threaded into response hooks too, even though a
  response carries no `Host` header.
- **Only the hooks you wrote are called.** A script is scanned at worker start for the
  hooks it defines, so a process-only extension pays nothing for decode/encode.
- **Scope now has four surfaces:** `proxy`, `interceptor`, `repeater` and the new
  `intruder`. `all` / `both` / empty still mean every surface. An extension only costs
  a Python round trip on the surfaces it is scoped to.
- **The shipped examples were replaced.** `examples/template_extension.py` was
  rewritten around the three kinds of hook (with a warning not to use `process_*` to
  decrypt for reading), and `examples/encrypt_decrypt_extension.py` is a working
  encrypt/decrypt extension that **learns from traffic** which endpoints are encrypted,
  so an app that mixes encrypted and plain endpoints keeps working. The two old
  `process_*`-era examples were removed.

## Intruder

- **Extensions apply to attacks.** Attack requests go through a dedicated
  `intruder_send` path, so an extension scoped to `intruder` transforms every request
  it sends and decodes every response it gets back.
- **Send to Intruder seeds the readable request.** When a decode hook produced a
  plaintext view, that is what lands in the template, so payload positions are marked
  on readable text and each generated request is re-encrypted on send.
- **Results keep both forms.** A row stores the request actually sent and the response
  as it arrived, so the result detail offers a Wire / Original / Extension switcher.
- **Export** (toolbar, enabled once a run has results):
  - Export as JSON
  - Export as XML
  - Save to Organizer (new collection)
- **Save to Organizer**, three ways:
  - one result, tagged `intruder(each)`
  - the whole run as a single record, tagged `intruder(all)`, whose note says which tab
    it came from
  - every result into a brand new collection named `<tab> intruder`, which is **not**
    made the active collection and which follows the tab when you rename it
- **Rename a tab** by double-clicking it or from its right-click menu.
- **Right-click menus** on the request template, result rows, the result request /
  response panes, and the empty area of the results table (which previously fell
  through to the webview's Reload / Inspect menu).
- **Arrow keys keep browsing results** after you click into the detail panes. The panes
  are not focusable, so focus fell back to the page and the old list-only key handler
  stopped seeing the keys.
- **Raw mode, per tab.** A `Raw` switch (Repeater and Intruder) skips extensions for
  that tab entirely: the request leaves exactly as typed and the response is shown
  exactly as it arrived, with no decode and no re-encode. The tab carries a `RAW` badge
  while it is on, the switch only appears when an extension actually covers that
  surface, and it is deliberately not persisted, so a forgotten bypass cannot outlive
  the session. It also removes the per-request Python round trip from a bulk run.

## Organizer

- **Saved requests keep their variants.** Saving from History, Repeater or Intruder now
  stores the extension and original forms alongside the wire message, so the saved
  detail shows the same Wire / Extension / Original switcher the live views do.
- **A saved Intruder run opens as a table.** Instead of one giant concatenated message,
  the run is stored row by row and rendered like Intruder's own results list; picking a
  row drives the request and response panes.
- **Search inside a saved run** by payload, status, row number or error.
- **Resizable columns** on both the saved requests table and the saved run table: drag
  a header grip, double-click it to reset. Widths are remembered per machine.
- **Right-click menus on the Request / Response panes** (copy raw / body / URL / cURL,
  send to Repeater, Intruder, Decoder, Comparer).
- **Highlight to decode:** selecting decodable text opens the decoder dock, as in
  History and Repeater.
- Fixed a layout bug where the request / response panes did not fill the available
  width, leaving dead space on the right.
- `Ctrl/Cmd+R` sends the selected saved request to Repeater.

## Repeater

- The first "Send to Repeater" now fills the lone untouched default tab in place
  instead of opening a second tab next to an empty one.
- The decoder dock floats over the right edge instead of taking a grid column, so
  opening or closing it no longer reflows the editor and misplaces the caret.

## Intercept

- Request and response bodies are beautified for reading as soon as a message is
  selected. Reformatting is **not** treated as an edit: forwarding an unedited message
  sends the exact original wire bytes, and no "edited" badge appears.

## History and the message viewer

- A Wire / Extension / Original switcher appears when a flow was edited in Intercept or
  an extension produced a readable view. **Wire stays the default**, so a security tool
  never hides what actually crossed the wire behind a decode.

## App and project

- **Version is visible** in Settings > Project, together with a check against the latest
  published GitHub release and a link to the releases page.
- **One place to bump the version.** The UI reads the version straight from
  `package.json`, and `tauri.conf.json` no longer pins its own copy (it inherits from
  `src-tauri/Cargo.toml`). Releasing means editing those two files only.
- **Reload shortcuts are swallowed inside the desktop app.** `Ctrl/Cmd+R`,
  `Ctrl+Shift+R` and `F5` used to reload the webview, throwing away all in-memory state
  and dropping you back on the project chooser, which looked like a crash. A plain dev
  browser keeps its normal reload.
- **Rebrand cleanup.** The last "cegat" references are gone: CA name, MCP server, config
  comments, run scripts, `.gitignore` and test fixtures all say Ngintip now.

## Tests

- 300 frontend tests, 137 Rust tests.
- New end-to-end coverage for the extension pipeline: interceptor decode plus re-encode
  on edit, a display-only decode extension that never changes the wire, and Repeater
  sending encrypted while showing decrypted.

---

## Upgrading from 0.1.0

One behaviour change to be aware of: **an extension scoped `all` (the default) now also
runs on Intruder**, because `intruder` is a new surface and `all` means every surface.
If you have such an extension, every attack request is transformed and pays a Python
round trip. That is usually what you want; when it is not, untick `intruder` in the
extension's scope, or flip the new per-tab `Raw` switch.

Extensions written for 0.1.0 keep working: `decode_*` and `encode_*` are additive, and
a script that only defines `process_*` behaves exactly as before.
