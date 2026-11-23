// WebSocket helpers for the Repeater: recognising a handshake, naming the socket behind
// it, and turning a captured frame back into something you can edit and send again.
//
// The last of those is the Repeater gesture applied to frames: click a message, change a
// field, send it back. The only awkward part is binary. A binary frame is stored as its bytes but
// SHOWN as a hex dump (see ws::display in Rust), so putting the displayed text into the
// composer would send the dump itself - offsets, ascii column and all. So a dump is
// read back into plain hex instead, which is what the composer's `binary` kind expects.

/** The exact shape ws::display writes: `00000000  de ad be ef …  ascii`. */
const DUMP_LINE = /^[0-9a-f]{8} {2}/;
// Columns, fixed by `format!("{:08x}  {:<47}  {ascii}")`: 8 offset, 2 spaces, 47 hex.
const HEX_FROM = 10;
const HEX_TO = 57;

export function looksLikeHexDump(text) {
  const first = String(text ?? '').split('\n', 1)[0];
  return DUMP_LINE.test(first);
}

/** The bytes of a hex dump, as spaced hex ("de ad be ef"). */
export function hexFromDump(text) {
  const out = [];
  for (const line of String(text ?? '').split('\n')) {
    if (!DUMP_LINE.test(line)) continue;
    for (const byte of line.slice(HEX_FROM, HEX_TO).trim().split(/\s+/)) {
      if (/^[0-9a-f]{2}$/.test(byte)) out.push(byte);
    }
  }
  return out.join(' ');
}

/** The UTF-8 bytes of `text`, as spaced hex. */
export function hexFromText(text) {
  return [...new TextEncoder().encode(String(text ?? ''))].map((b) => b.toString(16).padStart(2, '0')).join(' ');
}

/**
 * A stored message as composer fields, or null when the frame is not something you can
 * send again (a close frame is the connection ending, not a message).
 */
export function frameToCompose(msg) {
  if (!msg) return null;
  switch (msg.opcode) {
    case 1:
      return { kind: 'text', payload: msg.text ?? '' };
    case 2:
      return { kind: 'binary', payload: looksLikeHexDump(msg.text) ? hexFromDump(msg.text) : hexFromText(msg.text) };
    case 9:
      return { kind: 'ping', payload: msg.text ?? '' };
    case 10:
      return { kind: 'pong', payload: msg.text ?? '' };
    default:
      return null;
  }
}

/**
 * Whether what is shown is the whole frame. A long message is stored and displayed
 * truncated, so resending it would send something shorter than what arrived, and the
 * button has to say so rather than quietly lie.
 */
export function isWholeFrame(msg) {
  if (!msg) return false;
  if (msg.opcode === 2) {
    const shown = looksLikeHexDump(msg.text) ? hexFromDump(msg.text).split(' ').filter(Boolean).length : new TextEncoder().encode(msg.text ?? '').length;
    return shown >= Number(msg.length ?? 0);
  }
  return new TextEncoder().encode(msg.text ?? '').length >= Number(msg.length ?? 0);
}

// ---- the handshake, and the socket behind it ----------------------------------------
//
// A WebSocket starts life as an ordinary GET answered with 101, which is why a socket
// can be sent to the Repeater by accident: it looks like a request, it IS a request,
// and it is the one request that cannot be replayed. The 101 is spent the moment it is
// answered, so what the operator means is the socket at the same address.

/** True when the request head asks for the WebSocket upgrade. */
export function isHandshakeRequest(raw) {
  // The head only: a body quoting "Upgrade: websocket" is a body, not a handshake.
  const head = String(raw ?? '').split(/\r?\n\r?\n/, 1)[0];
  // `\r?` because CRLF is the wire's line ending and `$` only sees the \n.
  return /^[ \t]*upgrade[ \t]*:[ \t]*websocket[ \t]*\r?$/im.test(head);
}

/** True when an address can only be a socket, so the HTTP client cannot send it. */
export function isSocketUrl(url) {
  return /^wss?:\/\//i.test(String(url ?? '').trim());
}

/** The request-line path of a raw request ("GET /ws/x?y=1 HTTP/1.1" -> "/ws/x?y=1"). */
export function requestPath(raw) {
  const target = String(raw ?? '').split('\n', 1)[0].trim().split(/\s+/)[1] ?? '';
  return target.startsWith('/') ? target : '/';
}

/** The socket address of a flow: ws:// or wss://, never http(s). */
export function socketAddress(scheme, host, path = '/') {
  const secure = /^(https|wss)$/i.test(String(scheme ?? ''));
  return `${secure ? 'wss' : 'ws'}://${host}${path || '/'}`;
}
