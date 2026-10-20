import { describe, expect, it } from 'vitest';
import { buildSnapshot, drawSnapshot, drawShapes, bodyType, tokenizeLine, wordAt } from './capture.js';

// A recording 2D-context stub so we can run the real draw path (jsdom has no canvas).
function stubCtx() {
  const calls = { fillText: [] };
  const grad = { addColorStop() {} };
  return new Proxy({}, {
    get(_, p) {
      if (p === 'measureText') return (s) => ({ width: String(s).length * 7 });
      if (p === 'createLinearGradient') return () => grad;
      if (p === 'fillText') return (s) => calls.fillText.push(String(s));
      if (p === '__calls') return calls;
      if (typeof p === 'string' && /^(set|get)$/.test(p)) return undefined;
      return () => {}; // every other method is a no-op
    },
    set() { return true; }, // font/fillStyle/etc assignments
  });
}

const T = { text: 'TEXT', dim: 'DIM', blue: 'BLUE', green: 'GREEN', amber: 'AMBER', pink: 'PINK', cyan: 'CYAN', accent: 'ACCENT' };
const joined = (segs) => segs.map((s) => s.text).join('');
// jsdom has no 2D context; buildSnapshot only needs getContext to exist (falls back).
const fakeCanvas = () => ({ width: 0, height: 0, getContext: () => null });

describe('buildSnapshot', () => {
  const source = {
    method: 'POST', url: 'https://x/api', status: 200, durationMs: 42,
    request: 'POST /api HTTP/1.1\nHost: x\nCookie: session=abcdef1234567890xyz\n\n{"password":"Winter2020!"}',
    response: 'HTTP/1.1 200 OK\nContent-Type: application/json\n\n{"id":1337}',
  };

  it('lays out a sized card with request + response columns', () => {
    const snap = buildSnapshot(source, { theme: 'dark', layout: 'side' }, fakeCanvas());
    expect(snap.width).toBeGreaterThan(400);
    expect(snap.height).toBeGreaterThan(80);
    expect(snap.columns.map((c) => c.key)).toEqual(['req', 'res']);
  });

  it('stacked layout is narrower (one column wide) than side-by-side', () => {
    const side = buildSnapshot(source, { layout: 'side' }, fakeCanvas());
    const stacked = buildSnapshot(source, { layout: 'stacked' }, fakeCanvas());
    expect(stacked.width).toBeLessThan(side.width);
  });

  it('auto-redacts a cookie value but keeps the last 4 chars', () => {
    const snap = buildSnapshot(source, { autoRedact: true }, fakeCanvas());
    const cookieLine = snap.columns[0].lines.find((l) => l.cells.map((c) => c.ch).join('').includes('Cookie:'));
    expect(cookieLine).toBeTruthy();
    const redacted = cookieLine.rd.filter(Boolean).length;
    expect(redacted).toBeGreaterThan(4); // the value is hidden
    // the last 4 characters of the value are NOT redacted (kept visible)
    const chars = cookieLine.cells.map((c) => c.ch).join('');
    const lastRedacted = cookieLine.rd.lastIndexOf(true);
    expect(chars.length - 1 - lastRedacted).toBeGreaterThanOrEqual(4);
  });

  it('auto-redacts a sensitive body value (password)', () => {
    const snap = buildSnapshot(source, { autoRedact: true }, fakeCanvas());
    const pwLine = snap.columns[0].lines.find((l) => l.cells.map((c) => c.ch).join('').includes('password'));
    expect(pwLine.rd.some(Boolean)).toBe(true);
  });

  it('leaves everything visible when autoRedact is off', () => {
    const snap = buildSnapshot(source, { autoRedact: false }, fakeCanvas());
    for (const col of snap.columns) for (const l of col.lines) expect(l.rd.some(Boolean)).toBe(false);
  });

  const colText = (snap, i = 0) => snap.columns[i].lines.map((l) => l.cells.map((c) => c.ch).join('')).join('\n');

  it('auto-trims a very long body, keeping head, tail and a marker', () => {
    const body = Array.from({ length: 120 }, (_, i) => `line ${i}`).join('\n');
    const src = { request: `GET / HTTP/1.1\nHost: x\n\n${body}`, response: '' };
    const trimmed = buildSnapshot(src, { autoTrim: true }, fakeCanvas());
    const full = buildSnapshot(src, { autoTrim: false }, fakeCanvas());
    expect(trimmed.columns[0].lines.length).toBeLessThan(full.columns[0].lines.length);
    const txt = colText(trimmed);
    expect(txt).toContain('trimmed');
    expect(txt).toContain('line 0'); // head kept
    expect(txt).toContain('line 119'); // tail kept
    expect(txt).not.toContain('line 60'); // middle dropped
  });

  it('auto-trim caps a single very long body line', () => {
    const src = { request: `GET / HTTP/1.1\nHost: x\n\n${'A'.repeat(1000)}`, response: '' };
    const txt = colText(buildSnapshot(src, { autoTrim: true }, fakeCanvas()), 0);
    expect(txt).toContain('chars]'); // the "[+N chars]" marker
    expect(txt).not.toContain('A'.repeat(500)); // the bulk is cut
  });

  it('keeps the whole long body when autoTrim is off (up to the safety cap)', () => {
    const body = Array.from({ length: 120 }, (_, i) => `line ${i}`).join('\n');
    const src = { request: `GET / HTTP/1.1\nHost: x\n\n${body}`, response: '' };
    expect(colText(buildSnapshot(src, { autoTrim: false }, fakeCanvas()))).toContain('line 60');
  });

  it('crops to a visible line range (follow page scroll) with context markers', () => {
    // logical lines: 0 "GET.." 1 "Host:" 2 "" 3 "b0" ... 22 "b19"  (23 total)
    const body = Array.from({ length: 20 }, (_, i) => `b${i}`).join('\n');
    const src = { request: `GET / HTTP/1.1\nHost: x\n\n${body}`, response: '' };
    const txt = colText(buildSnapshot(src, { reqRange: { start: 5, end: 10 }, autoTrim: false }, fakeCanvas()));
    expect(txt).toContain('above'); // "N lines above" marker
    expect(txt).toContain('below'); // "N more lines below" marker
    expect(txt).toContain('b2'); // logical index 5 is "b2" -> in range
    expect(txt).not.toContain('b0'); // index 3 -> above the range
    expect(txt).not.toContain('b15'); // below the range
  });
});

describe('drawSnapshot (runs the full draw path)', () => {
  const source = {
    method: 'POST', url: 'https://app.example.com/api/v2/users/1337', status: 200, durationMs: 142,
    title: 'IDOR: role escalated', host: 'app.example.com',
    request: 'POST /api HTTP/1.1\nHost: app.example.com\nAuthorization: Bearer eyJabc.defXYZ9999\n\n{"role":"admin","password":"Winter2020!"}',
    response: 'HTTP/1.1 200 OK\nContent-Type: application/json\n\n{"email":"victim@example.com","apiToken":"sk_live_abcd2vQ7"}',
  };
  const run = (opts, state) => {
    const snap = buildSnapshot(source, opts, fakeCanvas());
    const ctx = stubCtx();
    const res = drawSnapshot(ctx, snap, state, 2);
    return { snap, ctx, res };
  };

  it('draws side-by-side with callout notes + manual redaction without throwing', () => {
    const { ctx, res } = run(
      { theme: 'dark', layout: 'side', title: 'IDOR: role escalated', watermark: 'ACME · {host} · {date}', redactStyle: 'blur' },
      { marks: [{ col: 'req', line: 4, color: '#f00', n: 1, note: 'client-controlled', dx: 0, dy: 0 }], redacts: [{ col: 'res', line: 4, c1: 2, c2: 8 }] },
    );
    expect(res.noteRects.length).toBe(1);
    const printed = ctx.__calls.fillText.join(' ');
    expect(printed).toContain('POST'); // method pill
    expect(printed).toContain('142 ms'); // response time
    expect(printed).toContain('IDOR'); // title heading
    expect(printed).toContain('client-controlled'); // callout note
  });

  it('draws stacked + legend + each redaction style without throwing', () => {
    for (const redactStyle of ['blur', 'solid', 'dots']) {
      const { res } = run(
        { theme: 'light', layout: 'stacked', notesMode: 'legend', redactStyle, legendLines: 1 },
        { marks: [{ col: 'req', line: 4, color: '#00f', n: 1, note: 'see line', dx: 0, dy: 0 }], redacts: [] },
      );
      expect(res).toBeTruthy();
    }
  });

  it('hides the response time when showTime is off', () => {
    const { ctx } = run({ showTime: false }, { marks: [], redacts: [] });
    expect(ctx.__calls.fillText.join(' ')).not.toContain('142 ms');
  });

  it('draws the Ngintip wordmark in the header', () => {
    const { ctx } = run({ title: 'x' }, { marks: [], redacts: [] });
    expect(ctx.__calls.fillText.join(' ')).toContain('Ngintip');
  });

  it('draws every freehand shape type without throwing', () => {
    const ctx = stubCtx();
    const shapes = [
      { type: 'rect', color: '#f00', width: 4, x: 1, y: 1, x2: 20, y2: 10 },
      { type: 'ellipse', color: '#0f0', width: 4, x: 1, y: 1, x2: 20, y2: 10 },
      { type: 'arrow', color: '#00f', width: 4, x: 1, y: 1, x2: 20, y2: 10 },
      { type: 'line', color: '#ff0', width: 2, x: 1, y: 1, x2: 20, y2: 10 },
      { type: 'pen', color: '#0ff', width: 3, points: [{ x: 1, y: 1 }, { x: 5, y: 8 }] },
      { type: 'highlight', color: '#ff0', width: 5, points: [{ x: 1, y: 1 }, { x: 9, y: 2 }] },
      { type: 'text', color: '#fff', width: 4, x: 3, y: 3, text: 'note' },
    ];
    expect(() => drawShapes(ctx, shapes)).not.toThrow();
    expect(ctx.__calls.fillText.join(' ')).toContain('note');
  });
});

describe('wordAt', () => {
  it('selects the value token under a char, stripping quotes', () => {
    const snap = buildSnapshot({ request: 'GET / HTTP/1.1\n\n{"role":"admin"}', response: '' }, {}, fakeCanvas());
    const line = snap.columns[0].lines.findIndex((l) => l.cells.map((c) => c.ch).join('').includes('admin'));
    const text = snap.columns[0].lines[line].cells.map((c) => c.ch).join('');
    const w = wordAt(snap, 'req', line, text.indexOf('admin') + 1);
    expect(text.slice(w.c1, w.c2)).toBe('admin');
  });
});

describe('bodyType', () => {
  it('detects JSON, markup and plain text', () => {
    expect(bodyType('{"a":1}')).toBe('json');
    expect(bodyType('  [1,2]')).toBe('json');
    expect(bodyType('<html>')).toBe('markup');
    expect(bodyType('hello')).toBe('');
    expect(bodyType(undefined)).toBe('');
  });
});

describe('tokenizeLine', () => {
  it('colours JSON keys, strings, numbers and literals', () => {
    const segs = tokenizeLine('{"k": "v", "n": 12, "b": true}', 'json', T);
    expect(joined(segs)).toBe('{"k": "v", "n": 12, "b": true}');
    const color = (txt) => segs.find((s) => s.text === txt)?.color;
    expect(color('"k"')).toBe(T.blue);
    expect(color('"v"')).toBe(T.green);
    expect(color('12')).toBe(T.amber);
    expect(color('true')).toBe(T.pink);
  });
  it('colours markup tags and attributes', () => {
    const segs = tokenizeLine('<a href="/x">hi</a>', 'markup', T);
    expect(joined(segs)).toBe('<a href="/x">hi</a>');
    const roles = segs.filter((s) => s.text.trim()).map((s) => [s.text, s.color]);
    expect(roles).toContainEqual(['a', T.pink]);
    expect(roles).toContainEqual(['href', T.cyan]);
    expect(roles).toContainEqual(['"/x"', T.green]);
  });
  it('preserves exact characters with no matches', () => {
    expect(joined(tokenizeLine('   ', 'json', T))).toBe('   ');
  });
});
