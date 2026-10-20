import { describe, expect, it } from 'vitest';
import { hostMatches, inScope } from './scope.js';

// Mirrors the Rust tests in src-tauri/src/scope.rs: both sides must agree.
describe('scope', () => {
  it('matches hosts like the backend', () => {
    expect(hostMatches('example.com', 'example.com')).toBe(true);
    expect(hostMatches('example.com', 'example.com:8443')).toBe(true);
    expect(hostMatches('example.com', 'api.example.com')).toBe(false);
    expect(hostMatches('*.example.com', 'api.example.com')).toBe(true);
    expect(hostMatches('*.example.com', 'example.com')).toBe(true);
    expect(hostMatches('*.example.com', 'badexample.com')).toBe(false);
    expect(hostMatches('example.com', 'EXAMPLE.com')).toBe(true);
  });

  it('treats an empty scope as everything', () => {
    expect(inScope([], 'anything.test')).toBe(true);
    expect(inScope(['*.example.com'], 'a.example.com')).toBe(true);
    expect(inScope(['*.example.com'], 'other.test')).toBe(false);
  });
});
