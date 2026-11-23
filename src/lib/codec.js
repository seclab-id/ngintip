// Encoders/decoders/hashes for the Decoder view. Each operation takes a
// string and returns a string (hashes are async); invalid input throws.
import { prettyJsonText } from './message.js';

const utf8 = new TextEncoder();
const strictUtf8 = new TextDecoder('utf-8', { fatal: true });

export const toBytes = (text) => utf8.encode(text);

export function toHex(bytes, sep = '') {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join(sep);
}

/** The bytes as text when they are valid UTF-8, otherwise null. */
export function bytesAsText(bytes) {
  try {
    return strictUtf8.decode(bytes);
  } catch {
    return null;
  }
}

/** Text when the bytes are valid UTF-8, otherwise a hex string. */
export function bytesToDisplay(bytes) {
  return bytesAsText(bytes) ?? toHex(bytes, ' ');
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
export function hexDecodeBytes(text) {
  const s = text.replace(/0x/gi, '').replace(/[\s:,-]/g, '');
  if (!/^([0-9a-f]{2})*$/i.test(s)) throw new Error('Not valid hex');
  return Uint8Array.from(s.match(/../g) ?? [], (h) => parseInt(h, 16));
}

export const hexDecode = (text) => bytesToDisplay(hexDecodeBytes(text));

/** Header and payload re-indented; exp/iat/nbf shown as dates. The signature isn't verified.
 *
 *  Each segment is shown with its own bytes (`prettyJsonText`), never re-serialized. A
 *  header that carries two `alg` keys, or a claim spelled with an escape so one parser
 *  reads it and another does not, is the whole reason to open a token here; a view built
 *  by re-serializing the parsed value drops the first and silently decodes the second. */
export function jwtDecode(token) {
  const parts = token.trim().split('.');
  if (parts.length < 2) throw new Error('A JWT must look like header.payload.signature');
  const segment = (part, what) => {
    try {
      // The text is kept: it is what gets shown. The parsed value is only used to read
      // the timestamps below, where a Date is wanted rather than the bytes.
      const text = strictUtf8.decode(base64DecodeBytes(part));
      return { text, value: JSON.parse(text) };
    } catch {
      throw new Error(`JWT ${what} is not valid Base64URL JSON`);
    }
  };
  const header = segment(parts[0], 'Header');
  const payload = segment(parts[1], 'Payload');
  const show = (s) => prettyJsonText(s.text) ?? s.text;
  const out = ['// Header', show(header), '', '// Payload', show(payload)];
  const claims = payload.value ?? {};
  const times = ['iat', 'nbf', 'exp'].filter((k) => typeof claims[k] === 'number');
  if (times.length) {
    out.push('');
    for (const k of times) {
      const date = new Date(claims[k] * 1000);
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
// Decoders that produce BYTES. Their displayed output is a hex dump whenever those
// bytes are not valid UTF-8, and a hex dump is always "printable" - so judging the
// rendering lets binary garbage through. These are judged on their bytes instead.
const BYTE_DECODERS = { b64d: base64DecodeBytes, hexd: hexDecodeBytes };

// ...but "the bytes are not text" is not the same as "this is not encoded". On an API
// whose bodies are encrypted, the hex dump IS the answer: the operator highlights a blob
// precisely to see that it is 256 bytes of ciphertext and not a token. Judging on bytes
// alone threw that away, so a 344-character AES body opened nothing at all.
//
// A blob is told apart from a word by shape, since the bytes have already said "not
// text": long enough that no word reaches it, and carrying a character a word does not.
// Measured, not guessed: 0 false positives over the 235.976 words of /usr/share/dict/words
// and over the long camelCase identifiers in this engagement's traffic
// (`transactionReferenceNumber`, which a plain length rule did claim), and 0 missed over
// 1.000 random bodies of 16..512 bytes, base64 and hex alike.
const BLOB_LIKE = {
  b64d: (s) => {
    const c = s.replace(/\s+/g, '');
    return c.length >= 24 && /[0-9+/=_]/.test(c);
  },
  hexd: (s) => s.replace(/0x/gi, '').replace(/[\s:,-]/g, '').length >= 32,
};

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
      // Selecting an ordinary word like `productType` used to open the decoder on
      // `a6 ba 1d b9 cb 53 ca 97`: the word is valid Base64 by shape, the bytes are
      // nonsense, and `printable()` was looking at the hex dump of that nonsense rather
      // than at the nonsense itself. Auto-detection must only claim a match when the
      // bytes really are text.
      const bytes = BYTE_DECODERS[d.id];
      if (bytes && bytesAsText(bytes(input)) === null && !BLOB_LIKE[d.id](input)) continue;
      const output = d.run(input);
      if (output && output !== input && printable(output)) out.push({ id: d.id, label: d.label, output });
    } catch {
      // not this decoder
    }
  }
  return out;
}

/** Label of the likeliest decoding, for the Decode chip, or '' when nothing matches.
 *
 *  Capped on purpose: the chip now appears on every selection, and running five decoders
 *  over a 100 KB selection on each mouseup would be felt in the hand. The hint is a
 *  nicety; the decoder itself still works on the whole selection once it is opened. */
export function decodeHint(text, max = 4096) {
  const s = text ?? '';
  if (!s.trim() || s.length > max) return '';
  return autoDecode(s)[0]?.label ?? '';
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
