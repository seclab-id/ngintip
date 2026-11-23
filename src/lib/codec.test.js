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

  it('jwt: shows the token as it was signed, not a re-serialized copy', () => {
    // The reason anyone opens a token in a proxy: two `alg` keys to split two libraries
    // apart, a claim spelled with an escape so a filter reads one thing and the backend
    // another, an id past 2^53. Re-serializing the parsed value drops the first key,
    // decodes the escape and rounds the id, so the view would show a token that was
    // never issued. Proven against the old implementation, which printed
    // `"alg": "HS256"` alone, `"adcmin"` and `12345678901234567000`.
    const BS = '\\';
    const b64u = (o) => btoa(o).replace(/[+]/g, '-').replace(/[/]/g, '_').replace(/=+$/, '');
    const header = `{"alg":"none","alg":"HS256","typ":"J${BS}u0057T"}`;
    const payload = `{"user":"ad${BS}u0063min","id":12345678901234567890,"role":"user","role":"admin"}`;
    const out = c.jwtDecode(`${b64u(header)}.${b64u(payload)}.sig`);
    expect(out).toContain('"alg": "none"');
    expect(out).toContain('"alg": "HS256"');
    expect(out).toContain(`"typ": "J${BS}u0057T"`);
    expect(out).toContain(`"user": "ad${BS}u0063min"`);
    expect(out).toContain('"id": 12345678901234567890');
    expect(out).toContain('"role": "user"');
    expect(out).toContain('"role": "admin"');
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

  // Highlighting text in a message opens the mini decoder only when autoDecode claims a
  // match, so a false match is not cosmetic: the panel appears over the request you were
  // reading, showing nonsense. Reported from a real engagement, selecting `productType`
  // in a Repeater request popped the decoder open on `a6 ba 1d b9 cb 53 ca 97`.
  it('does not claim a match when the decoded BYTES are not text', () => {
    // Shaped like Base64 (length >= 8, only Base64 characters) but the bytes are binary.
    // The old guard was defeated by its own rendering: non-UTF-8 bytes are displayed as
    // a hex dump, and a hex dump is always "printable".
    expect(c.autoDecode('productType')).toEqual([]);
    expect(c.autoDecode('Content-Length')).toEqual([]);
    expect(c.autoDecode('gamesmax-booster')).toEqual([]);
    expect(c.autoDecode('application/json')).toEqual([]);
    expect(c.autoDecode('081235564795')).toEqual([]);
    // Valid hex, but of bytes that are not text.
    expect(c.autoDecode('a6ba1db9cb53ca97')).toEqual([]);

    // The real ones still match, which is the whole point of the feature.
    expect(c.autoDecode('SGVsbG8gZnJvbSBOZ2ludGlw')[0]).toMatchObject({ id: 'b64d', output: 'Hello from Ngintip' });
    expect(c.autoDecode('48656c6c6f20776f726c64')[0]).toMatchObject({ id: 'hexd', output: 'Hello world' });
    expect(c.autoDecode('%7B%22a%22%3A1%7D')[0]).toMatchObject({ id: 'urld', output: '{"a":1}' });
    expect(c.autoDecode('&lt;script&gt;')[0]).toMatchObject({ id: 'htmld', output: '<script>' });
  });

  it('autoDecode still claims a blob whose plaintext is binary', () => {
    // The point of the byte check above is that `productType` is not Base64 just because
    // its shape fits. The point of this one is the opposite half: on an encrypted API the
    // hex dump is exactly what the operator highlighted the blob to see, and judging on
    // bytes alone made a 344-character AES body open nothing at all.
    const aes = btoa(String.fromCharCode(...Array.from({ length: 64 }, (_, i) => (i * 37 + 11) % 256)));
    expect(aes.length).toBeGreaterThan(24);
    expect(c.autoDecode(aes)[0].id).toBe('b64d');
    // Padded, and short enough that a plain length rule would have been tempting.
    expect(c.autoDecode('q83vEjRWeJCrze8SNFZ4kA==')[0].id).toBe('b64d');
    // 32+ hex characters of binary: a digest or an IV, not a word. A hex string of this
    // length is valid Base64 by shape too, so both are offered and the test asks for
    // membership rather than for an order the DECODERS list happens to give.
    expect(c.autoDecode('a6ba1db9cb53ca97a6ba1db9cb53ca97').map((x) => x.id)).toContain('hexd');
  });

  it('autoDecode does not mistake a long identifier for a blob', () => {
    // The shape rule has to survive the words an operator actually double-clicks in a
    // JSON body. These are all >= 24 characters and valid Base64 by shape; none carries
    // a digit or padding, so none of them opens a hex dump. Checked against the whole of
    // /usr/share/dict/words as well, which yields no match either.
    expect(c.autoDecode('transactionReferenceNumber')).toEqual([]);
    expect(c.autoDecode('formaldehydesulphoxylate')).toEqual([]);
    expect(c.autoDecode('scientificophilosophical')).toEqual([]);
  });

  it('still SHOWS binary when a decoder is chosen by hand', () => {
    // Rejecting a guess is not the same as refusing to decode: picking Base64 explicitly
    // in the decoder must still render the bytes, as a hex dump.
    expect(c.base64Decode('productType')).toBe('a6 ba 1d b9 cb 53 ca 97');
    expect(c.hexDecode('a6ba1db9cb53ca97')).toBe('a6 ba 1d b9 cb 53 ca 97');
    expect(c.bytesAsText(c.base64DecodeBytes('productType'))).toBeNull();
    expect(c.bytesAsText(c.base64DecodeBytes('SGVsbG8='))).toBe('Hello');
  });
});
