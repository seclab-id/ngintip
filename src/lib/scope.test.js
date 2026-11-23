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

  // The HOST is normalized once per call now, not once per pattern, so every pattern
  // after the first sees the already-stripped, already-lowercased host. Port and case on
  // the host must still be ignored no matter WHICH pattern in the list matches it.
  //
  // The patterns themselves are lowercased when the scope is saved (`normalize` in
  // src-tauri/src/scope.rs), so an upper-case pattern cannot reach here and is NOT
  // handled: that was true before this change too.
  it('ignores port and case against any pattern in the list, not just the first', () => {
    const scope = ['first.test', '*.second.test', 'third.test'];
    expect(inScope(scope, 'FIRST.test:8443')).toBe(true);
    expect(inScope(scope, 'API.Second.test:443')).toBe(true);
    expect(inScope(scope, 'second.test:9000')).toBe(true);
    expect(inScope(scope, 'third.TEST:1234')).toBe(true);
    expect(inScope(scope, 'notsecond.test:443')).toBe(false);
    expect(inScope(scope, 'fourth.test:443')).toBe(false);
  });
});
