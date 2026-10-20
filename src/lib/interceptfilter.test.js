import { describe, expect, it } from 'vitest';
import { interceptAllows, newInterceptRule, normalizeInterceptRule } from './interceptfilter.js';

const msg = (o) => ({ method: 'GET', host: 'a.com', path: '/x', url: 'http://a.com/x', ...o });

describe('interceptAllows (mirrors the Rust filter)', () => {
  it('holds everything with no rules', () => {
    expect(interceptAllows([], msg())).toBe(true);
    expect(interceptAllows([newInterceptRule({ value: '' })], msg())).toBe(true); // empty value ignored
  });

  it('include: only matching messages are held', () => {
    const rules = [newInterceptRule({ field: 'host', mode: 'include', value: 'api.' })];
    expect(interceptAllows(rules, msg({ host: 'api.a.com', url: 'http://api.a.com/x' }))).toBe(true);
    expect(interceptAllows(rules, msg({ host: 'cdn.a.com', url: 'http://cdn.a.com/x' }))).toBe(false);
  });

  it('exclude wins over include', () => {
    const rules = [
      newInterceptRule({ field: 'host', mode: 'include', value: 'a.com' }),
      newInterceptRule({ field: 'path', mode: 'exclude', value: '/static/' }),
    ];
    expect(interceptAllows(rules, msg({ path: '/api/v1', url: 'http://a.com/api/v1' }))).toBe(true);
    expect(interceptAllows(rules, msg({ path: '/static/app.js', url: 'http://a.com/static/app.js' }))).toBe(false);
  });

  it('honours case sensitivity and the method field', () => {
    expect(interceptAllows([newInterceptRule({ field: 'path', mode: 'include', value: '/API', sensitive: true })], msg({ path: '/api' }))).toBe(false);
    expect(interceptAllows([newInterceptRule({ field: 'path', mode: 'include', value: '/API', sensitive: true })], msg({ path: '/API' }))).toBe(true);
    expect(interceptAllows([newInterceptRule({ field: 'method', mode: 'include', value: 'POST' })], msg({ method: 'POST' }))).toBe(true);
    expect(interceptAllows([newInterceptRule({ field: 'method', mode: 'include', value: 'POST' })], msg({ method: 'GET' }))).toBe(false);
  });

  it('normalizes unknown fields/modes', () => {
    const r = normalizeInterceptRule({ field: 'bogus', mode: 'weird', value: 5, sensitive: 1 });
    expect(r).toEqual({ field: 'host', mode: 'include', value: '5', sensitive: true });
  });
});
