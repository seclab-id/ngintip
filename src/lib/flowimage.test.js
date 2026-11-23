import { describe, it, expect } from 'vitest';
import { sourceFromDetail, optsFromRequest, renderPng } from './flowimage.js';

// A flow as api.getFlow returns it, with an extension version of both halves.
const detail = (over = {}) => ({
  summary: { method: 'POST', scheme: 'https', host: 'api.test', path: '/pay/v3', status: 200, durationMs: 1902, note: 'IDOR candidate' },
  requestRaw: 'POST /pay/v3 HTTP/1.1\nHost: api.test\n\n<encrypted>',
  responseRaw: 'HTTP/1.1 200 OK\n\n<encrypted>',
  variants: { reqExtension: 'POST /pay/v3 HTTP/1.1\nHost: api.test\n\n{"amount":1}', resExtension: 'HTTP/1.1 200 OK\n\n{"ok":true}' },
  ...over,
});

// jsdom has no canvas, so the drawing is exercised against a stand-in that answers every
// call. It proves the wiring (buildSnapshot -> drawSnapshot -> toDataURL) and the clamps,
// not the pixels: the pixels come from the same renderer the window uses every day.
function fakeCanvasFactory() {
  const calls = { draws: 0, sizes: [] };
  const factory = (w, h) => {
    const canvas = { width: w, height: h };
    const ctx2d = new Proxy(
      {},
      {
        get(_t, k) {
          if (k === 'measureText') return (s) => ({ width: String(s).length * 7 });
          if (k === 'canvas') return canvas;
          // Everything else is a drawing call. Returning the same proxy keeps chained
          // calls like createLinearGradient(...).addColorStop(...) working.
          return (...a) => {
            if (k === 'fillText') calls.draws += 1;
            return a.length >= 0 ? ctx2d : ctx2d;
          };
        },
        set: () => true,
      },
    );
    canvas.getContext = () => ctx2d;
    canvas.toDataURL = () => 'data:image/png;base64,UE5HQllURVM=';
    calls.sizes.push([w, h]);
    return canvas;
  };
  return { factory, calls };
}

describe('flowimage: the card MCP asks the window to draw', () => {
  it('draws the wire bytes by default', () => {
    const { view, source } = sourceFromDetail(detail());
    expect(view).toBe('wire');
    expect(source.request).toContain('<encrypted>');
    expect(source.response).toContain('<encrypted>');
    expect(source.url).toBe('https://api.test/pay/v3');
    expect(source.method).toBe('POST');
    expect(source.status).toBe(200);
    expect(source.durationMs).toBe(1902);
  });

  it('draws the readable version when asked for it', () => {
    const { view, source } = sourceFromDetail(detail(), 'extension');
    expect(view).toBe('extension');
    expect(source.request).toContain('{"amount":1}');
    expect(source.response).toContain('{"ok":true}');
  });

  it('falls back to the wire when there is no extension version, and says so', () => {
    // Drawing an empty card would be the wrong answer, and so would a receipt claiming
    // a decoded view that never existed.
    const { view, source } = sourceFromDetail(detail({ variants: {} }), 'extension');
    expect(view).toBe('wire');
    expect(source.request).toContain('<encrypted>');
  });

  it('titles the card with the flow note, unless the caller says otherwise', () => {
    expect(sourceFromDetail(detail()).source.title).toBe('IDOR candidate');
    expect(sourceFromDetail(detail(), 'wire', 'Payment replay').source.title).toBe('Payment replay');
    // An empty title from the caller means "no title", not "fall back to the note".
    expect(sourceFromDetail(detail(), 'wire', '').source.title).toBe('');
  });

  it('survives a flow with no response and no summary fields', () => {
    const { source } = sourceFromDetail({ requestRaw: 'GET / HTTP/1.1\n\n' });
    expect(source.response).toBe('');
    expect(source.url).toBe('http://');
    expect(source.title).toBe('');
  });

  it('keeps the window defaults, which are the operator defaults', () => {
    // Redaction on, trim on: a card asked for over MCP must not expose more than the
    // same card taken by hand in the window.
    expect(optsFromRequest({})).toMatchObject({ theme: 'dark', layout: 'side', autoRedact: true, autoTrim: true });
    expect(optsFromRequest({ theme: 'light', layout: 'stacked' })).toMatchObject({ theme: 'light', layout: 'stacked' });
    expect(optsFromRequest({ redact: false, trim: false })).toMatchObject({ autoRedact: false, autoTrim: false });
    // Only an explicit false turns them off; anything else keeps the safe default.
    expect(optsFromRequest({ redact: 'no', trim: null })).toMatchObject({ autoRedact: true, autoTrim: true });
  });

  it('returns base64 without the data: prefix, and reports what it drew', () => {
    const { factory, calls } = fakeCanvasFactory();
    const out = renderPng(detail(), { view: 'extension' }, factory);
    expect(out.data).toBe('UE5HQllURVM=');
    expect(out.view).toBe('extension');
    expect(out.scale).toBe(1);
    expect(calls.draws).toBeGreaterThan(0);
    expect(out.width).toBeGreaterThan(0);
    expect(out.height).toBeGreaterThan(0);
  });

  it('clamps the scale to what the renderer supports', () => {
    const { factory } = fakeCanvasFactory();
    const at = (scale) => renderPng(detail(), { scale }, factory);
    const one = at(1);
    expect(at(3).width).toBe(one.width * 3);
    // 1 is the floor and 3 the ceiling, and junk means 1 rather than a canvas of NaN
    // pixels, which throws inside the browser rather than here.
    expect(at(9).scale).toBe(3);
    expect(at(0).scale).toBe(1);
    expect(at('x').scale).toBe(1);
  });

  it('says what is missing when the window has no 2D context', () => {
    const noCtx = (w, h) => ({ width: w, height: h, getContext: () => null, toDataURL: () => '' });
    expect(() => renderPng(detail(), {}, noCtx)).toThrow(/no 2D canvas/);
  });
});
