import { describe, expect, it } from 'vitest';
import { shellQuote, toCurl } from './curl.js';

describe('toCurl', () => {
  it('builds a POST with headers and body', () => {
    const raw = 'POST /api?x=1 HTTP/1.1\nHost: example.com\nContent-Length: 99\nAccept-Encoding: gzip\n\n{"a":"it\'s"}';
    expect(toCurl(raw, 'https://example.com')).toBe(
      `curl -sk -i --compressed -H 'Host: example.com' -H 'Accept-Encoding: gzip' --data-binary '{"a":"it'\\''s"}' 'https://example.com/api?x=1'`,
    );
  });

  it('adds -X only when the method is not implied', () => {
    expect(toCurl('GET / HTTP/1.1\nHost: h\n\n', 'http://h')).toBe(`curl -sk -i -H 'Host: h' 'http://h/'`);
    expect(toCurl('DELETE /x HTTP/1.1\n\n', 'http://h/')).toContain('-X DELETE');
    expect(toCurl('PUT /x HTTP/1.1\n\nbody', 'http://h')).toContain("-X PUT");
  });

  it('keeps absolute targets and skips placeholders and viewer notes', () => {
    expect(toCurl('GET http://other/x HTTP/1.1\n\n', 'http://h')).toMatch(/'http:\/\/other\/x'$/);
    expect(toCurl('POST / HTTP/1.1\n\n[binary body: 12 bytes]', 'http://h')).not.toContain('--data-binary');
    const noted = toCurl('POST / HTTP/1.1\n\nabc\n\n[Intip: body 99 byte, yang disimpan hanya 3 byte pertama]', 'http://h');
    expect(noted).toContain("--data-binary 'abc'");
  });

  it('quotes for the shell', () => {
    expect(shellQuote(`a'b "c" $d`)).toBe(`'a'\\''b "c" $d'`);
  });
});
