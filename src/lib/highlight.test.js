import { describe, expect, it } from 'vitest';
import { highlightBody, highlightHttp } from './highlight.js';

// The editor overlay relies on highlighting never changing the text itself.
const text = (html) =>
  html
    .replace(/<[^>]+>/g, '')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&amp;/g, '&');

describe('highlightHttp', () => {
  const samples = [
    'GET /a?b=<c>&d HTTP/1.1\nHost: x\nCookie: a=b; c=d\n\n{"k": [1, true, null, "s\\"q"]}',
    'HTTP/1.1 404 Not Found\nContent-Type: text/html\n\n<p class="x">a & b</p>',
    'weird first line\nno-colon-header\n\nplain body',
    'POST / HTTP/1.1\n\n',
  ];

  it.each(samples)('keeps the characters of %#', (raw) => {
    expect(text(highlightHttp(raw))).toBe(raw);
  });

  it('escapes markup from the message', () => {
    const html = highlightHttp('HTTP/1.1 200 OK\n\n<script>alert(1)</script>');
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;');
  });

  it('colours status and method', () => {
    expect(highlightHttp('HTTP/1.1 503 Busy')).toContain('s-5xx');
    expect(highlightHttp('DELETE /x HTTP/1.1')).toContain('m-delete');
  });

  it('marks JSON keys and values', () => {
    const html = highlightBody('{"a": 1, "b": "x", "c": false}');
    expect(html).toContain('<span class="hl-key">"a"</span>');
    expect(html).toContain('<span class="hl-num">1</span>');
    expect(html).toContain('<span class="hl-str">"x"</span>');
    expect(html).toContain('<span class="hl-lit">false</span>');
  });

  it('skips colouring past the limit but honours a higher one', () => {
    // A JSON body just over the default 200 KB cap: not coloured by default...
    const big = '{"k":"' + 'a'.repeat(210_000) + '"}';
    expect(highlightBody(big)).not.toContain('hl-');
    // ...but coloured when the caller (the read-only viewer) raises the limit.
    expect(highlightBody(big, 2_000_000)).toContain('hl-key');
    // highlightHttp forwards the limit to the body.
    const msg = 'HTTP/1.1 200 OK\nContent-Type: application/json\n\n' + big;
    expect(highlightHttp(msg)).not.toContain('hl-key');
    expect(highlightHttp(msg, 2_000_000)).toContain('hl-key');
  });
});
