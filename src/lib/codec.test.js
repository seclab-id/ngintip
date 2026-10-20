import { describe, expect, it } from 'vitest';
import * as c from './codec.js';

describe('codec', () => {
  it('base64 round-trips UTF-8 and accepts URL-safe input', () => {
    expect(c.base64Encode('héllo ✓')).toBe('aMOpbGxvIOKckw==');
    expect(c.base64Decode('aMOpbGxvIOKckw==')).toBe('héllo ✓');
    expect(c.base64Encode('ö?>', { url: true })).toBe('w7Y_Pg');
    expect(c.base64Decode('w7Y_Pg')).toBe('ö?>');
    expect(c.base64Decode('/w==')).toBe('ff'); // not UTF-8: shown as hex
    expect(() => c.base64Decode('***')).toThrow();
  });

  it('url encodes and decodes leniently', () => {
    expect(c.urlEncode('a b&c=✓')).toBe('a%20b%26c%3D%E2%9C%93');
    expect(c.urlEncode('ab', { all: true })).toBe('%61%62');
    expect(c.urlDecode('a%20b%zz%E2%9C%93')).toBe('a b%zz✓');
  });

  it('html entities', () => {
    expect(c.htmlEncode(`<a href="x">'&'</a>`)).toBe('&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;');
    expect(c.htmlDecode('&lt;&#x41;&#66;&amp;&bogus;&nbsp;')).toBe('<AB&&bogus; ');
  });

  it('hex', () => {
    expect(c.hexEncode('hi ✓')).toBe('686920e29c93');
    expect(c.hexDecode('0x68 69:21')).toBe('hi!');
    expect(() => c.hexDecode('abc')).toThrow();
  });

  it('jwt', () => {
    const out = c.jwtDecode('eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMiLCJleHAiOjE3MDAwMDAwMDB9.sig');
    expect(out).toContain('"alg": "HS256"');
    expect(out).toContain('"sub": "123"');
    expect(out).toContain('exp: 2023-11-14T22:13:20.000Z (expired)');
    expect(() => c.jwtDecode('nope')).toThrow();
  });

  it('hashes match known vectors', async () => {
    expect(c.md5('')).toBe('d41d8cd98f00b204e9800998ecf8427e');
    expect(c.md5('The quick brown fox jumps over the lazy dog')).toBe('9e107d9d372bb6826bd81d3542a419d6');
    expect(c.md5('a'.repeat(200))).toBe('887f30b43b2867f4a9accceee7d16e6c');
    expect(await c.sha('SHA-256', 'abc')).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    expect(await c.sha('SHA-1', 'abc')).toBe('a9993e364706816aba3e25717850c26c9cd0d89d');
  });

  it('every operation is wired', async () => {
    const sample = { jwt: 'e30.e30.', hexd: '6869' };
    for (const op of c.OPERATIONS) {
      expect(await op.run(sample[op.id] ?? 'aGk=')).toBeTypeOf('string');
    }
  });

  it('runRecipe chains operations, feeding each into the next', async () => {
    // Empty recipe passes the input through unchanged.
    expect(await c.runRecipe('hi', [])).toMatchObject({ output: 'hi', errorStep: -1 });
    // Double URL-decode: "%2541" -> "%41" -> "A".
    expect(await c.runRecipe('%2541', ['urld', 'urld'])).toMatchObject({ output: 'A', error: '' });
    // Mixed encode chain: text -> Base64 -> Hex.
    const enc = await c.runRecipe('hi', ['b64e', 'hexe']);
    expect(enc.output).toBe(c.hexEncode(c.base64Encode('hi')));
    // Round-trip: URL-encode then Base64-encode, then decode back.
    const round = await c.runRecipe('a b&c', ['urle', 'b64e', 'b64d', 'urld']);
    expect(round.output).toBe('a b&c');
    // A failing step reports its index and stops.
    const bad = await c.runRecipe('!!!not-hex', ['hexd']);
    expect(bad.errorStep).toBe(0);
    expect(bad.error).toBeTruthy();
    // opById resolves known ids and ignores unknown.
    expect(c.opById('b64d').label).toBe('Base64');
    expect(c.opById('nope')).toBeUndefined();
  });

  it('autoDecode detects the likely encoding of a highlighted snippet', () => {
    // A JWT wins over plain Base64 (checked first, needs three parts).
    const jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.sig';
    expect(c.autoDecode(jwt)[0].id).toBe('jwt');

    // URL-encoded text is detected via the %XX escapes.
    expect(c.autoDecode('a%20b%26c')[0]).toMatchObject({ id: 'urld', output: 'a b&c' });

    // Base64 needs enough length and a valid alphabet.
    expect(c.autoDecode('cm9sZT1hZG1pbg==')[0]).toMatchObject({ id: 'b64d', output: 'role=admin' });

    // Plain text matches nothing.
    expect(c.autoDecode('just some words')).toEqual([]);
    expect(c.autoDecode('   ')).toEqual([]);
  });
});
