import { describe, expect, it } from 'vitest';
import { isLoopback } from './net.js';
import { fmtSize, statusClass } from './format.js';

describe('isLoopback', () => {
  it.each(['127.0.0.1', '127.1.2.3', 'localhost', '::1', '[::1]', ' LOCALHOST '])('%s is local', (h) => {
    expect(isLoopback(h)).toBe(true);
  });
  it.each(['0.0.0.0', '192.168.1.10', '::', 'example.com', ''])('%s is exposed', (h) => {
    expect(isLoopback(h)).toBe(false);
  });
});

describe('format', () => {
  it('sizes and status classes', () => {
    expect([fmtSize(0), fmtSize(512), fmtSize(2048), fmtSize(5 * 1024 * 1024)]).toEqual(['0 B', '512 B', '2.0 KB', '5.0 MB']);
    expect([statusClass(0), statusClass(204), statusClass(302), statusClass(404), statusClass(500)]).toEqual(['s-err', 's-2xx', 's-3xx', 's-4xx', 's-5xx']);
  });
});
