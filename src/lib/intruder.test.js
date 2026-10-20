import { describe, expect, it } from 'vitest';
import {
  MARK,
  parseTemplate,
  stripMarkers,
  generate,
  wrapPosition,
  clearPositions,
  resolvePayloads,
  countPayloads,
  processPayload,
  defaultPayloadConfig,
} from './intruder.js';

const tpl = `GET /u?id=${MARK}1${MARK}&t=${MARK}x${MARK} HTTP/1.1\n\n`;

describe('intruder templates', () => {
  it('parses positions and strips markers', () => {
    const { positions } = parseTemplate(tpl);
    expect(positions).toEqual(['1', 'x']);
    expect(stripMarkers(tpl)).toBe('GET /u?id=1&t=x HTTP/1.1\n\n');
    expect(parseTemplate('no marks').positions).toEqual([]);
  });

  it('leaves an unmatched marker literal', () => {
    expect(parseTemplate(`a${MARK}b`).positions).toEqual([]);
    expect(stripMarkers(`a${MARK}b`)).toBe(`a${MARK}b`);
  });
});

describe('generate', () => {
  it('sniper: one position at a time, others default', () => {
    const g = generate(tpl, 'sniper', ['A\nB']);
    expect(g.count).toBe(4);
    expect(g.requests[0].request).toContain('id=A&t=x');
    expect(g.requests[1].request).toContain('id=B&t=x');
    expect(g.requests[2].request).toContain('id=1&t=A');
    expect(g.requests[3].request).toContain('id=1&t=B');
  });

  it('battering ram: same payload everywhere', () => {
    const g = generate(tpl, 'ram', ['A\nB']);
    expect(g.count).toBe(2);
    expect(g.requests[0].request).toContain('id=A&t=A');
    expect(g.requests[1].request).toContain('id=B&t=B');
  });

  it('pitchfork: parallel, shortest set wins', () => {
    const g = generate(tpl, 'pitchfork', ['a1\na2\na3', 'b1\nb2']);
    expect(g.count).toBe(2);
    expect(g.requests[0].request).toContain('id=a1&t=b1');
    expect(g.requests[1].request).toContain('id=a2&t=b2');
  });

  it('cluster bomb: every combination', () => {
    const g = generate(tpl, 'cluster', ['a1\na2', 'b1\nb2\nb3']);
    expect(g.total).toBe(6);
    expect(g.count).toBe(6);
    expect(g.requests[0].request).toContain('id=a1&t=b1');
    expect(g.requests[5].request).toContain('id=a2&t=b3');
    expect(g.requests.map((r) => r.payloads.join(','))).toContain('a2,b1');
  });

  it('reports when there are no positions', () => {
    expect(generate('GET / HTTP/1.1\n\n', 'sniper', ['x']).error).toMatch(/No payload positions/);
  });

  it('accepts payload-type configs, not just wordlist strings', () => {
    // Numbers generator into a battering ram (same payload in both positions).
    const g = generate(tpl, 'ram', [{ type: 'numbers', numbers: { from: 1, to: 3, step: 1 } }]);
    expect(g.count).toBe(3);
    expect(g.requests[0].request).toContain('id=1&t=1');
    expect(g.requests[2].request).toContain('id=3&t=3');
  });

  it('reports a payload generator error (brute forcer with no characters)', () => {
    const g = generate(tpl, 'sniper', [{ type: 'brute', brute: { chars: '', min: 1, max: 2 } }]);
    expect(g.error).toMatch(/character/i);
  });
});

describe('payload generators', () => {
  it('numbers: range, step, zero-pad and hex', () => {
    expect(resolvePayloads({ type: 'numbers', numbers: { from: 1, to: 3, step: 1 } }).list).toEqual(['1', '2', '3']);
    expect(resolvePayloads({ type: 'numbers', numbers: { from: 0, to: 10, step: 5 } }).list).toEqual(['0', '5', '10']);
    expect(resolvePayloads({ type: 'numbers', numbers: { from: 3, to: 1, step: 1 } }).list).toEqual(['3', '2', '1']);
    expect(resolvePayloads({ type: 'numbers', numbers: { from: 1, to: 3, step: 1, pad: 3 } }).list).toEqual(['001', '002', '003']);
    expect(resolvePayloads({ type: 'numbers', numbers: { from: 10, to: 12, step: 1, hex: true } }).list).toEqual(['a', 'b', 'c']);
  });

  it('brute forcer: every combination across the length range', () => {
    const r = resolvePayloads({ type: 'brute', brute: { chars: 'ab', min: 1, max: 2 } });
    expect(r.list).toEqual(['a', 'b', 'aa', 'ab', 'ba', 'bb']);
    expect(countPayloads({ type: 'brute', brute: { chars: 'abc', min: 1, max: 2 } })).toBe(3 + 9);
  });

  it('null payloads: N empty strings', () => {
    expect(resolvePayloads({ type: 'null', nul: { count: 3 } }).list).toEqual(['', '', '']);
    expect(countPayloads({ type: 'null', nul: { count: 4 } })).toBe(4);
  });

  it('a plain string is treated as a simple list (back-compat)', () => {
    expect(resolvePayloads('a\n\nb\nc').list).toEqual(['a', 'b', 'c']);
    expect(countPayloads('a\nb')).toBe(2);
  });

  it('processing: case, prefix/suffix, encodings and null byte', () => {
    expect(processPayload('Ab', { case: 'lower' })).toBe('ab');
    expect(processPayload('Ab', { case: 'upper' })).toBe('AB');
    expect(processPayload('x', { prefix: '<', suffix: '>' })).toBe('<x>');
    expect(processPayload('a b&c', { encode: 'url' })).toBe('a%20b%26c');
    expect(processPayload('a ', { encode: 'urlall' })).toBe('%61%20');
    expect(processPayload('AB', { encode: 'base64' })).toBe('QUI=');
    expect(processPayload('x', { nullByte: true })).toBe('x\u0000');
    // Applied across a generated list.
    const r = resolvePayloads({ type: 'numbers', numbers: { from: 1, to: 2, step: 1 }, process: { prefix: 'id-' } });
    expect(r.list).toEqual(['id-1', 'id-2']);
  });

  it('defaultPayloadConfig is a fresh simple-list config each time', () => {
    const a = defaultPayloadConfig();
    const b = defaultPayloadConfig();
    expect(a.type).toBe('simple');
    expect(a).not.toBe(b);
    expect(a.numbers).not.toBe(b.numbers);
  });

  it('wrapPosition marks a selection and clearPositions removes markers', () => {
    const raw = 'GET /u?id=1 HTTP/1.1\n\n';
    // Wrap the "1" (index 10..11).
    const w = wrapPosition(raw, 10, 11);
    expect(w.raw).toBe(`GET /u?id=${MARK}1${MARK} HTTP/1.1\n\n`);
    expect(w.caret).toBe(13); // just after the closing marker
    expect(parseTemplate(w.raw).positions).toEqual(['1']);
    // No selection -> a §payload§ placeholder at the caret.
    const p = wrapPosition('GET / HTTP/1.1\n\n', 4, 4);
    expect(p.raw).toContain(`${MARK}payload${MARK}`);
    // Clear removes every marker.
    expect(clearPositions(w.raw)).toBe(raw);
  });
});
