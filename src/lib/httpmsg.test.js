import { describe, expect, it } from 'vitest';
import { addBody, addHeader, byteLength, fixContentLength, getHeader, hasBody, setBody, setHeader, splitMsg } from './httpmsg.js';

const REQ = 'GET /a HTTP/1.1\nHost: x\n\n';
const POST = 'POST /a HTTP/1.1\nHost: x\nContent-Length: 3\n\nabc';

describe('httpmsg', () => {
  it('splits and detects body', () => {
    expect(splitMsg(POST)).toEqual({ head: 'POST /a HTTP/1.1\nHost: x\nContent-Length: 3', body: 'abc', hadBody: true });
    expect(hasBody(REQ)).toBe(false);
    expect(hasBody(POST)).toBe(true);
  });

  it('adds a header before the body', () => {
    expect(addHeader(REQ, 'X-A', '1')).toBe('GET /a HTTP/1.1\nHost: x\nX-A: 1\n\n');
    expect(addHeader(POST, 'X-A', '1')).toBe('POST /a HTTP/1.1\nHost: x\nContent-Length: 3\nX-A: 1\n\nabc');
  });

  it('setHeader adds or replaces (case-insensitive), never the start line', () => {
    // Adds when missing.
    expect(setHeader(REQ, 'X-Forwarded-For', '127.0.0.1')).toBe('GET /a HTTP/1.1\nHost: x\nX-Forwarded-For: 127.0.0.1\n\n');
    // Replaces an existing value, matching the header name case-insensitively.
    const withUA = 'GET /a HTTP/1.1\nHost: x\nuser-agent: old\n\n';
    expect(setHeader(withUA, 'User-Agent', 'new')).toBe('GET /a HTTP/1.1\nHost: x\nUser-Agent: new\n\n');
    // Keeps the body and doesn't touch a look-alike request line.
    expect(setHeader(POST, 'Host', 'y')).toBe('POST /a HTTP/1.1\nHost: y\nContent-Length: 3\n\nabc');
  });

  it('getHeader reads a value case-insensitively, else empty', () => {
    expect(getHeader('GET / HTTP/1.1\nUser-Agent:  curl/8 \n\n', 'user-agent')).toBe('curl/8');
    expect(getHeader(REQ, 'Authorization')).toBe('');
  });

  it('setBody sets body, Content-Length and Content-Type together', () => {
    const out = setBody('POST /a HTTP/1.1\nHost: x\n\n', '{"k":1}', 'application/json');
    expect(splitMsg(out).body).toBe('{"k":1}');
    expect(getHeader(out, 'Content-Length')).toBe('7');
    expect(getHeader(out, 'Content-Type')).toBe('application/json');
    // Replaces an existing body and its Content-Length.
    const rep = setBody('POST /a HTTP/1.1\nContent-Type: text/plain\nContent-Length: 3\n\nabc', 'a=1&b=2', 'application/x-www-form-urlencoded');
    expect(splitMsg(rep).body).toBe('a=1&b=2');
    expect(getHeader(rep, 'Content-Length')).toBe('7');
    expect(getHeader(rep, 'Content-Type')).toBe('application/x-www-form-urlencoded');
  });

  it('adds a body with a matching Content-Length', () => {
    expect(addBody(REQ, 'hello')).toBe('GET /a HTTP/1.1\nHost: x\nContent-Length: 5\n\nhello');
  });

  it('fixes Content-Length (replace or add), UTF-8 aware', () => {
    expect(fixContentLength('POST / HTTP/1.1\nContent-Length: 99\n\nabcd')).toContain('Content-Length: 4');
    expect(fixContentLength('POST / HTTP/1.1\nHost: x\n\nhi')).toBe('POST / HTTP/1.1\nHost: x\nContent-Length: 2\n\nhi');
    expect(byteLength('✓')).toBe(3);
    expect(fixContentLength('POST / HTTP/1.1\n\n✓')).toContain('Content-Length: 3');
  });
});
