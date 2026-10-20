// Small helpers for editing a raw HTTP message (request or response) as text.
// Editors only produce "\n"; these keep that convention and let the backend
// restore CRLF and recompute Content-Length on send.

const enc = new TextEncoder();

/** UTF-8 byte length (what Content-Length should be). */
export const byteLength = (s) => enc.encode(s).length;

/** Split into head (start line + headers) and body at the first blank line. */
export function splitMsg(raw = '') {
  const n = raw.replace(/\r\n/g, '\n');
  const i = n.indexOf('\n\n');
  if (i === -1) return { head: n.replace(/\n+$/, ''), body: '', hadBody: false };
  return { head: n.slice(0, i), body: n.slice(i + 2), hadBody: true };
}

function join(head, body, hadBody) {
  return hadBody || body ? `${head}\n\n${body}` : `${head}\n\n`;
}

/** True when the message has a non-empty body. */
export function hasBody(raw) {
  return splitMsg(raw).body.length > 0;
}

/** Append a header line to the header block (before the body). */
export function addHeader(raw, name = 'X-New-Header', value = 'value') {
  const { head, body, hadBody } = splitMsg(raw);
  return join(`${head}\n${name}: ${value}`, body, hadBody);
}

/** The current value of a header (case-insensitive), or '' when absent. */
export function getHeader(raw, name) {
  const { head } = splitMsg(raw);
  const re = new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*:(.*)$`, 'i');
  for (const line of head.split('\n').slice(1)) {
    const m = line.match(re);
    if (m) return m[1].trim();
  }
  return '';
}

/**
 * Add a header, or replace its value if it already exists (case-insensitive).
 * The start line (index 0) is never touched. Powers the smart "+ Header" menu.
 */
export function setHeader(raw, name, value) {
  const { head, body, hadBody } = splitMsg(raw);
  const lines = head.split('\n');
  const re = new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*:`, 'i');
  let found = false;
  const out = lines.map((l, i) => {
    if (i === 0) return l;
    if (re.test(l)) {
      found = true;
      return `${name}: ${value}`;
    }
    return l;
  });
  if (!found) out.push(`${name}: ${value}`);
  return join(out.join('\n'), body, hadBody);
}

/** Give the message a body (default placeholder) and a matching Content-Length. */
export function addBody(raw, body = 'body') {
  const { head } = splitMsg(raw);
  return `${setContentLength(head, byteLength(body))}\n\n${body}`;
}

/**
 * Replace the body with `body`, set a matching Content-Length, and (when given)
 * set Content-Type. Used by the Repeater "+ Body" skeleton menu.
 */
export function setBody(raw, body, contentType) {
  const out = addBody(raw, body);
  return contentType ? setHeader(out, 'Content-Type', contentType) : out;
}

/** Set (or add) Content-Length to match the current body's byte length. */
export function fixContentLength(raw) {
  const { head, body, hadBody } = splitMsg(raw);
  return join(setContentLength(head, byteLength(body)), body, hadBody);
}

// Replace an existing Content-Length header (case-insensitive) or add one.
function setContentLength(head, len) {
  const lines = head.split('\n');
  let found = false;
  const out = lines.map((l, i) => {
    if (i === 0) return l; // request/status line
    if (/^content-length\s*:/i.test(l)) {
      found = true;
      return `Content-Length: ${len}`;
    }
    return l;
  });
  if (!found) out.push(`Content-Length: ${len}`);
  return out.join('\n');
}
