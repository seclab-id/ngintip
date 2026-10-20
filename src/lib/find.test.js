import { describe, expect, it } from 'vitest';
import { matchRanges, markHtmlRanges } from './find.js';
import { highlightHttp } from './highlight.js';

// Decode highlighted HTML back to its plain text (drop tags, undo entities), the
// same way the editor overlay relies on highlighting never changing the text.
const text = (html) =>
  html
    .replace(/<[^>]+>/g, '')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&amp;/g, '&');

describe('matchRanges', () => {
  it('finds every case-insensitive occurrence in order', () => {
    // 'aXaxAX' lowercases to 'axaxax' -> 'ax' at 0, 2, 4.
    const { ranges, count } = matchRanges('aXaxAX', 'ax');
    expect(count).toBe(3);
    expect(ranges).toEqual([
      [0, 2],
      [2, 4],
      [4, 6],
    ]);
  });

  it('is empty for a blank query', () => {
    expect(matchRanges('anything', '')).toEqual({ ranges: [], count: 0 });
  });

  it('does not overlap matches', () => {
    // 'aaa' with query 'aa' -> one match, then next search starts after it.
    expect(matchRanges('aaaa', 'aa').ranges).toEqual([
      [0, 2],
      [2, 4],
    ]);
  });

  it('escapes regex metacharacters in plain (non-regex) mode', () => {
    expect(matchRanges('a.b axb', 'a.b').ranges).toEqual([[0, 3]]); // '.' is literal, not "any char"
  });

  it('honours caseSensitive', () => {
    expect(matchRanges('aXAx', 'ax', { caseSensitive: true }).count).toBe(0);
    expect(matchRanges('aXax', 'ax', { caseSensitive: true }).ranges).toEqual([[2, 4]]);
  });

  it('supports regex mode', () => {
    const { ranges } = matchRanges('id=12, id=345', 'id=\\d+', { regex: true });
    expect(ranges).toEqual([[0, 5], [7, 13]]);
  });

  it('supports whole-word', () => {
    const { ranges } = matchRanges('cat category cat', 'cat', { word: true });
    expect(ranges).toEqual([[0, 3], [13, 16]]); // not the "cat" inside "category"
  });

  it('reports an invalid regex instead of throwing', () => {
    const r = matchRanges('x', '(', { regex: true });
    expect(r).toEqual({ ranges: [], count: 0, error: true });
  });
});

describe('markHtmlRanges', () => {
  it('returns the html untouched when there are no ranges', () => {
    const html = highlightHttp('GET / HTTP/1.1');
    expect(markHtmlRanges(html, [], 0)).toBe(html);
    expect(markHtmlRanges(html, null, 0)).toBe(html);
  });

  it('keeps the underlying text and the syntax spans while marking', () => {
    const raw = 'GET /a?b=<c>&d HTTP/1.1\nHost: x\n\n{"k": "value"}';
    const html = highlightHttp(raw);
    const { ranges } = matchRanges(raw, 'value');
    const marked = markHtmlRanges(html, ranges, 0);
    // Colours (spans) survive.
    expect(marked).toContain('hl-');
    // A mark was inserted, and the active one is flagged.
    expect(marked).toContain('<mark class="mk on">');
    // Text content is unchanged once marks + spans are stripped.
    expect(text(marked)).toBe(raw);
  });

  it('marks a match that spans a syntax span boundary', () => {
    // Search across the method/path boundary: "GET /" straddles two spans (and
    // the bare space between them), so it becomes several adjacent marks whose
    // combined text is still exactly the query.
    const raw = 'GET / HTTP/1.1';
    const html = highlightHttp(raw);
    const { ranges } = matchRanges(raw, 'GET /');
    const marked = markHtmlRanges(html, ranges, 0);
    const marks = [...marked.matchAll(/<mark class="mk on">([\s\S]*?)<\/mark>/g)];
    expect(marks.length).toBeGreaterThan(1);
    expect(marks.map((m) => text(m[1])).join('')).toBe('GET /');
    expect(text(marked)).toBe(raw);
  });

  it('treats an entity (&amp;) as a single matched character', () => {
    const raw = 'GET /a?x=1&y=2 HTTP/1.1';
    const html = highlightHttp(raw);
    // Query straddles the & which is encoded as &amp; in the html.
    const { ranges } = matchRanges(raw, '1&y');
    const marked = markHtmlRanges(html, ranges, 0);
    expect(marked).toContain('&amp;');
    expect(text(marked)).toBe(raw);
    // Exactly the matched run is wrapped: decode just the marked slice.
    const inner = marked.match(/<mark class="mk on">([\s\S]*?)<\/mark>/);
    expect(inner && text(inner[1])).toBe('1&y');
  });

  it('only flags the active match with .on', () => {
    const raw = 'abc abc abc';
    const html = highlightHttp(raw);
    const { ranges } = matchRanges(raw, 'abc');
    const marked = markHtmlRanges(html, ranges, 1);
    expect((marked.match(/<mark class="mk">/g) || []).length).toBe(2);
    expect((marked.match(/<mark class="mk on">/g) || []).length).toBe(1);
  });
});
