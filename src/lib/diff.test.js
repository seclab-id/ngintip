import { describe, expect, it } from 'vitest';
import { lineDiff, diffStats } from './diff.js';

describe('lineDiff', () => {
  it('marks added, removed and unchanged lines with per-side line numbers', () => {
    const ops = lineDiff('l1\nl2\nl3', 'l1\nX2\nl3');
    expect(ops.map((o) => o.type)).toEqual(['same', 'del', 'add', 'same']);
    expect(ops[0]).toMatchObject({ type: 'same', text: 'l1', a: 1, b: 1 });
    expect(ops[1]).toMatchObject({ type: 'del', text: 'l2', a: 2, b: null });
    expect(ops[2]).toMatchObject({ type: 'add', text: 'X2', a: null, b: 2 });
    expect(ops[3]).toMatchObject({ type: 'same', text: 'l3', a: 3, b: 3 });
    expect(diffStats(ops)).toMatchObject({ added: 1, removed: 1, same: 2, changed: 2 });
  });

  it('reports zero changes for identical input', () => {
    const ops = lineDiff('a\nb\nc', 'a\nb\nc');
    expect(ops.every((o) => o.type === 'same')).toBe(true);
    expect(diffStats(ops).changed).toBe(0);
  });

  it('handles pure insertion and deletion', () => {
    expect(lineDiff('a\nb', 'a\nb\nc').map((o) => o.type)).toEqual(['same', 'same', 'add']);
    expect(lineDiff('a\nb\nc', 'a\nb').map((o) => o.type)).toEqual(['same', 'same', 'del']);
  });

  it('normalizes CRLF so line endings alone are not a difference', () => {
    expect(diffStats(lineDiff('a\r\nb', 'a\nb')).changed).toBe(0);
  });

  it('keeps trailing lines aligned after an inserted block', () => {
    const ops = lineDiff('h1\nbody', 'h1\nx\ny\nbody');
    expect(ops.map((o) => o.type)).toEqual(['same', 'add', 'add', 'same']);
    // The shared trailing line keeps correct numbers on each side.
    const last = ops[ops.length - 1];
    expect(last).toMatchObject({ type: 'same', text: 'body', a: 2, b: 4 });
  });

  it('respects case sensitivity (default on, ignoreCase off)', () => {
    expect(diffStats(lineDiff('Hello', 'hello')).changed).toBe(2); // del + add
    const ops = lineDiff('Hello', 'hello', { ignoreCase: true });
    expect(diffStats(ops).changed).toBe(0);
    // Display keeps the original (A side) text for a matched line.
    expect(ops[0]).toMatchObject({ type: 'same', text: 'Hello' });
  });

  it('can ignore whitespace differences', () => {
    expect(diffStats(lineDiff('a  b', 'a b')).changed).toBe(2);
    expect(diffStats(lineDiff('a  b', 'a b', { ignoreWhitespace: true })).changed).toBe(0);
    expect(diffStats(lineDiff('  x  ', 'x', { ignoreWhitespace: true })).changed).toBe(0);
  });
});
