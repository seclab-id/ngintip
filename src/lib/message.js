// Helpers for raw HTTP messages ("head\n\nbody") shown in viewers/editors.

export function splitMessage(raw = '') {
  const i = raw.indexOf('\n\n');
  return i === -1 ? { head: raw, body: '' } : { head: raw.slice(0, i), body: raw.slice(i + 2) };
}

export function contentType(head) {
  const m = head.match(/^content-type:\s*(.+)$/im);
  return m ? m[1].trim().toLowerCase() : '';
}

// Auto-formatting (JSON.parse / markup reflow) is O(n) with heavy string work, so
// skip it for very large bodies to keep opening a request snappy (a huge 502/HTML
// page could otherwise freeze the UI). "Show all" still shows the raw text.
// Must stay >= the backend view cap (raw.rs VIEW_LIMIT = 256 KB) so the normal,
// view-capped body (e.g. a ~280 KB SAP OData $metadata XML) can still be prettified;
// only genuinely huge "Show all" bodies above this are left raw.
export const MAX_PRETTY = 512 * 1024;

// A single very long line (an inline `data:` URI, a minified JS/JSON/HTML blob with
// no whitespace) makes the browser line-breaker (word-break: break-all on .code)
// pathologically slow and beachballs the whole webview, even at a few hundred KB.
// Break such lines into chunks so layout only ever sees short lines. Only over-long
// lines are touched; ordinary text is returned unchanged, so copy/search stay intact
// for normal messages. Used by the read-only viewers, never by the editable overlay
// (whose highlight layer must stay character-aligned with its textarea).
export const MAX_LINE = 2000;

export function softWrapLong(text, max = MAX_LINE) {
  if (!text) return '';
  if (text.length <= max) return text;
  // Fast path: leave the text untouched unless some single line is over the limit.
  let over = false;
  for (let s = 0; s < text.length; ) {
    let nl = text.indexOf('\n', s);
    if (nl === -1) nl = text.length;
    if (nl - s > max) {
      over = true;
      break;
    }
    s = nl + 1;
  }
  if (!over) return text;

  const out = [];
  for (const line of text.split('\n')) {
    if (line.length <= max) {
      out.push(line);
      continue;
    }
    let i = 0;
    while (i < line.length) {
      let end = Math.min(line.length, i + max);
      // Don't split a UTF-16 surrogate pair across the boundary.
      const c = line.charCodeAt(end - 1);
      if (end < line.length && c >= 0xd800 && c <= 0xdbff) end -= 1;
      out.push(line.slice(i, end));
      i = end;
    }
  }
  return out.join('\n');
}

/** End index (exclusive) of the JSON string starting at `start`, escapes honoured. */
function stringEnd(src, start) {
  for (let j = start + 1; j < src.length; j++) {
    if (src[j] === '\\') {
      j += 1; // whatever follows a backslash is part of the string, including a quote
      continue;
    }
    if (src[j] === '"') return j + 1;
  }
  return src.length; // unterminated; the caller already validated, so this cannot happen
}

/**
 * Re-indent JSON by copying its own tokens, never by re-serializing it.
 *
 * `JSON.stringify(JSON.parse(body), null, 2)` looks like the obvious way to do this and
 * is wrong for a tool whose job is sending and showing exact bytes. The round trip is a
 * normalizer: it rewrites the payload instead of re-spacing it.
 *
 *   {"user":"ad\u0063min"}            ->  {"user":"adcmin"}
 *   {"path":"\/etc\/passwd"}          ->  {"path":"/etc/passwd"}
 *   {"role":"user","role":"admin"}   ->  {"role":"admin"}
 *   {"id":12345678901234567890}      ->  {"id":12345678901234567000}
 *   {"n":1e3,"z":-0}                 ->  {"n":1000,"z":0}
 *
 * Every line of that is a payload class someone uses on purpose: an escaped character to
 * slip past a filter that only reads the literal, an escaped solidus, a duplicate key to
 * split two parsers apart, an integer past 2^53 to find where precision is lost. The old
 * behaviour destroyed all of them, in the Repeater body about to be sent, in the Pretty
 * view of a captured request, in an intercept draft on its way to the target, and in the
 * evidence image. The one it was reported through was `\u0063` turning into `c`.
 *
 * So this walks the source instead: strings are copied verbatim from quote to quote,
 * numbers and `true` / `false` / `null` are copied as typed, and only the whitespace
 * BETWEEN tokens is replaced. The layout it produces matches
 * `JSON.stringify(value, null, 2)` for input that has nothing unusual in it, so nothing
 * about the familiar look changes.
 */
function reindentJson(src) {
  const out = [];
  let indent = 0;
  const pad = () => `\n${'  '.repeat(indent)}`;
  const nextNonSpace = (from) => {
    let i = from;
    while (i < src.length && (src[i] === ' ' || src[i] === '\t' || src[i] === '\n' || src[i] === '\r')) i += 1;
    return i;
  };

  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === '"') {
      // Copied from quote to quote, so nothing inside is read as structure.
      const stop = stringEnd(src, i);
      out.push(src.slice(i, stop));
      i = stop;
    } else if (c === ' ' || c === '\t' || c === '\n' || c === '\r') {
      i += 1; // the only thing this function is allowed to change
    } else if (c === '{' || c === '[') {
      // An empty container stays on one line, as JSON.stringify writes it.
      const close = c === '{' ? '}' : ']';
      const after = nextNonSpace(i + 1);
      if (src[after] === close) {
        out.push(c, close);
        i = after + 1;
      } else {
        indent += 1;
        out.push(c, pad());
        i += 1;
      }
    } else if (c === '}' || c === ']') {
      indent -= 1;
      out.push(pad(), c);
      i += 1;
    } else if (c === ',') {
      out.push(',', pad());
      i += 1;
    } else if (c === ':') {
      out.push(': ');
      i += 1;
    } else {
      out.push(c); // a number, or true / false / null: exactly as typed
      i += 1;
    }
  }
  return out.join('');
}

// Cost, measured (node, this machine, against the native round trip it replaced):
// 48 KB body 1.4 ms vs 0.4 ms, 198 KB 4.6 ms vs 1.5 ms, 400 KB 8.9 ms vs 3.1 ms. So
// about 3x, bounded by MAX_PRETTY, on a pane that already costs more than this to
// highlight. Two faster-looking rewrites were tried and measured: ending verbatim runs
// with a Set of structural characters was WORSE (2.1 ms at 48 KB), and a charCodeAt
// switch was the same (1.6 / 4.3 / 9.0). Neither earned its complexity, so this stayed
// the readable one.

// The backend caps the default body view at raw.rs VIEW_LIMIT (256 KB) and says so in
// a line it appends. That line matters here: the text in front of it is a document with
// its tail cut off, usually mid-string, so it cannot parse. Pretty therefore used to
// disappear at exactly the size where reading one 256 KB line by hand is hopeless.
const TRUNCATION_NOTE = /\n*\[truncated: showing the first \d+ bytes; use Show all for the rest\]\s*$/;

/** `{ text, note }`: the body without the backend's truncation line, and that line
 *  (empty when the body is whole). The note is handed back unchanged so a formatted
 *  view still ends by saying the body goes on. */
export function splitTruncation(body) {
  const m = (body ?? '').match(TRUNCATION_NOTE);
  return m ? { text: body.slice(0, m.index), note: body.slice(m.index) } : { text: body ?? '', note: '' };
}

/** A bare JSON document re-indented, or null when it is not JSON.
 *
 *  Its own bytes are preserved; only the whitespace between tokens changes. See
 *  `reindentJson` for why that distinction is the whole point. Used for a message body
 *  and for a WebSocket frame, which has no headers to split off.
 *
 *  `cut` says the caller knows this text is a document with its tail missing, so failing
 *  to parse is expected and not a verdict about what it is. Re-indenting is still safe
 *  (whitespace between tokens is all that moves) and it is the only way a capped JSON
 *  body is readable at all. Callers pass it only for a body the backend marked as cut
 *  AND whose Content-Type claims JSON, so a truncated script that happens to start with
 *  `{` is not reformatted on a guess. */
export function prettyJsonText(src, cut = false) {
  const text = (src ?? '').trim();
  if (!text) return null;
  try {
    // Parsed for validation only. The value it produces is deliberately thrown away:
    // the output is built from the source text, not from the parsed value.
    JSON.parse(text);
  } catch {
    if (!cut || !/^[{[]/.test(text)) return null;
  }
  return reindentJson(text);
}

/** Message with a JSON body re-indented, or null if the body isn't JSON. */
export function prettyJson(raw) {
  const { head, body } = splitMessage(raw);
  if (!body.trim() || body.length > MAX_PRETTY) return null;
  const { text, note } = splitTruncation(body);
  const out = prettyJsonText(text, note !== '' && /json/i.test(contentType(head)));
  return out === null ? null : `${head}\n\n${out}${note}`;
}

/** Message with a JSON, HTML or XML body re-indented, or null if there's nothing to format. */
export function prettyBody(raw) {
  const { head, body } = splitMessage(raw);
  if (!body.trim() || body.length > MAX_PRETTY) return null;
  const json = prettyJson(raw);
  if (json) return json;
  const ct = contentType(head);
  // The truncation line is held aside and put back after: formatting it as markup
  // would wrap a square-bracketed sentence that is the viewer talking, not the body.
  const { text, note } = splitTruncation(body);
  // Treat it as markup when the body starts with a tag AND either the content-type
  // says so, there is no content-type, or the body itself clearly declares XML/HTML
  // (so XML served under an odd MIME type still gets prettified).
  const declaresMarkup = /^\s*<(\?xml[\s?]|!doctype\b|!\[cdata\[|svg\b|html\b)/i.test(text);
  const markup = /^\s*</.test(text) && (/xml|html|svg/.test(ct) || !ct || declaresMarkup);
  if (!markup) return null;
  const formatted = formatMarkup(text);
  return formatted.trim() === text.trim() ? null : `${head}\n\n${formatted}${note}`;
}

// Elements without a closing tag, and ones whose content is kept verbatim.
const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']);
const VERBATIM = new Set(['script', 'style', 'pre', 'textarea']);
// Comments and CDATA are copied as they are.
const SPECIAL = [
  ['<!--', '-->'],
  ['<![CDATA[', ']]>'],
];

/** True for a run of text between tags that is the source's own line layout: whitespace
 *  all the way through, and carrying a line break.
 *
 *  That run is the only thing `formatMarkup` is allowed to drop, because it replaces it
 *  with a line break and indentation of its own. Everything else between tags is data
 *  and is copied byte for byte. This used to collapse every run with `\s+ -> ' '` plus a
 *  `trim()`, which rewrote content: `<pw> secret </pw>` became `<pw>secret</pw>`,
 *  `<pad> </pad>` became `<pad></pad>`, and `a  b` became `a b`. On the display side
 *  that is only untidy, but Beautify also runs on what Repeater and Intercept send, so
 *  it silently altered a signed SOAP body and any value whose spacing matters.
 *
 *  The cost is that a source which is already indented keeps its own line breaks inside
 *  a text node instead of being squared up. A minified document, which is where
 *  Beautify is actually wanted, is unaffected: it has no line breaks to keep. */
const isLayoutGap = (s) => s.includes('\n') && !s.trim();

/** Index of the `>` closing the tag that starts at `i`, skipping quoted attribute values. */
function tagEnd(src, i) {
  let quote = null;
  for (let j = i + 1; j < src.length; j++) {
    const c = src[j];
    if (quote) {
      if (c === quote) quote = null;
    } else if (c === '"' || c === "'") {
      quote = c;
    } else if (c === '>') {
      return j;
    }
  }
  return src.length - 1;
}

/** One tag or text run per line, indented by nesting. Lenient with sloppy HTML.
 *
 *  Two lookups used to re-scan the whole document per element, which made this
 *  quadratic: a 380 KB HTML response took 1.04 s here (and 1.44 s through `prettyBody`),
 *  and because `MessageView`'s mode list reads `pretty` for its label, that was paid on
 *  every mount of the History detail pane even in Raw mode. Hence the lowercase copy
 *  below, made at most once per call, and the fixed-width window at the inline-leaf
 *  check. Output is unchanged; see the equivalence tests in message.test.js.
 */
export function formatMarkup(src, indent = '  ') {
  const out = [];
  let depth = 0;
  const push = (s) => out.push(indent.repeat(depth) + s);
  let i = 0;
  // Only built if a verbatim element (script/style/pre/textarea) is actually met, and
  // then only once for the whole document instead of once per such element.
  let lowered = null;
  const lower = () => (lowered ??= src.toLowerCase());
  while (i < src.length) {
    const verbatim = SPECIAL.find(([open]) => src.startsWith(open, i));
    if (verbatim) {
      const end = src.indexOf(verbatim[1], i);
      const j = end === -1 ? src.length : end + verbatim[1].length;
      push(src.slice(i, j));
      i = j;
      continue;
    }

    if (src[i] === '<') {
      const end = tagEnd(src, i);
      const tag = src.slice(i, end + 1);
      i = end + 1;
      if (tag.startsWith('</')) {
        depth = Math.max(0, depth - 1);
        push(tag);
        continue;
      }
      push(tag);
      const name = (tag.match(/^<([\w:-]+)/)?.[1] ?? '').toLowerCase();
      // Processing instructions / declarations (<?xml?>, <!DOCTYPE>), self-closing
      // and void elements have no children.
      if (!name || tag.endsWith('/>') || VOID.has(name)) continue;
      if (VERBATIM.has(name)) {
        depth++;
        const close = lower().indexOf(`</${name}`, i);
        const j = close === -1 ? src.length : close;
        const content = src.slice(i, j).replace(/^\s*\n|\s+$/g, '');
        if (content.trim()) out.push(content);
        i = j;
        continue;
      }
      // Inline leaf: an element whose only content is a text node stays on one line,
      // e.g. <to>Tove</to> instead of splitting the text onto its own indented line.
      const nextLt = src.indexOf('<', i);
      if (nextLt !== -1) {
        const closeTok = `</${name}`;
        // Exactly as much as the test below reads: the token plus the one character
        // after it. Slicing the rest of the document here is what made this quadratic.
        const rest = src.slice(nextLt, nextLt + closeTok.length + 1).toLowerCase();
        const after = rest[closeTok.length];
        if (rest.startsWith(closeTok) && (after === undefined || after === '>' || /\s/.test(after))) {
          const closeEnd = tagEnd(src, nextLt);
          const run = src.slice(i, nextLt);
          out[out.length - 1] += (isLayoutGap(run) ? '' : run) + src.slice(nextLt, closeEnd + 1);
          i = closeEnd + 1;
          continue;
        }
      }
      depth++;
      continue;
    }

    const next = src.indexOf('<', i);
    const j = next === -1 ? src.length : next;
    const run = src.slice(i, j);
    if (!isLayoutGap(run)) push(run);
    i = j;
  }
  return out.join('\n');
}

/** HTML body for the Preview tab, or null. */
export function htmlBody(raw) {
  const { head, body } = splitMessage(raw);
  if (!body.trim()) return null;
  const ct = contentType(head);
  return ct.includes('html') || (!ct && /^\s*<(!doctype html|html)/i.test(body)) ? body : null;
}
