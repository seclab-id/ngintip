import { describe, expect, it } from 'vitest';
import { contentType, formatMarkup, htmlBody, MAX_LINE, prettyBody, prettyJson, prettyJsonText, softWrapLong, splitMessage } from './message.js';

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

/** The JSON text with whitespace between tokens removed; whitespace INSIDE a string is
 *  kept, because there it is content. Lets a test compare two layouts of one document. */
function stripJsonSpace(src) {
  let out = '';
  for (let i = 0; i < src.length; i += 1) {
    const c = src[i];
    if (c === '"') {
      let j = i + 1;
      for (; j < src.length; j += 1) {
        if (src[j] === '\\') {
          j += 1;
          continue;
        }
        if (src[j] === '"') break;
      }
      out += src.slice(i, j + 1);
      i = j;
    } else if (c !== ' ' && c !== '\t' && c !== '\n' && c !== '\r') {
      out += c;
    }
  }
  return out;
}

describe('prettyJson / prettyBody', () => {
  it('re-indents JSON bodies', () => {
    expect(prettyJson('H\n\n{"a":[1,2]}')).toBe('H\n\n{\n  "a": [\n    1,\n    2\n  ]\n}');
    expect(prettyJson('H\n\nnot json')).toBeNull();
    expect(prettyJson('H\n\n')).toBeNull();
  });

  // Reported from an engagement: a Repeater request with c in its JSON body came back
  // as `c` after the project was closed and opened again. The cause was
  // `JSON.stringify(JSON.parse(body), null, 2)`, which does not re-indent a body, it
  // normalizes it. Re-indenting is a whitespace change; normalizing rewrites the payload.
  //
  // `B` is one backslash. The fixtures build their escapes from it so that what this file
  // asserts is the byte sequence that reaches the parser, not an escape this file's own
  // reader has already resolved.
  const B = '\\';

  it('re-indents a JSON body WITHOUT rewriting what is in it', () => {
    // The reported case: an escaped character used against a filter that only reads the
    // literal one. Turning it into `c` sends a different request than the one on screen.
    expect(prettyJson(`H\n\n{"user":"ad${B}u0063min"}`)).toBe(`H\n\n{\n  "user": "ad${B}u0063min"\n}`);

    // An escaped solidus is legal JSON and means something to whoever reads the path.
    expect(prettyJson(`H\n\n{"path":"${B}/etc${B}/passwd"}`)).toBe(`H\n\n{\n  "path": "${B}/etc${B}/passwd"\n}`);

    // Two keys with one name is how two parsers are told apart. Keeping only the last one
    // throws away the entire test.
    expect(prettyJson('H\n\n{"role":"user","role":"admin"}')).toBe('H\n\n{\n  "role": "user",\n  "role": "admin"\n}');

    // Past 2^53 a round trip through a double silently changes the number.
    expect(prettyJson('H\n\n{"id":12345678901234567890}')).toBe('H\n\n{\n  "id": 12345678901234567890\n}');

    // How a number is spelled is part of the payload too.
    expect(prettyJson('H\n\n{"n":1e3,"z":-0,"f":1.50}')).toBe('H\n\n{\n  "n": 1e3,\n  "z": -0,\n  "f": 1.50\n}');

    // A string holding JSON punctuation, an escaped quote and an escape sequence is
    // copied from quote to quote, so none of it is mistaken for structure.
    expect(prettyJson(`H\n\n{"q":"a,b:{c}${B}"d${B}" ${B}n${B}u00e9"}`)).toBe(
      `H\n\n{\n  "q": "a,b:{c}${B}"d${B}" ${B}n${B}u00e9"\n}`,
    );
  });

  it('matches the familiar layout and settles after one pass', () => {
    const once = prettyJson('H\n\n{"a":[1,{"b":null,"c":[]},{}],"d":{}}');
    // Empty containers stay on one line, the way JSON.stringify writes them.
    expect(once).toContain('[]');
    expect(once).toContain('{}');
    expect(once).toBe('H\n\n{\n  "a": [\n    1,\n    {\n      "b": null,\n      "c": []\n    },\n    {}\n  ],\n  "d": {}\n}');
    // Beautify has to settle: the Repeater re-beautifies a tab every time that view is
    // mounted, so a second pass that changed anything would rewrite the request forever.
    expect(prettyJson(once)).toBe(once);
  });

  it('keeps every token when it re-indents, token by token', () => {
    const bodies = [
      `{"user":"ad${B}u0063min","path":"${B}/x","role":"a","role":"b"}`,
      '[1,2,[3,[4]],{"k":"v"},[],{}]',
      `{"s":"${B}u0000${B}b${B}f${B}r${B}n${B}t${B}"${B}${B}","t":true,"f":false,"n":null}`,
      '{"big":12345678901234567890,"neg":-1.5e-7,"zero":-0}',
      '  {  "spaced" :  [ 1 , 2 ]  }  ',
      '"just a string"',
      '42',
    ];
    for (const body of bodies) {
      const out = prettyJson(`H\n\n${body}`);
      expect(out, body).not.toBeNull();
      // The invariant: with the whitespace BETWEEN tokens removed, the re-indented body
      // is character for character the body that went in. Everything the old round trip
      // rewrote shows up here as a difference.
      expect(stripJsonSpace(out.slice('H\n\n'.length)), body).toBe(stripJsonSpace(body));
    }
  });

  it('re-indents a bare document too, for a WebSocket frame with no headers', () => {
    // WsView showed a frame through the same round trip, so a captured frame with an
    // escape in it read differently here than it did on the wire. Display only, but a
    // proxy's Pretty view must not misquote the traffic it captured.
    expect(prettyJsonText(`{"user":"ad${B}u0063min"}`)).toBe(`{\n  "user": "ad${B}u0063min"\n}`);
    expect(prettyJsonText('not json')).toBeNull();
    expect(prettyJsonText('')).toBeNull();
    expect(prettyJsonText(null)).toBeNull();
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

  // The inline-leaf check reads only the closing token plus the character after it, so
  // it may not confuse `</toxic>` with `</to>`. It used to lowercase the whole rest of
  // the document to decide this, which is what made formatting quadratic.
  it('does not mistake a longer tag name for the closing tag', () => {
    // `</toxic>` is still a closing tag, so it drops the depth before being pushed.
    expect(formatMarkup('<to>Tove</toxic>').split('\n')).toEqual(['<to>', '  Tove', '</toxic>']);
    expect(formatMarkup('<to>Tove</to >')).toBe('<to>Tove</to >');
    expect(formatMarkup('<to>Tove</to\n>')).toBe('<to>Tove</to\n>');
  });

  it('survives a closing tag cut off at the end of the body', () => {
    // A truncated capture: the character after the close token does not exist.
    expect(formatMarkup('<to>Tove</to')).toBe('<to>Tove</to');
    expect(formatMarkup('<script>unclosed')).toBe('<script>\nunclosed');
  });

  it('formats a large markup body in linear time', () => {
    // Guard, not a benchmark. Both lookups in the element loop used to re-scan the whole
    // document, so a 380 KB HTML response took ~1.4 s through prettyBody and was paid on
    // every mount of the History detail pane, even in Raw mode. Linear is ~8 ms here, so
    // a one-second budget fails only if the quadratic behaviour comes back.
    const block = (i) =>
      `<div class="row r${i}" data-id="${i}"><span class="k">Field ${i}</span><span class="v">value-${i}</span></div>`;
    let body = '';
    for (let i = 0; body.length < 380 * 1024; i++) body += block(i);
    const raw = `HTTP/1.1 200 OK\nContent-Type: text/html\n\n<html><body>${body}</body></html>`;
    const started = Date.now();
    const out = prettyBody(raw);
    expect(Date.now() - started).toBeLessThan(1000);
    expect(out).toContain('<span class="k">Field 0</span>');
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

describe('Beautify copies text nodes instead of collapsing them', () => {
  // Beautify is not only a viewer: Repeater, Intercept and the Capture editor run it on
  // what they are about to SEND. It used to rewrite every text node with
  // `replace(/\s+/g, ' ').trim()`, so pressing Beautify silently changed the payload.
  const xml = (b) => `H\nContent-Type: application/xml\n\n${b}`;
  const bodyOf = (out) => out.slice(out.indexOf('\n\n') + 2);

  it('keeps the spaces around a value', () => {
    // The reported shape: a padded value sent to a target that trims on one side only.
    expect(bodyOf(prettyBody(xml('<r><pw> secret </pw></r>')))).toBe('<r>\n  <pw> secret </pw>\n</r>');
  });

  it('keeps a value that is only whitespace', () => {
    expect(bodyOf(prettyBody(xml('<r><pad> </pad><n>1</n></r>')))).toBe('<r>\n  <pad> </pad>\n  <n>1</n>\n</r>');
  });

  it('keeps a run of spaces inside text', () => {
    expect(bodyOf(prettyBody(xml('<r><t>a  b</t></r>')))).toBe('<r>\n  <t>a  b</t>\n</r>');
  });

  it('keeps text that is not a leaf of its own element', () => {
    expect(bodyOf(prettyBody(xml('<r>  lead  <b>x</b></r>')))).toBe('<r>\n    lead  \n  <b>x</b>\n</r>');
  });

  it('still re-indents, because a line break between tags is the layout it replaces', () => {
    const out = bodyOf(prettyBody(xml('<r>\n<a>1</a>\n   <b>2</b>\n</r>')));
    expect(out).toBe('<r>\n  <a>1</a>\n  <b>2</b>\n</r>');
  });
});

describe('a body the backend cut at its view limit can still be read', () => {
  // raw.rs caps the default view at VIEW_LIMIT (256 KB) and appends a line saying so.
  // The JSON in front of that line is a document with its tail missing, usually cut
  // mid-string, so JSON.parse fails and Pretty used to disappear at exactly the size
  // where reading one 256 KB line unformatted is hopeless.
  const NOTE = '\n\n[truncated: showing the first 262144 bytes; use Show all for the rest]';

  it('re-indents the part that arrived and keeps the note', () => {
    const out = prettyBody(`H\nContent-Type: application/json\n\n{"a":[1,2],"b":"half of a stri${NOTE}`);
    expect(out).toBe(
      `H\nContent-Type: application/json\n\n{\n  "a": [\n    1,\n    2\n  ],\n  "b": "half of a stri${NOTE}`,
    );
  });

  it('leaves a whole body exactly as it was', () => {
    expect(prettyBody('H\nContent-Type: application/json\n\n{"a":1}')).toBe('H\nContent-Type: application/json\n\n{\n  "a": 1\n}');
  });

  it('does not guess at a cut body that is not declared JSON', () => {
    // A truncated script opening with `{` is not a JSON document, and re-indenting it
    // would be this viewer inventing a shape the server never claimed.
    expect(prettyBody(`H\nContent-Type: application/javascript\n\n{a:1,b:"unclos${NOTE}`)).toBeNull();
  });

  it('does not treat the note as broken JSON when the body is not cut at all', () => {
    expect(prettyBody('H\nContent-Type: application/json\n\n{"a":"not clos')).toBeNull();
  });

  it('keeps the note out of a truncated XML body', () => {
    // `<b>` never closes, so it is not an inline leaf and its text goes on its own
    // indented line. The point of the assertion is the tail: the note is the viewer
    // talking, so it is held aside and put back, never indented as if it were markup.
    const out = prettyBody(`H\nContent-Type: application/xml\n\n<r><a>1</a><b>2${NOTE}`);
    expect(out).toBe(`H\nContent-Type: application/xml\n\n<r>\n  <a>1</a>\n  <b>\n    2${NOTE}`);
  });
});
