// Encoders/decoders/hashes for the Decoder view. Each operation takes a
// string and returns a string (hashes are async); invalid input throws.

const utf8 = new TextEncoder();
const strictUtf8 = new TextDecoder('utf-8', { fatal: true });

export const toBytes = (text) => utf8.encode(text);

export function toHex(bytes, sep = '') {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join(sep);
}

/** Text when the bytes are valid UTF-8, otherwise a hex string. */
export function bytesToDisplay(bytes) {
  try {
    return strictUtf8.decode(bytes);
  } catch {
    return toHex(bytes, ' ');
  }
}

export function base64Encode(text, { url = false } = {}) {
  let bin = '';
  for (const b of toBytes(text)) bin += String.fromCharCode(b);
  const b64 = btoa(bin);
  return url ? b64.replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '') : b64;
}

/** Accepts standard and URL-safe alphabets, missing padding and whitespace. */
export function base64DecodeBytes(text) {
  let s = text.replace(/\s+/g, '').replace(/-/g, '+').replace(/_/g, '/');
  if (!/^[A-Za-z0-9+/]*=*$/.test(s)) throw new Error('Not valid Base64');
  s = s.replace(/=+$/, '');
  if (s.length % 4 === 1) throw new Error('Invalid Base64 length');
  s += '='.repeat((4 - (s.length % 4)) % 4);
  return Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
}

export const base64Decode = (text) => bytesToDisplay(base64DecodeBytes(text));

/** `all` also encodes unreserved characters (every byte becomes %XX). */
export function urlEncode(text, { all = false } = {}) {
  if (!all) return encodeURIComponent(text);
  return Array.from(toBytes(text), (b) => `%${b.toString(16).toUpperCase().padStart(2, '0')}`).join('');
}

/** Lenient: malformed `%` sequences are left as they are. */
export function urlDecode(text) {
  const bytes = [];
  for (let i = 0; i < text.length; i++) {
    const hex = text.slice(i + 1, i + 3);
    if (text[i] === '%' && /^[0-9a-f]{2}$/i.test(hex)) {
      bytes.push(parseInt(hex, 16));
      i += 2;
    } else {
      bytes.push(...toBytes(text[i]));
    }
  }
  return bytesToDisplay(new Uint8Array(bytes));
}

const HTML_ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const HTML_NAMED = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' };

export const htmlEncode = (text) => text.replace(/[&<>"']/g, (c) => HTML_ESC[c]);

export function htmlDecode(text) {
  return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, ent) => {
    if (ent[0] === '#') {
      const code = ent[1] === 'x' || ent[1] === 'X' ? parseInt(ent.slice(2), 16) : parseInt(ent.slice(1), 10);
      return code <= 0x10ffff ? String.fromCodePoint(code) : m;
    }
    return HTML_NAMED[ent.toLowerCase()] ?? m;
  });
}

export const hexEncode = (text) => toHex(toBytes(text));

/** Accepts spaces, colons and a 0x prefix. */
export function hexDecode(text) {
  const s = text.replace(/0x/gi, '').replace(/[\s:,-]/g, '');
  if (!/^([0-9a-f]{2})*$/i.test(s)) throw new Error('Not valid hex');
  return bytesToDisplay(Uint8Array.from(s.match(/../g) ?? [], (h) => parseInt(h, 16)));
}

/** Header and payload pretty-printed; exp/iat/nbf shown as dates. The signature isn't verified. */
export function jwtDecode(token) {
  const parts = token.trim().split('.');
  if (parts.length < 2) throw new Error('A JWT must look like header.payload.signature');
  const json = (part, what) => {
    try {
      return JSON.parse(strictUtf8.decode(base64DecodeBytes(part)));
    } catch {
      throw new Error(`JWT ${what} is not valid Base64URL JSON`);
    }
  };
  const header = json(parts[0], 'Header');
  const payload = json(parts[1], 'Payload');
  const out = ['// Header', JSON.stringify(header, null, 2), '', '// Payload', JSON.stringify(payload, null, 2)];
  const times = ['iat', 'nbf', 'exp'].filter((k) => typeof payload[k] === 'number');
  if (times.length) {
    out.push('');
    for (const k of times) {
      const date = new Date(payload[k] * 1000);
      const note = k === 'exp' ? (date < new Date() ? ' (expired)' : ' (not yet expired)') : '';
      out.push(`// ${k}: ${date.toISOString()}${note}`);
    }
  }
  out.push('', '// Signature (not verified)', parts[2] || '(empty: alg none?)');
  return out.join('\n');
}

// MD5 isn't in WebCrypto. RFC 1321, working on 32-bit words.
const MD5_S = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
const MD5_K = Array.from({ length: 64 }, (_, i) => Math.floor(Math.abs(Math.sin(i + 1)) * 2 ** 32) >>> 0);

export function md5(text) {
  const bytes = toBytes(text);
  const len = bytes.length;
  const blocks = Math.floor((len + 8) / 64) + 1;
  const buf = new Uint8Array(blocks * 64);
  buf.set(bytes);
  buf[len] = 0x80;
  const view = new DataView(buf.buffer);
  view.setUint32(buf.length - 8, (len * 8) >>> 0, true);
  view.setUint32(buf.length - 4, Math.floor(len / 2 ** 29), true);

  let [a0, b0, c0, d0] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
  for (let block = 0; block < blocks; block++) {
    const M = Array.from({ length: 16 }, (_, w) => view.getUint32(block * 64 + w * 4, true));
    let [A, B, C, D] = [a0, b0, c0, d0];
    for (let i = 0; i < 64; i++) {
      let F, g;
      if (i < 16) [F, g] = [(B & C) | (~B & D), i];
      else if (i < 32) [F, g] = [(D & B) | (~D & C), (5 * i + 1) % 16];
      else if (i < 48) [F, g] = [B ^ C ^ D, (3 * i + 5) % 16];
      else [F, g] = [C ^ (B | ~D), (7 * i) % 16];
      F = (F + A + MD5_K[i] + M[g]) >>> 0;
      const s = MD5_S[(i >> 4) * 4 + (i & 3)];
      [A, D, C] = [D, C, B];
      B = (B + ((F << s) | (F >>> (32 - s)))) >>> 0;
    }
    [a0, b0, c0, d0] = [(a0 + A) >>> 0, (b0 + B) >>> 0, (c0 + C) >>> 0, (d0 + D) >>> 0];
  }
  const out = new DataView(new ArrayBuffer(16));
  [a0, b0, c0, d0].forEach((w, i) => out.setUint32(i * 4, w, true));
  return toHex(new Uint8Array(out.buffer));
}

/** SHA-1 / SHA-256 / SHA-384 / SHA-512 via WebCrypto. */
export async function sha(algorithm, text) {
  return toHex(new Uint8Array(await crypto.subtle.digest(algorithm, toBytes(text))));
}

/** Decode operations usable on a highlighted snippet, in auto-detect priority. */
export const DECODERS = [
  { id: 'jwt', label: 'JWT', run: jwtDecode },
  { id: 'urld', label: 'URL', run: urlDecode },
  { id: 'b64d', label: 'Base64', run: base64Decode },
  { id: 'hexd', label: 'Hex', run: hexDecode },
  { id: 'htmld', label: 'HTML', run: htmlDecode },
];

// Mostly-printable text (lets us reject binary garbage that "decoded" fine).
function printable(s) {
  if (!s) return false;
  let bad = 0;
  for (const ch of s) {
    const c = ch.codePointAt(0);
    if (c === 9 || c === 10 || c === 13) continue;
    if (c < 32 || c === 0xfffd) bad++;
  }
  return bad / s.length < 0.15;
}

/**
 * Try every decoder on `text` and return the ones that plausibly apply, best
 * first. Each result is `{ id, label, output }`. Used by the highlight-to-decode
 * mini decoder in the History/Repeater viewers.
 */
export function autoDecode(text) {
  const input = (text ?? '').trim();
  const out = [];
  if (!input) return out;
  for (const d of DECODERS) {
    // Skip decoders that clearly don't match the shape of the input.
    if (d.id === 'jwt' && input.split('.').length < 3) continue;
    if (d.id === 'urld' && !/%[0-9a-f]{2}/i.test(input)) continue;
    if (d.id === 'htmld' && !/&(#x?[0-9a-f]+|[a-z]+);/i.test(input)) continue;
    if (d.id === 'b64d' && (input.length < 8 || !/^[A-Za-z0-9+/\-_=\s]+$/.test(input))) continue;
    try {
      const output = d.run(input);
      if (output && output !== input && printable(output)) out.push({ id: d.id, label: d.label, output });
    } catch {
      // not this decoder
    }
  }
  return out;
}

/** Look up an operation (decode/encode/hash) by id. */
export const opById = (id) => OPERATIONS.find((o) => o.id === id);

/**
 * Run an ordered list of operation ids over `text`, feeding each step's output
 * into the next (a CyberChef-style recipe). Async because some ops hash.
 * Returns `{ output, error, errorStep }`; errorStep is the failing index or -1.
 * An empty recipe passes the input straight through.
 */
export async function runRecipe(text, ids) {
  let cur = text ?? '';
  for (let i = 0; i < (ids ?? []).length; i++) {
    const op = opById(ids[i]);
    if (!op) continue;
    try {
      cur = await op.run(cur);
    } catch (e) {
      return { output: '', error: e.message ?? String(e), errorStep: i };
    }
  }
  return { output: cur, error: '', errorStep: -1 };
}

export const OPERATIONS = [
  { group: 'Decode', id: 'b64d', label: 'Base64', run: base64Decode },
  { group: 'Decode', id: 'urld', label: 'URL', run: urlDecode },
  { group: 'Decode', id: 'htmld', label: 'HTML', run: htmlDecode },
  { group: 'Decode', id: 'hexd', label: 'Hex', run: hexDecode },
  { group: 'Decode', id: 'jwt', label: 'JWT', run: jwtDecode },
  { group: 'Encode', id: 'b64e', label: 'Base64', run: (t) => base64Encode(t) },
  { group: 'Encode', id: 'b64u', label: 'Base64URL', run: (t) => base64Encode(t, { url: true }) },
  { group: 'Encode', id: 'urle', label: 'URL', run: (t) => urlEncode(t) },
  { group: 'Encode', id: 'urla', label: 'URL (all)', run: (t) => urlEncode(t, { all: true }) },
  { group: 'Encode', id: 'htmle', label: 'HTML', run: htmlEncode },
  { group: 'Encode', id: 'hexe', label: 'Hex', run: hexEncode },
  { group: 'Hash', id: 'md5', label: 'MD5', run: md5 },
  { group: 'Hash', id: 'sha1', label: 'SHA-1', run: (t) => sha('SHA-1', t) },
  { group: 'Hash', id: 'sha256', label: 'SHA-256', run: (t) => sha('SHA-256', t) },
  { group: 'Hash', id: 'sha512', label: 'SHA-512', run: (t) => sha('SHA-512', t) },
];
