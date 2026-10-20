// Tiny HTTP message highlighter. It only wraps text in spans and never changes
// the characters, so the editor overlay stays aligned with its textarea.

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

function startLine(line) {
  const parts = line.split(' ');
  if (/^HTTP\/\d/.test(parts[0])) {
    const [version, status = '', ...reason] = parts;
    const cls = status[0] >= '2' && status[0] <= '5' ? `s-${status[0]}xx` : 's-err';
    return (
      `<span class="hl-dim">${esc(version)}</span> ` +
      `<span class="hl-status ${cls}">${esc(status)}</span> ` +
      `<span class="hl-dim">${esc(reason.join(' '))}</span>`
    );
  }
  if (parts.length >= 3) {
    const [method, ...rest] = parts;
    const version = rest.pop();
    return (
      `<span class="hl-method m-${esc(method.toLowerCase())}">${esc(method)}</span> ` +
      `<span class="hl-path">${esc(rest.join(' '))}</span> ` +
      `<span class="hl-dim">${esc(version)}</span>`
    );
  }
  return esc(line);
}

function headerLine(line) {
  const i = line.indexOf(':');
  if (i <= 0) return esc(line);
  return (
    `<span class="hl-hname">${esc(line.slice(0, i))}</span>` +
    `<span class="hl-dim">:</span>` +
    `<span class="hl-hval">${esc(line.slice(i + 1))}</span>`
  );
}

const JSON_TOKEN = /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;
const MARKUP_TOKEN = /(&lt;\/?)([\w:-]+)|([\w:-]+)(=)("[^"]*"|'[^']*')|(\/?&gt;)/g;

// Above this many characters, colouring is skipped (just escaped) to keep the
// editable editor snappy per keystroke. The read-only viewers pass a much higher
// limit (see `highlightHttp`), so a large pretty-printed body still gets colour.
export const HL_LIMIT = 200_000;

/** Highlight a bare body (JSON or markup); other text is only escaped. */
export function highlightBody(text, limit = HL_LIMIT) {
  if (!text) return '';
  if (text.length > limit) return esc(text); // keep huge bodies cheap
  const trimmed = text.trimStart();
  if (trimmed[0] === '{' || trimmed[0] === '[') {
    return esc(text).replace(JSON_TOKEN, (m, str, colon, lit, num) => {
      if (str) return colon ? `<span class="hl-key">${str}</span>${colon}` : `<span class="hl-str">${str}</span>`;
      if (lit) return `<span class="hl-lit">${lit}</span>`;
      return `<span class="hl-num">${num}</span>`;
    });
  }
  if (trimmed[0] === '<') {
    return esc(text).replace(MARKUP_TOKEN, (m, open, tag, attr, eq, val, close) => {
      if (tag) return `<span class="hl-dim">${open}</span><span class="hl-tag">${tag}</span>`;
      if (attr) return `<span class="hl-attr">${attr}</span>${eq}<span class="hl-str">${val}</span>`;
      return `<span class="hl-dim">${close}</span>`;
    });
  }
  return esc(text);
}

export function highlightHttp(raw, limit = HL_LIMIT) {
  if (!raw) return '';
  const split = raw.indexOf('\n\n');
  const head = split === -1 ? raw : raw.slice(0, split);
  const [first, ...headers] = head.split('\n');

  let out = startLine(first);
  for (const h of headers) out += '\n' + headerLine(h);
  if (split !== -1) out += '\n\n' + highlightBody(raw.slice(split + 2), limit);
  return out;
}
