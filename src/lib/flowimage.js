// Drawing a flow's evidence card with no editor open, for MCP's `history_image`.
//
// The renderer is the one the operator uses (capture.js), deliberately: an agent's card
// and an operator's card can end up in the same report, so there is one card shape and
// one piece of code that draws it. What is here is only the part the editor otherwise
// does by hand - turning a flow detail into a `source`, turning tool arguments into
// `opts` - plus the offscreen draw.
import { buildSnapshot, drawSnapshot } from './capture.js';

/** The request/response text to draw, and the meta row above it.
 *
 *  `view` picks which version of the bytes: `wire` is what crossed the wire and is the
 *  evidence; `extension` is the readable form a decode extension produced, which for an
 *  encrypted API is the only version worth looking at. An `extension` ask with no such
 *  version falls back to the wire rather than drawing an empty card, and says so in the
 *  returned `view` so the receipt cannot claim something that did not happen. */
export function sourceFromDetail(detail, view = 'wire', title = null) {
  const s = detail?.summary ?? {};
  const v = detail?.variants ?? {};
  const wantExt = view === 'extension';
  const req = (wantExt && v.reqExtension) || detail?.requestRaw || '';
  const res = (wantExt && v.resExtension) || detail?.responseRaw || '';
  const used = wantExt && (v.reqExtension || v.resExtension) ? 'extension' : 'wire';
  return {
    view: used,
    source: {
      method: s.method,
      host: s.host,
      url: `${s.scheme ?? 'http'}://${s.host ?? ''}${s.path ?? ''}`,
      status: s.status,
      durationMs: s.durationMs,
      // The window's Capture image titles the card with the flow's note, so a card
      // asked for over MCP carries the operator's own words unless the caller replaced
      // them. An empty string from the caller means "no title", which is why this
      // checks for null rather than falsiness.
      title: title ?? s.note ?? '',
      request: req,
      response: res,
      requestView: req,
      responseView: res,
    },
  };
}

/** Tool arguments as capture.js options. Anything absent keeps capture.js's default,
 *  which is also what the window starts with, so a card asked for with no options is
 *  the card the operator would have taken. */
export function optsFromRequest(req = {}) {
  return {
    theme: req.theme === 'light' ? 'light' : 'dark',
    layout: req.layout === 'stacked' ? 'stacked' : 'side',
    autoRedact: req.redact !== false,
    autoTrim: req.trim !== false,
    title: '',
    // Beautify is an editor-level toggle applied before this point; a card drawn here
    // shows the bytes as they are, so there is nothing to undo later.
    showTime: true,
  };
}

/** The PNG as base64 (no data: prefix), drawn offscreen at `scale`.
 *
 *  `canvasFor` is injectable because jsdom has no canvas: the tests exercise everything
 *  around the draw, and the draw itself is the code the window runs every day. */
export function renderPng(detail, req = {}, canvasFor = defaultCanvas) {
  const { view, source } = sourceFromDetail(detail, req.view, req.title ?? null);
  const opts = { ...optsFromRequest(req), title: source.title };
  const scale = Math.min(3, Math.max(1, Math.round(Number(req.scale) || 1)));

  const measure = canvasFor(1, 1);
  const snap = buildSnapshot(source, opts, measure);
  const off = canvasFor(Math.round(snap.width * scale), Math.round(snap.height * scale));
  const ctx = off.getContext('2d');
  if (!ctx) throw new Error('this window has no 2D canvas, so the card cannot be drawn');
  drawSnapshot(ctx, snap, { marks: [], redacts: [] }, scale);

  const url = off.toDataURL('image/png');
  const comma = url.indexOf(',');
  if (comma < 0) throw new Error('the canvas did not return a PNG');
  return { data: url.slice(comma + 1), width: off.width, height: off.height, view, scale };
}

function defaultCanvas(w, h) {
  const c = document.createElement('canvas');
  c.width = w;
  c.height = h;
  return c;
}
