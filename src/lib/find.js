// Shared in-pane find/search helpers, used by MessageView, CodeEditor and the
// Intercept Original view so the find bar behaves the same everywhere. Matches
// are layered ON TOP of already-highlighted HTML (via text offsets), so syntax
// colours stay while searching.

/**
 * Match spans [start, end) of `query` in `text`, in order and non-overlapping.
 * Options (all off by default, so plain callers get case-insensitive substring search):
 *   - caseSensitive: match case exactly.
 *   - regex: treat `query` as a JS regular expression (invalid regex -> no matches, error).
 *   - word: whole-word only (wraps the pattern in \b...\b).
 * Returns { ranges, count, error } (error = true only for an invalid regex).
 */
export function matchRanges(text, query, opts = {}) {
  if (!query) return { ranges: [], count: 0 };
  let re;
  try {
    let pattern = opts.regex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    if (opts.word) pattern = `\\b(?:${pattern})\\b`;
    re = new RegExp(pattern, opts.caseSensitive ? 'g' : 'gi');
  } catch {
    return { ranges: [], count: 0, error: true };
  }
  const ranges = [];
  let m;
  let guard = 0;
  while ((m = re.exec(text)) && guard++ < 200000) {
    if (m[0].length === 0) { re.lastIndex++; continue; } // skip zero-width matches
    ranges.push([m.index, m.index + m[0].length]);
  }
  return { ranges, count: ranges.length, error: false };
}

/**
 * Insert `<mark>` around `ranges` (text offsets) into already-highlighted `html`,
 * so search highlighting is layered ON TOP of syntax highlighting (colours stay).
 * Walks the HTML: tags are copied verbatim (a mark is closed before a tag and
 * reopened after, so nesting stays valid); entities count as one text char. The
 * match at `activeIdx` gets `.on`.
 */
export function markHtmlRanges(html, ranges, activeIdx = 0) {
  if (!ranges || !ranges.length) return html;
  let out = '';
  let pos = 0; // decoded-text position
  let i = 0;
  let ri = 0; // index of the range we might be inside
  let open = false;
  const close = () => {
    if (open) {
      out += '</mark>';
      open = false;
    }
  };
  while (i < html.length) {
    while (ri < ranges.length && pos >= ranges[ri][1]) ri++; // drop finished ranges
    if (html[i] === '<') {
      close();
      const end = html.indexOf('>', i);
      const stop = end === -1 ? html.length : end + 1;
      out += html.slice(i, stop);
      i = stop;
      continue;
    }
    // One text char, which may be an HTML entity (&amp; / &lt; / &gt;).
    let len = 1;
    if (html[i] === '&') {
      const semi = html.indexOf(';', i);
      if (semi !== -1 && semi - i <= 10) len = semi - i + 1;
    }
    const inMatch = ri < ranges.length && pos >= ranges[ri][0] && pos < ranges[ri][1];
    if (inMatch && !open) {
      out += `<mark class="mk${ri === activeIdx ? ' on' : ''}">`;
      open = true;
    } else if (!inMatch && open) {
      close();
    }
    out += html.slice(i, i + len);
    i += len;
    pos++;
  }
  close();
  return out;
}
