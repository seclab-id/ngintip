// The note renderer. The formatting cases matter, but the injection cases are the
// reason this file exists: its output goes through `{@html}`, the app has no CSP, and a
// note in a proxy tool regularly holds a payload copied straight out of a response.
import { describe, expect, it } from 'vitest';
import { MD_LIMIT, renderMarkdown, safeHref } from './markdown.js';

const md = (s) => renderMarkdown(s);

describe('markdown: nothing user-written becomes an element', () => {
  it('renders a saved XSS payload as text', () => {
    const html = md('found it: <img src=x onerror=alert(1)>');
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img src=x onerror=alert(1)&gt;');
  });

  it('escapes a script tag, open or closing', () => {
    const html = md('<script>alert(document.cookie)</script>');
    expect(html).not.toContain('<script');
    expect(html).not.toContain('</script>');
    expect(html).toContain('&lt;script&gt;');
  });

  it('keeps quotes out of attribute position', () => {
    // The classic break-out: a quote meant to end href and start a new attribute. It
    // may well appear in the output as visible text - that is the point, nothing is
    // swallowed - but never with a live quote around it.
    const html = md('[x](https://a.test/"onmouseover="alert(1))');
    expect(html).not.toContain('onmouseover="');
    expect(html).not.toContain('onmouseover=\'');
    expect(html).toContain('&quot;onmouseover=&quot;');
    // The only unescaped quotes in the result are the ones this renderer writes.
    expect(html.replace(/(?:href|rel|type|class)="[^"]*"/g, '')).not.toContain('"');
  });

  it('does not let a payload inside a code span escape', () => {
    const html = md('try `"><svg onload=alert(1)>` next');
    expect(html).not.toContain('<svg');
    expect(html).toContain('<code>&quot;&gt;&lt;svg onload=alert(1)&gt;</code>');
  });

  it('does not let a payload inside a fenced block escape', () => {
    const html = md('```\n<iframe src=javascript:alert(1)></iframe>\n```');
    expect(html).not.toContain('<iframe');
    expect(html).toContain('&lt;iframe src=javascript:alert(1)&gt;');
  });

  it('refuses a javascript: link and keeps the source visible as text', () => {
    // Both forms: with parens in the URL and without, since the URL pattern accepts
    // balanced parens. Neither becomes a link, and neither loses what was written -
    // a payload in a note is usually there on purpose.
    for (const src of ['[click me](javascript:alert(1))', '[click me](javascript:alert)']) {
      const html = md(src);
      expect(html).not.toContain('<a ');
      expect(html).not.toContain('href');
      expect(html).toContain('click me');
      expect(html).toContain('javascript:alert');
    }
  });

  it('refuses data:, file:, vbscript: and a protocol-relative URL', () => {
    for (const url of ['data:text/html,<script>alert(1)</script>', 'file:///etc/passwd', 'vbscript:msgbox', '//evil.test/x']) {
      expect(safeHref(url)).toBe(null);
      const html = md(`[x](${url})`);
      expect(html).not.toContain('<a ');
      expect(html).not.toContain('<script');
    }
  });

  it('ignores a case-mangled or padded javascript: scheme', () => {
    for (const url of ['JaVaScRiPt:alert(1)', '  javascript:alert(1)', 'java\tscript:alert(1)']) {
      expect(safeHref(url)).toBe(null);
    }
  });

  it('accepts http, https and mailto', () => {
    expect(safeHref('https://seclab.id/x?a=1&b=2')).toBe('https://seclab.id/x?a=1&b=2');
    expect(safeHref('http://127.0.0.1:8080/')).toBe('http://127.0.0.1:8080/');
    expect(safeHref('mailto:someone@example.test')).toBe('mailto:someone@example.test');
  });

  it('cannot be fooled by a forged placeholder sentinel', () => {
    // The internal parking slots use U+0000, which is stripped from the source first.
    const html = md('\u00000\u0000 and `real`');
    expect(html).toContain('<code>real</code>');
    expect(html).not.toContain('\u0000');
  });

  it('leaves an image as a plain link, never an <img> that fetches', () => {
    // A note full of remote images would leak which notes you opened, and when.
    const html = md('![shot](https://evil.test/track.png)');
    expect(html).not.toContain('<img');
    expect(html).toContain('<a href="https://evil.test/track.png"');
  });
});

describe('markdown: formatting', () => {
  it('renders headings', () => {
    expect(md('# One')).toBe('<h1>One</h1>');
    expect(md('### Three')).toBe('<h3>Three</h3>');
    expect(md('#NotAHeading')).toContain('<p>#NotAHeading</p>');
  });

  it('renders emphasis, including bold inside italic', () => {
    expect(md('**bold**')).toBe('<p><strong>bold</strong></p>');
    expect(md('*italic*')).toBe('<p><em>italic</em></p>');
    expect(md('_italic_')).toBe('<p><em>italic</em></p>');
    expect(md('~~gone~~')).toBe('<p><del>gone</del></p>');
    expect(md('***both***')).toBe('<p><strong><em>both</em></strong></p>');
  });

  it('leaves snake_case and a bare asterisk alone', () => {
    // Pentest notes are full of identifiers; turning them into italics would be worse
    // than not supporting emphasis at all.
    expect(md('user_id and session_token')).toBe('<p>user_id and session_token</p>');
    expect(md('2 * 3 * 4')).toBe('<p>2 * 3 * 4</p>');
  });

  it('renders a fenced block with its language as a class', () => {
    const html = md('```http\nGET / HTTP/1.1\n```');
    expect(html).toContain('class="md-lang-http"');
    expect(html).toContain('GET / HTTP/1.1');
  });

  it('keeps an unterminated fence instead of dropping the text', () => {
    expect(md('```\nstill here')).toContain('still here');
  });

  it('renders nested lists', () => {
    const html = md('- a\n  - b\n- c');
    expect(html).toBe('<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>');
  });

  it('renders an ordered list', () => {
    expect(md('1. one\n2. two')).toBe('<ol><li>one</li><li>two</li></ol>');
  });

  it('renders task lists as read-only checkboxes', () => {
    const html = md('- [ ] todo\n- [x] done');
    expect(html).toContain('<input type="checkbox" disabled /> todo');
    expect(html).toContain('<input type="checkbox" disabled checked /> done');
  });

  it('renders a blockquote, with markdown inside it', () => {
    expect(md('> **note**')).toBe('<blockquote><p><strong>note</strong></p></blockquote>');
  });

  it('renders a table with alignment', () => {
    const html = md('| a | b |\n| :-- | --: |\n| 1 | 2 |');
    expect(html).toContain('<th class="md-l">a</th>');
    expect(html).toContain('<th class="md-r">b</th>');
    expect(html).toContain('<td class="md-l">1</td>');
  });

  it('needs the delimiter row before treating pipes as a table', () => {
    // Notes hold raw lines with pipes in them (shell, payloads); those are not tables.
    expect(md('cat /etc/passwd | grep root')).toBe('<p>cat /etc/passwd | grep root</p>');
  });

  it('renders a rule', () => {
    expect(md('---')).toBe('<hr />');
  });

  it('joins a wrapped paragraph and honours a hard break', () => {
    expect(md('one\ntwo')).toBe('<p>one two</p>');
    expect(md('one  \ntwo')).toBe('<p>one<br />two</p>');
  });

  it('autolinks a bare URL', () => {
    expect(md('see https://seclab.id now')).toContain('<a href="https://seclab.id"');
  });

  it('is empty for empty or blank input', () => {
    expect(md('')).toBe('');
    expect(md(null)).toBe('');
    expect(md('   \n\n  ')).toBe('');
  });

  it('survives pathological input without hanging or overflowing the stack', () => {
    // Found by a stress run, not by imagination: 5k `>` recursed once per level and
    // blew the stack, and unbounded lazy inline patterns made 10k brackets quadratic.
    // A note in this app can hold any payload, so none of this may take the view down.
    const cases = [
      '>'.repeat(5000), // was: RangeError: Maximum call stack size exceeded
      '['.repeat(10000) + ']'.repeat(10000), // was: 87 ms, growing quadratically
      '*'.repeat(50000),
      '**a'.repeat(20000),
      '`a'.repeat(20000),
      '~~a'.repeat(20000),
      '_a'.repeat(20000),
      '[a]('.repeat(10000),
      '```\nx\n'.repeat(1000),
      Array.from({ length: 2000 }, (_, i) => ' '.repeat(i % 40) + '- x').join('\n'),
    ];
    for (const src of cases) {
      expect(() => md(src)).not.toThrow();
    }
  });

  it('caps blockquote nesting instead of recursing per level', () => {
    const html = md(`${'>'.repeat(20)} deep`);
    const depth = (html.match(/<blockquote>/g) ?? []).length;
    expect(depth).toBeLessThanOrEqual(8);
    expect(html).toContain('deep'); // and the text is still there
  });

  it('falls back to preformatted text for a giant note', () => {
    const html = md('#'.repeat(MD_LIMIT + 1));
    expect(html.startsWith('<pre class="md-raw">')).toBe(true);
  });
});
