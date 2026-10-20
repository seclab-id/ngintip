import { describe, expect, it } from 'vitest';
import { contentType, formatMarkup, htmlBody, MAX_LINE, prettyBody, prettyJson, softWrapLong, splitMessage } from './message.js';

describe('splitMessage / contentType', () => {
  it('splits at the first blank line', () => {
    expect(splitMessage('A\nB: 1\n\nbody\n\nmore')).toEqual({ head: 'A\nB: 1', body: 'body\n\nmore' });
    expect(splitMessage('only head')).toEqual({ head: 'only head', body: '' });
  });

  it('finds the content type case-insensitively', () => {
    expect(contentType('X\ncontent-TYPE:  Application/JSON; charset=utf-8')).toBe('application/json; charset=utf-8');
    expect(contentType('X\nY: z')).toBe('');
  });
});

describe('prettyJson / prettyBody', () => {
  it('re-indents JSON bodies', () => {
    expect(prettyJson('H\n\n{"a":[1,2]}')).toBe('H\n\n{\n  "a": [\n    1,\n    2\n  ]\n}');
    expect(prettyJson('H\n\nnot json')).toBeNull();
    expect(prettyJson('H\n\n')).toBeNull();
  });

  it('formats XML and HTML, but not plain text', () => {
    // Leaf text stays inline (<b>1</b>); only element children get their own lines.
    expect(prettyBody('H\nContent-Type: application/xml\n\n<a><b>1</b></a>')).toBe('H\nContent-Type: application/xml\n\n<a>\n  <b>1</b>\n</a>');
    expect(prettyBody('H\nContent-Type: text/plain\n\n<a><b>1</b></a>')).toBeNull();
    expect(prettyBody('H\n\nhello')).toBeNull();
  });

  it('skips formatting only genuinely huge bodies (keeps opening a request snappy)', () => {
    const bigJson = 'H\n\n' + '[' + '1,'.repeat(300_000) + '1]'; // > 512 KB
    expect(bigJson.length).toBeGreaterThan(512 * 1024);
    expect(prettyJson(bigJson)).toBeNull();
    expect(prettyBody(bigJson)).toBeNull();
    const bigHtml = 'H\nContent-Type: text/html\n\n<div>' + 'x'.repeat(600_000) + '</div>';
    expect(prettyBody(bigHtml)).toBeNull();
    // A moderately large (view-capped) XML, like a ~280 KB SAP $metadata, still formats.
    const midXml = 'H\nContent-Type: application/xml\n\n<r>' + '<i>x</i>'.repeat(30_000) + '</r>';
    expect(midXml.length).toBeGreaterThan(200_000);
    expect(midXml.length).toBeLessThan(512 * 1024);
    expect(prettyBody(midXml)).not.toBeNull();
    // Small bodies still format.
    expect(prettyJson('H\n\n{"a":1}')).toBe('H\n\n{\n  "a": 1\n}');
  });
});

describe('softWrapLong', () => {
  it('leaves normal multi-line text untouched', () => {
    const text = 'GET / HTTP/1.1\nHost: x\n\n{"a":1}\n<div>hi</div>';
    expect(softWrapLong(text)).toBe(text);
    expect(softWrapLong('')).toBe('');
    expect(softWrapLong(undefined)).toBe('');
  });

  it('chunks a single pathologically long line so layout never sees it whole', () => {
    // A 400 KB single line, like an inline data: URI or a minified blob.
    const blob = 'A'.repeat(400_000);
    const out = softWrapLong(blob);
    const lines = out.split('\n');
    expect(lines.length).toBe(Math.ceil(400_000 / MAX_LINE));
    expect(Math.max(...lines.map((l) => l.length))).toBe(MAX_LINE);
    // No characters are lost, only newlines inserted.
    expect(out.replace(/\n/g, '')).toBe(blob);
  });

  it('only breaks the long line, keeping the head and short lines intact', () => {
    const head = 'HTTP/1.1 520 Origin Error\nContent-Type: text/html\n\n';
    const raw = head + '<img src="data:x;base64,' + 'Q'.repeat(300_000) + '">';
    const out = softWrapLong(raw);
    // The head is preserved verbatim (still splits into head/body at the blank line).
    expect(out.startsWith(head)).toBe(true);
    expect(out.indexOf('\n\n')).toBe(raw.indexOf('\n\n'));
    // Every rendered line is now bounded.
    expect(Math.max(...out.split('\n').map((l) => l.length))).toBeLessThanOrEqual(MAX_LINE);
  });

  it('does not split a UTF-16 surrogate pair at a chunk boundary', () => {
    // Put an emoji exactly on the boundary; the chunk must not end mid-pair.
    const line = 'x'.repeat(MAX_LINE - 1) + '😀' + 'y'.repeat(10);
    const out = softWrapLong(line);
    expect(out.replace(/\n/g, '')).toBe(line);
    for (const l of out.split('\n')) expect([...l].join('')).toBe(l); // no lone surrogates
  });
});

describe('formatMarkup', () => {
  it('handles void elements, comments, quoted > and verbatim script', () => {
    const out = formatMarkup('<!--c1--><!--c2--><div a="1>2"><br><img src=x><script>if (a<b) go()</script></div>');
    expect(out.split('\n')).toEqual([
      '<!--c1-->',
      '<!--c2-->',
      '<div a="1>2">',
      '  <br>',
      '  <img src=x>',
      '  <script>',
      'if (a<b) go()',
      '  </script>',
      '</div>',
    ]);
  });

  it('does not go negative on stray closing tags', () => {
    expect(formatMarkup('</x><a>t</a>')).toBe('</x>\n<a>t</a>');
  });

  it('keeps pure-text leaves inline, self-closing and empty tags intact', () => {
    expect(formatMarkup('<note><to>Tove</to><empty/><body></body></note>').split('\n')).toEqual([
      '<note>',
      '  <to>Tove</to>',
      '  <empty/>',
      '  <body></body>',
      '</note>',
    ]);
  });
});

describe('prettyBody for XML', () => {
  it('prettifies XML and keeps leaf text inline', () => {
    const out = prettyBody('H\nContent-Type: application/xml\n\n<?xml version="1.0"?><n><a>x</a><b>y</b></n>');
    expect(out).toBe('H\nContent-Type: application/xml\n\n<?xml version="1.0"?>\n<n>\n  <a>x</a>\n  <b>y</b>\n</n>');
  });

  it('prettifies XML served under an odd MIME when it declares <?xml', () => {
    expect(prettyBody('H\nContent-Type: text/plain\n\n<?xml version="1.0"?><a><b>1</b></a>')).not.toBeNull();
    // Plain text under text/plain is still left alone.
    expect(prettyBody('H\nContent-Type: text/plain\n\n<hello not xml')).toBeNull();
  });
});

describe('htmlBody', () => {
  it('returns HTML bodies only', () => {
    expect(htmlBody('H\nContent-Type: text/html\n\n<p>x</p>')).toBe('<p>x</p>');
    expect(htmlBody('H\n\n<!doctype html><p>x</p>')).toBe('<!doctype html><p>x</p>');
    expect(htmlBody('H\nContent-Type: application/json\n\n{}')).toBeNull();
  });
});
