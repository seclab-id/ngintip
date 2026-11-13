// Markdown subset for notes, written for a tool whose notes routinely hold text copied
// out of live traffic: payloads, response fragments, tokens.
//
// THE RULE: no character of the source ever reaches the output unescaped. Every text
// segment goes through `esc()` before it is wrapped in a tag, and the only tags in the
// result are the ones written here. That is the same approach `highlight.js` takes for
// HTTP bodies, and it is what makes `{@html}` safe on this output: a note containing
// `<img src=x onerror=alert(1)>` renders as that text, it does not become an element.
//
// There is no CSP in this app (tauri.conf.json) and any script in the webview can reach
// `invoke`, so rendering note content as real HTML would turn a payload you saved into
// code running against your own backend. Hence a hand-written subset instead of a
// general parser plus a sanitiser: nothing to misconfigure, and no raw-HTML passthrough
// to remember to switch off.

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ESC[c]);

/** Beyond this, rendering is skipped and the note is shown as plain preformatted text.
 *  Notes are human-written, so this is only a guard against a giant paste. */
export const MD_LIMIT = 200_000;

/** Schemes allowed in a link. Everything else (`javascript:`, `data:`, `file:`, a bare
 *  `//host`) renders as plain text, so a note can never carry a clickable exploit. */
const SAFE_URL = /^(?:https?:\/\/|mailto:)[^\s]+$/i;

/** The checked URL, or null when it is not one we are willing to make clickable. */
export function safeHref(raw) {
  const url = String(raw ?? '').trim();
  return SAFE_URL.test(url) ? url : null;
}

// Placeholders park already-rendered fragments (code spans, links) so later inline
// passes cannot reach inside them. U+0000 is stripped from the source first, so a note
// cannot forge one of these.
const HOLD = '\u0000';
const holdRe = /\u0000(\d+)\u0000/g;

// Every inline pattern below is length-bounded. An unbounded lazy `[\s\S]*?` rescans the
// rest of the note from each start position, so a note full of unclosed `**` or `` ` ``
// (which is exactly what a saved payload looks like) would cost O(n^2). These caps are
// far above any real span and turn the cost linear. Measured: without them, 20k nested
// brackets took 87 ms and 5k `>` overflowed the stack.
const SPAN = 2000; // longest emphasised run
const CODE = 4000; // longest inline code span; longer belongs in a fenced block
const LABEL = 200; // longest link label
const URL = 2000; // longest link target

// Compiled once. `emphasis` runs for every paragraph, list item and table cell, so
// building these per call cost ~40% of a render and left garbage behind for the
// collector: measured 0.039 -> 0.059 ms on a small note before they were hoisted.
const EMPHASIS = [
  [new RegExp(`\\*\\*\\*(?!\\s)([\\s\\S]{0,${SPAN}}?\\S)\\*\\*\\*`, 'g'), '<strong><em>$1</em></strong>'],
  [new RegExp(`\\*\\*(?!\\s)([\\s\\S]{0,${SPAN}}?\\S)\\*\\*`, 'g'), '<strong>$1</strong>'],
  [new RegExp(`(^|[^\\w*])\\*(?!\\s)([^*\\n]{0,${SPAN}}?\\S)\\*(?!\\w)`, 'g'), '$1<em>$2</em>'],
  [new RegExp(`(^|[^\\w_])__(?!\\s)([\\s\\S]{0,${SPAN}}?\\S)__(?!\\w)`, 'g'), '$1<strong>$2</strong>'],
  [new RegExp(`(^|[^\\w_])_(?!\\s)([^_\\n]{0,${SPAN}}?\\S)_(?!\\w)`, 'g'), '$1<em>$2</em>'],
  [new RegExp(`~~(?!\\s)([\\s\\S]{0,${SPAN}}?\\S)~~`, 'g'), '<del>$1</del>'],
];

/** Inline emphasis, applied to text that is ALREADY escaped. */
function emphasis(s) {
  let out = s;
  for (const [re, to] of EMPHASIS) out = out.replace(re, to);
  return out;
}

const CODE_RE = new RegExp(`(\`+)([\\s\\S]{0,${CODE}}?[^\`])\\1(?!\`)`, 'g');
const LINK_RE = new RegExp(`!?\\[([^\\]\\n]{0,${LABEL}})\\]\\(((?:[^()\\s]|\\([^()\\s]*\\)){1,${URL}})\\)`, 'g');

/** Render one run of inline markdown. `raw` is unescaped source. */
function inline(raw) {
  const held = [];
  const park = (html) => `${HOLD}${held.push(html) - 1}${HOLD}`;

  let s = String(raw ?? '');

  // Code spans first: their contents must survive verbatim, with no emphasis applied.
  s = s.replace(CODE_RE, (m, _ticks, code) => park(`<code>${esc(code)}</code>`));

  // Links. Done before escaping, so a `&` in a query string stays a `&` in the href
  // instead of being double-escaped; the URL is escaped again on the way into the
  // attribute. The URL may hold one level of balanced parentheses, which real URLs do.
  //
  // A rejected scheme falls back to the ORIGINAL markdown as text, not to the label
  // alone: the URL in a note is often the point (a payload you meant to keep), so
  // silently swallowing it would lose your work. Nothing is dropped, nothing is linked.
  s = s.replace(LINK_RE, (m, label, url) => {
    const href = safeHref(url);
    if (!href) return park(esc(m));
    const text = emphasis(esc(label)) || esc(url);
    return park(`<a href="${esc(href)}" rel="noreferrer noopener">${text}</a>`);
  });

  // Autolinks: <https://...> and a bare URL.
  s = s.replace(/<((?:https?:\/\/|mailto:)[^\s>]+)>/gi, (m, url) => {
    const href = safeHref(url);
    return park(href ? `<a href="${esc(href)}" rel="noreferrer noopener">${esc(url)}</a>` : esc(m));
  });
  s = s.replace(/(^|[\s(])(https?:\/\/[^\s<>()]+)/g, (m, pre, url) => {
    const href = safeHref(url);
    return pre + park(href ? `<a href="${esc(href)}" rel="noreferrer noopener">${esc(url)}</a>` : esc(url));
  });

  // Everything still in `s` is untrusted source text. From here on it is escaped.
  s = emphasis(esc(s));

  return s.replace(holdRe, (m, i) => held[Number(i)] ?? '');
}

const LIST_ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TASK = /^\[([ xX])\]\s+(.*)$/;
const DELIM_ROW = /^\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/;

/** Split a table row into its cells, tolerating the optional outer pipes. */
function cells(line) {
  return line
    .trim()
    .replace(/^\|/, '')
    .replace(/\|$/, '')
    .split('|')
    .map((c) => c.trim());
}

function alignments(delim) {
  return cells(delim).map((c) => (/^:-+:$/.test(c) ? 'c' : /-+:$/.test(c) ? 'r' : /^:-+/.test(c) ? 'l' : ''));
}

/** True when `line` starts a block that must interrupt an open paragraph. */
function startsBlock(line) {
  return (
    line.trim() === '' ||
    /^(```|~~~)/.test(line) ||
    /^#{1,6}\s/.test(line) ||
    /^\s*>/.test(line) ||
    /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line) ||
    LIST_ITEM.test(line)
  );
}

/** Render `lines[from..]` as nested lists, up to the first line that is not a list item
 *  at or deeper than the opening indent. Returns the HTML and where to carry on. */
function renderList(lines, from) {
  const out = [];
  const stack = []; // { indent, tag }
  let i = from;

  const close = (toIndent) => {
    while (stack.length && stack[stack.length - 1].indent > toIndent) {
      out.push('</li>', `</${stack.pop().tag}>`);
    }
  };

  while (i < lines.length) {
    const m = LIST_ITEM.exec(lines[i]);
    if (!m) {
      // A blank line inside a list is allowed as long as a deeper/equal item follows.
      if (lines[i].trim() === '' && LIST_ITEM.test(lines[i + 1] ?? '')) {
        i++;
        continue;
      }
      break;
    }
    const [, pad, bullet, rest] = m;
    const indent = pad.replace(/\t/g, '  ').length;
    const tag = /\d/.test(bullet) ? 'ol' : 'ul';

    if (!stack.length || indent > stack[stack.length - 1].indent) {
      stack.push({ indent, tag });
      out.push(`<${tag}>`);
    } else {
      close(indent);
      const top = stack[stack.length - 1];
      if (top.tag !== tag) {
        out.push('</li>', `</${top.tag}>`, `<${tag}>`);
        stack[stack.length - 1] = { indent, tag };
      } else {
        out.push('</li>');
      }
    }

    const task = TASK.exec(rest);
    if (task) {
      const checked = task[1] !== ' ' ? ' checked' : '';
      out.push(`<li class="md-task"><input type="checkbox" disabled${checked} /> ${inline(task[2])}`);
    } else {
      out.push(`<li>${inline(rest)}`);
    }
    i++;
  }

  close(-1);
  return [out.join(''), i];
}

/** How deep blockquotes may nest before the rest is rendered flat. Each level is a
 *  recursive call, so an unbounded one lets `>>>>>...` overflow the stack and take the
 *  whole view down with it. Real notes quote a level or two. */
const MAX_QUOTE_DEPTH = 8;

/**
 * Render a markdown subset to HTML that is safe to inject: headings, fenced and inline
 * code, bold / italic / strikethrough, links (safe schemes only), nested and task
 * lists, blockquotes, tables and rules. Anything else stays as the text it is.
 */
export function renderMarkdown(src) {
  return render(src, 0);
}

function render(src, depth) {
  const text = String(src ?? '')
    .replace(/\r\n?/g, '\n')
    .replace(/\u0000/g, '');
  if (!text.trim()) return '';
  if (text.length > MD_LIMIT) return `<pre class="md-raw">${esc(text)}</pre>`;

  const lines = text.split('\n');
  const out = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (line.trim() === '') {
      i++;
      continue;
    }

    // Fenced code. Unterminated fences run to the end rather than dropping the text.
    const fence = /^\s*(```+|~~~+)\s*([\w+#.-]*)\s*$/.exec(line);
    if (fence) {
      const close = new RegExp(`^\\s*${fence[1][0] === '`' ? '`' : '~'}{${fence[1].length},}\\s*$`);
      const body = [];
      i++;
      while (i < lines.length && !close.test(lines[i])) body.push(lines[i++]);
      if (i < lines.length) i++;
      const lang = fence[2] ? ` class="md-lang-${esc(fence[2].toLowerCase())}"` : '';
      out.push(`<pre class="md-pre"><code${lang}>${esc(body.join('\n'))}</code></pre>`);
      continue;
    }

    if (/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      out.push('<hr />');
      i++;
      continue;
    }

    const head = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (head) {
      const n = head[1].length;
      out.push(`<h${n}>${inline(head[2])}</h${n}>`);
      i++;
      continue;
    }

    // Table: a row of cells whose next line is the delimiter row.
    if (line.includes('|') && DELIM_ROW.test(lines[i + 1] ?? '')) {
      const align = alignments(lines[i + 1]);
      const cls = (n) => (align[n] ? ` class="md-${align[n]}"` : '');
      const head = cells(line).map((c, n) => `<th${cls(n)}>${inline(c)}</th>`).join('');
      i += 2;
      const body = [];
      while (i < lines.length && lines[i].includes('|') && lines[i].trim() !== '') {
        body.push(`<tr>${cells(lines[i]).map((c, n) => `<td${cls(n)}>${inline(c)}</td>`).join('')}</tr>`);
        i++;
      }
      out.push(`<table class="md-table"><thead><tr>${head}</tr></thead><tbody>${body.join('')}</tbody></table>`);
      continue;
    }

    if (/^\s*>/.test(line)) {
      const quoted = [];
      while (i < lines.length && (/^\s*>/.test(lines[i]) || (quoted.length && lines[i].trim() !== ''))) {
        quoted.push(lines[i].replace(/^\s*>\s?/, ''));
        i++;
      }
      // Past the depth cap, stop recursing and show what is left as text, so a note
      // made of `>` characters neither disappears nor blows the stack.
      const inner =
        depth + 1 < MAX_QUOTE_DEPTH
          ? render(quoted.join('\n'), depth + 1)
          : `<p>${inline(quoted.join('\n'))}</p>`;
      out.push(`<blockquote>${inner}</blockquote>`);
      continue;
    }

    if (LIST_ITEM.test(line)) {
      const [html, next] = renderList(lines, i);
      out.push(html);
      i = next;
      continue;
    }

    // Paragraph: runs until a blank line or the start of another block. A trailing
    // backslash or two spaces makes a hard break, like every other markdown.
    const para = [];
    while (i < lines.length && !startsBlock(lines[i])) para.push(lines[i++]);
    out.push(`<p>${inline(para.join('\n')).replace(/(\\|\s\s)\n/g, '<br />').replace(/\n/g, ' ')}</p>`);
  }

  return out.join('\n');
}
