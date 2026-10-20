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

/** Message with a JSON body re-indented, or null if the body isn't JSON. */
export function prettyJson(raw) {
  const { head, body } = splitMessage(raw);
  if (!body.trim() || body.length > MAX_PRETTY) return null;
  try {
    return `${head}\n\n${JSON.stringify(JSON.parse(body), null, 2)}`;
  } catch {
    return null;
  }
}

/** Message with a JSON, HTML or XML body re-indented, or null if there's nothing to format. */
export function prettyBody(raw) {
  const { head, body } = splitMessage(raw);
  if (!body.trim() || body.length > MAX_PRETTY) return null;
  const json = prettyJson(raw);
  if (json) return json;
  const ct = contentType(head);
  // Treat it as markup when the body starts with a tag AND either the content-type
  // says so, there is no content-type, or the body itself clearly declares XML/HTML
  // (so XML served under an odd MIME type still gets prettified).
  const declaresMarkup = /^\s*<(\?xml[\s?]|!doctype\b|!\[cdata\[|svg\b|html\b)/i.test(body);
  const markup = /^\s*</.test(body) && (/xml|html|svg/.test(ct) || !ct || declaresMarkup);
  if (!markup) return null;
  const formatted = formatMarkup(body);
  return formatted.trim() === body.trim() ? null : `${head}\n\n${formatted}`;
}

// Elements without a closing tag, and ones whose content is kept verbatim.
const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']);
const VERBATIM = new Set(['script', 'style', 'pre', 'textarea']);
// Comments and CDATA are copied as they are.
const SPECIAL = [
  ['<!--', '-->'],
  ['<![CDATA[', ']]>'],
];

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

/** One tag or text run per line, indented by nesting. Lenient with sloppy HTML. */
export function formatMarkup(src, indent = '  ') {
  const out = [];
  let depth = 0;
  const push = (s) => out.push(indent.repeat(depth) + s);
  let i = 0;
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
        const close = src.toLowerCase().indexOf(`</${name}`, i);
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
        const rest = src.slice(nextLt).toLowerCase();
        const closeTok = `</${name}`;
        const after = rest[closeTok.length];
        if (rest.startsWith(closeTok) && (after === undefined || after === '>' || /\s/.test(after))) {
          const closeEnd = tagEnd(src, nextLt);
          const text = src.slice(i, nextLt).replace(/\s+/g, ' ').trim();
          out[out.length - 1] += text + src.slice(nextLt, closeEnd + 1);
          i = closeEnd + 1;
          continue;
        }
      }
      depth++;
      continue;
    }

    const next = src.indexOf('<', i);
    const j = next === -1 ? src.length : next;
    const text = src.slice(i, j).trim();
    if (text) push(text.replace(/\s+/g, ' '));
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
