import { describe, expect, it } from 'vitest';
import { followRequest, redirectInfo } from './redirect.js';

const res = (status, loc) => `HTTP/1.1 ${status} Redirect\nLocation: ${loc}\nContent-Type: text/html\n\n`;

describe('redirect', () => {
  it('detects a redirect + Location, ignores non-redirects', () => {
    expect(redirectInfo(res(302, '/next'))).toEqual({ status: 302, location: '/next' });
    expect(redirectInfo('HTTP/1.1 200 OK\nLocation: /x\n\n')).toBeNull();
    expect(redirectInfo('HTTP/1.1 302 Found\nContent-Type: text/html\n\n')).toBeNull();
  });

  it('follows a relative Location as GET (301/302/303), dropping the body', () => {
    const req = 'POST /login HTTP/1.1\nHost: example.com\nContent-Type: application/json\nContent-Length: 7\n\n{"a":1}';
    const next = followRequest(req, res(303, '/dashboard'), 'https://example.com');
    expect(next.target).toBe('https://example.com');
    expect(next.raw.split('\n')[0]).toBe('GET /dashboard HTTP/1.1');
    expect(next.raw).toContain('Host: example.com');
    expect(next.raw).not.toContain('{"a":1}'); // body dropped
    expect(next.raw).not.toMatch(/content-type/i);
  });

  it('keeps method and body on 307/308', () => {
    const req = 'POST /a HTTP/1.1\nHost: example.com\n\nkeepme';
    const next = followRequest(req, res(307, '/b'), 'https://example.com');
    expect(next.raw.split('\n')[0]).toBe('POST /b HTTP/1.1');
    expect(next.raw).toContain('keepme');
  });

  it('follows an absolute Location to another host', () => {
    const req = 'GET / HTTP/1.1\nHost: example.com\n\n';
    const next = followRequest(req, res(302, 'https://cdn.other.com/asset?v=2'), 'https://example.com');
    expect(next.target).toBe('https://cdn.other.com');
    expect(next.raw).toContain('GET /asset?v=2 HTTP/1.1');
    expect(next.raw).toContain('Host: cdn.other.com');
  });

  it('returns null when there is nothing to follow', () => {
    expect(followRequest('GET / HTTP/1.1\nHost: x\n\n', 'HTTP/1.1 200 OK\n\n', 'https://x')).toBeNull();
  });
});
