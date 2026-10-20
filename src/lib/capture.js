// Snapshot renderer: turn a request/response into a clean, presentable, auto-redacted
// "evidence card" that can be marked up and exported as a crisp PNG. Everything is drawn
// to a canvas (no screenshot) so it is sharp at any export scale and never leaks the
// pixels under a redaction. The editor (CaptureEditor.svelte) drives it.
import { BRAND } from './brand.js';

// ---- Metrics (logical px; the canvas is supersampled by `scale` for crisp output) ----
const FONT_PX = 12.5;
const LH = 18; // line height
const MONO = `${FONT_PX}px ui-monospace, "SF Mono", Menlo, Consolas, monospace`;
const MONO_BOLD = `bold ${MONO}`;
const SANS = 'ui-sans-serif, system-ui, sans-serif';
const BRAND_ORANGE = '#e8712a'; // the classic capture wordmark colour
const MARGIN = 26; // themed backdrop around the card (watermark bleeds over it)
const CARD_PAD = 16; // inside the card edge
const GUTTER_PAD = 10; // gap between line-number gutter and code
const COL_GAP = 1; // divider between request & response columns
const CONTENT_SIDE = 500; // code width per column, side-by-side
const CONTENT_STACK = 900; // code width, stacked
const MAX_LINES = 400; // safety cap per message (visual lines)
// Auto-trim (when enabled): cap one body line's chars, and a tall body's line count.
const LINE_CAP = 400; // chars kept from a single over-long body line
const MAX_BODY = 60; // body logical lines above which the middle is trimmed
const KEEP_HEAD = 40; // body lines kept from the top
const KEEP_TAIL = 8; // body lines kept from the bottom
export const EXPORT_SCALES = [1, 2, 3];

// Two self-contained palettes so the snapshot's Light/Dark is independent of the app's
// current theme (values mirror src/app.css).
const DARK = {
  backdrop: '#0b0d12', card: '#13161d', soft: '#1a1e27', border: '#232836',
  text: '#e4e7ef', dim: '#8a92a6', faint: '#5a6275', accent: '#8b7bff',
  green: '#3ecf8e', blue: '#5eb1ff', amber: '#f5b454', red: '#ff6b7a', pink: '#f472b6', cyan: '#4fd1c5',
  bar: '#343a4a', barHi: '#454c5e', wm: '#ffffff',
};
const LIGHT = {
  backdrop: '#e8ebf1', card: '#ffffff', soft: '#f3f5f9', border: '#e1e4ec',
  text: '#1b1f2a', dim: '#5f6679', faint: '#9aa1b2', accent: '#5b4bff',
  green: '#0f9f6e', blue: '#1f7ae0', amber: '#c27c0e', red: '#e0344a', pink: '#c02d86', cyan: '#0e8f86',
  bar: '#c7ccd8', barHi: '#b4bac9', wm: '#000000',
};
export const MARK_COLORS = ['#ff5c6c', '#ff9345', '#f2c94c', '#3ecf8e', '#4f9dff', '#9b7bff'];

export const DEFAULT_OPTS = {
  theme: 'dark', // 'dark' | 'light'
  layout: 'side', // 'side' | 'stacked'
  redactStyle: 'blur', // 'blur' | 'solid' | 'dots'
  autoRedact: true,
  autoTrim: true, // trim over-long bodies while keeping context (head + tail + marker)
  // Editor-level (handled in CaptureEditor; here for discoverability):
  beautifyReq: true, // pretty-print the request body
  beautifyRes: true, // pretty-print the response body
  followUi: false, // capture only the region currently scrolled into view in the app
  showTime: true,
  watermark: '', // text; supports {host} and {date}
  notesMode: 'callout', // 'callout' | 'legend'
  markColor: MARK_COLORS[0],
  title: '',
};

const paletteFor = (theme) => (theme === 'light' ? LIGHT : DARK);

function statusColor(t, code) {
  if (!code) return t.red;
  if (code < 300) return t.green;
  if (code < 400) return t.blue;
  if (code < 500) return t.amber;
  return t.red;
}
function methodColor(t, method) {
  switch ((method || '').toUpperCase()) {
    case 'GET': return t.green;
    case 'POST': return t.amber;
    case 'PUT':
    case 'PATCH': return t.blue;
    case 'DELETE': return t.red;
    case 'OPTIONS':
    case 'HEAD': return t.dim;
    default: return t.accent;
  }
}

/** Body kind for colouring: 'json', 'markup', or '' (plain). */
export function bodyType(text) {
  const c = (text ?? '').trimStart()[0];
  if (c === '{' || c === '[') return 'json';
  if (c === '<') return 'markup';
  return '';
}

const JSON_TOKEN = /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g;
const MARKUP_TOKEN = /(<\/?)([\w:-]+)|([\w:-]+)(=)("[^"]*"|'[^']*')|(\/?>)/g;

/** Split one body line into coloured segments [{text, color}] (matches highlight.js). */
export function tokenizeLine(line, type, t) {
  const segs = [];
  const push = (s, color) => s && segs.push({ text: s, color });
  let last = 0, m;
  if (type === 'json') {
    JSON_TOKEN.lastIndex = 0;
    while ((m = JSON_TOKEN.exec(line))) {
      push(line.slice(last, m.index), t.text);
      if (m[1]) { if (m[2]) { push(m[1], t.blue); push(m[2], t.text); } else push(m[1], t.green); }
      else if (m[3]) push(m[3], t.pink);
      else if (m[4]) push(m[4], t.amber);
      last = JSON_TOKEN.lastIndex;
    }
    push(line.slice(last), t.text);
  } else if (type === 'markup') {
    MARKUP_TOKEN.lastIndex = 0;
    while ((m = MARKUP_TOKEN.exec(line))) {
      push(line.slice(last, m.index), t.text);
      if (m[2]) { push(m[1], t.dim); push(m[2], t.pink); }
      else if (m[3]) { push(m[3], t.cyan); push(m[4], t.text); push(m[5], t.green); }
      else if (m[6]) push(m[6], t.dim);
      last = MARKUP_TOKEN.lastIndex;
    }
    push(line.slice(last), t.text);
  } else {
    push(line, t.text);
  }
  return segs;
}

function startSegs(line, t) {
  const parts = line.split(' ');
  if (/^HTTP\/\d/.test(parts[0])) {
    const [version, status = '', ...reason] = parts;
    return [
      { text: version, color: t.faint },
      { text: status ? ' ' + status : '', color: statusColor(t, parseInt(status, 10) || 0), bold: true },
      { text: reason.length ? ' ' + reason.join(' ') : '', color: t.faint },
    ];
  }
  if (parts.length >= 3) {
    const method = parts[0];
    const version = parts[parts.length - 1];
    const path = parts.slice(1, -1).join(' ');
    return [
      { text: method, color: methodColor(t, method), bold: true },
      { text: ' ' + path + ' ', color: t.text },
      { text: version, color: t.faint },
    ];
  }
  return [{ text: line, color: t.text }];
}
function headerSegs(line, t) {
  const i = line.indexOf(':');
  if (i <= 0) return [{ text: line, color: t.text }];
  return [
    { text: line.slice(0, i), color: t.accent },
    { text: ':', color: t.dim },
    { text: line.slice(i + 1), color: t.text },
  ];
}

// ---- Auto-redaction -------------------------------------------------------------
const SENS_HEADER = new Set(['x-api-key', 'api-key', 'apikey', 'x-auth-token', 'authentication', 'x-csrf-token', 'x-xsrf-token', 'x-session-token']);
const SENS_KEY = /pass(word|wd)?|secret|token|api[_-]?key|apikey|auth|session|credential|private[_-]?key|access[_-]?key|client[_-]?secret|refresh|otp|cvv|ssn/i;
const KEEP = 4; // trailing chars kept visible

/** Redaction spans [{c1,c2,keep}] (char columns) for ONE logical line. */
function redactSpans(line, isStart, inHeaders, kind) {
  if (isStart) return [];
  if (inHeaders) {
    const m = /^(\s*)([A-Za-z0-9-]+)(\s*:\s*)(.*)$/.exec(line);
    if (!m) return [];
    const name = m[2].toLowerCase();
    const vStart = m[1].length + m[2].length + m[3].length;
    const val = m[4];
    const spans = [];
    if (name === 'cookie' || name === 'set-cookie') {
      const re = /([^=;,\s][^=;]*)=([^;,]*)/g; let x;
      while ((x = re.exec(val))) {
        const vs = vStart + x.index + x[1].length + 1;
        if (x[2].length > KEEP) spans.push({ c1: vs, c2: vs + x[2].length, keep: KEEP });
        if (name === 'set-cookie') break; // only the cookie value, keep attributes readable
      }
    } else if (name === 'authorization' || name === 'proxy-authorization') {
      const mm = /^(\s*)(\S+)(\s+)(.+)$/.exec(val);
      if (mm) { const ts = vStart + mm[1].length + mm[2].length + mm[3].length; const te = vStart + val.length; if (te - ts > KEEP) spans.push({ c1: ts, c2: te, keep: KEEP }); }
      else if (val.trim().length > KEEP) spans.push({ c1: vStart, c2: vStart + val.length, keep: KEEP });
    } else if (SENS_HEADER.has(name)) {
      if (val.trim().length > KEEP) spans.push({ c1: vStart, c2: vStart + val.length, keep: KEEP });
    }
    return spans;
  }
  // body: scan for "key": "value" (or non-string value) anywhere on the line, so both
  // pretty-printed and compact JSON are covered.
  const re = /"([^"]+)"\s*:\s*("(?:\\.|[^"\\])*"|[^,}\]\s]+)/g;
  const spans = []; let x;
  while ((x = re.exec(line))) {
    if (!SENS_KEY.test(x[1])) continue;
    const vFull = x[2];
    const vStart = x.index + (x[0].length - vFull.length);
    if (vFull[0] === '"') { const is = vStart + 1; const inner = vFull.slice(1, -1); if (inner.length > KEEP) spans.push({ c1: is, c2: is + inner.length, keep: KEEP }); }
    else if (vFull.length > KEEP) spans.push({ c1: vStart, c2: vStart + vFull.length, keep: KEEP });
  }
  return spans;
}

// ---- Layout -----------------------------------------------------------------
function measureCharW(canvas) {
  const ctx = canvas && canvas.getContext && canvas.getContext('2d');
  if (!ctx) return FONT_PX * 0.6;
  ctx.font = MONO;
  return ctx.measureText('M0abcABC').width / 8 || FONT_PX * 0.6;
}

// Split a message into logical entries (start line / headers / separator / body),
// keeping each entry's ORIGINAL line number. When `autoTrim` is on, the body is
// trimmed to stay presentable while keeping context: every over-long body line is
// capped (great for minified blobs), and a very tall body keeps its head + tail with a
// "… N lines trimmed …" marker in the middle. Headers are never trimmed.
function messageEntries(text, autoTrim) {
  const logical = (text ?? '').replace(/\r/g, '').split('\n');
  const blank = logical.indexOf('');
  const headEnd = blank >= 0 ? blank : logical.length;
  const entries = [];
  for (let i = 0; i < headEnd; i++) entries.push({ text: logical[i], kind: i === 0 ? 'start' : 'header', no: i + 1 });
  if (blank < 0) return entries;
  entries.push({ text: '', kind: 'sep', no: blank + 1 });
  let body = [];
  for (let i = blank + 1; i < logical.length; i++) body.push({ text: logical[i], kind: 'body', no: i + 1 });
  if (autoTrim && body.length) {
    body = body.map((e) =>
      e.text.length > LINE_CAP ? { ...e, text: e.text.slice(0, LINE_CAP) + ` … [+${e.text.length - LINE_CAP} chars]` } : e,
    );
    if (body.length > MAX_BODY) {
      const omitted = body.length - KEEP_HEAD - KEEP_TAIL;
      body = [
        ...body.slice(0, KEEP_HEAD),
        { text: `⋯ ${omitted} line${omitted === 1 ? '' : 's'} trimmed ⋯`, kind: 'marker', no: null },
        ...body.slice(-KEEP_TAIL),
      ];
    }
  }
  entries.push(...body);
  return entries;
}

// Per-char colour + redaction for each entry, wrapped into visual slices. With a
// `range` {start,end} (0-based logical line indices) only those lines are emitted -
// used by "follow page scroll" to capture just the region visible in the app, with
// "N lines above/below" markers for context.
function processMessage(text, kind, t, cols, autoRedact, bType, autoTrim, range) {
  let entries = messageEntries(text, range ? false : autoTrim);
  if (range) {
    const total = entries.reduce((m, e) => Math.max(m, e.no || 0), 0);
    const kept = entries.filter((e) => e.no != null && e.no - 1 >= range.start && e.no - 1 < range.end);
    const out = [];
    if (range.start > 0) out.push({ text: `⋯ ${range.start} line${range.start === 1 ? '' : 's'} above ⋯`, kind: 'marker', no: null });
    out.push(...kept);
    if (range.end < total) out.push({ text: `⋯ ${total - range.end} more line${total - range.end === 1 ? '' : 's'} below ⋯`, kind: 'marker', no: null });
    entries = out;
  }
  const vis = [];
  for (let idx = 0; idx < entries.length && vis.length < MAX_LINES; idx++) {
    const e = entries[idx];
    const line = e.text;
    let segs;
    if (e.kind === 'start') segs = startSegs(line, t);
    else if (e.kind === 'header' && line.includes(':')) segs = headerSegs(line, t);
    else if (e.kind === 'marker') segs = [{ text: line, color: t.faint }];
    else segs = tokenizeLine(line, bType, t);
    // per-char colour
    const colors = [];
    for (const s of segs) for (const ch of s.text) colors.push({ c: s.color, b: !!s.bold, ch });
    // per-char redaction
    const red = new Array(line.length).fill(false);
    if (autoRedact && e.kind !== 'marker') for (const sp of redactSpans(line, e.kind === 'start', e.kind === 'header', kind)) for (let i = sp.c1; i < sp.c2 - sp.keep; i++) if (i >= 0 && i < red.length) red[i] = true;
    // wrap
    const n = Math.max(1, Math.ceil(line.length / cols));
    for (let k = 0; k < n && vis.length < MAX_LINES; k++) {
      const a = k * cols, b = Math.min(line.length, a + cols);
      const cells = colors.slice(a, b);
      const rd = red.slice(a, b);
      let c1 = -1, c2 = -1;
      for (let i = 0; i < cells.length; i++) if (cells[i].ch !== ' ') { if (c1 < 0) c1 = i; c2 = i + 1; }
      vis.push({ no: k === 0 ? e.no : null, cells, rd, c1: a + (c1 < 0 ? 0 : c1), c2: a + (c2 < 0 ? b - a : c2), off: a });
    }
  }
  return vis;
}

/** Build the snapshot layout from a flow source + options. Pure geometry + colours. */
export function buildSnapshot(source, opts, canvas) {
  const o = { ...DEFAULT_OPTS, ...opts };
  const t = paletteFor(o.theme);
  const charW = measureCharW(canvas);
  const stacked = o.layout === 'stacked';
  const contentW = stacked ? CONTENT_STACK : CONTENT_SIDE;

  const reqText = source.request ?? '';
  const resText = source.response ?? '';
  const reqBody = bodyType(reqText.split('\n\n')[1] ?? '');
  const resBody = bodyType(resText.split('\n\n')[1] ?? '');
  const cols = Math.max(24, Math.floor(contentW / charW));
  const reqLines = processMessage(reqText, 'req', t, cols, o.autoRedact, reqBody, o.autoTrim, o.reqRange);
  const resLines = processMessage(resText, 'res', t, cols, o.autoRedact, resBody, o.autoTrim, o.resRange);

  const maxNo = Math.max(reqLines.length, resLines.length, 1);
  const gutter = Math.ceil(String(maxNo).length * charW) + GUTTER_PAD * 2;
  const colW = gutter + contentW + CARD_PAD; // gutter + code + right padding

  // Header geometry
  const hasTitle = !!(o.title || '').trim();
  const brandH = 18; // "Ngintip" wordmark row
  const titleH = hasTitle ? 28 : 0;
  const metaH = 34; // method / url / status row
  const colTitleH = 26;
  const headerH = CARD_PAD + brandH + titleH + metaH;

  let cardW, columns, bodyTop;
  if (stacked) {
    cardW = CARD_PAD + colW;
    const reqH = colTitleH + reqLines.length * LH + 8;
    bodyTop = headerH;
    columns = [
      { key: 'req', title: 'Request', x: CARD_PAD, y: bodyTop, w: colW - CARD_PAD, gutter, textX: CARD_PAD + gutter, lines: reqLines, colTitleH },
      { key: 'res', title: 'Response', x: CARD_PAD, y: bodyTop + reqH + 10, w: colW - CARD_PAD, gutter, textX: CARD_PAD + gutter, lines: resLines, colTitleH },
    ];
  } else {
    cardW = CARD_PAD + colW + COL_GAP + colW;
    bodyTop = headerH;
    columns = [
      { key: 'req', title: 'Request', x: CARD_PAD, y: bodyTop, w: colW, gutter, textX: CARD_PAD + gutter, lines: reqLines, colTitleH },
      { key: 'res', title: 'Response', x: CARD_PAD + colW + COL_GAP, y: bodyTop, w: colW, gutter, textX: CARD_PAD + colW + COL_GAP + gutter, lines: resLines, colTitleH },
    ];
  }
  // Each column's first code line sits below its own title strip.
  for (const c of columns) c.codeTop = c.y + c.colTitleH + 6;

  const columnsBottom = Math.max(...columns.map((c) => c.codeTop + c.lines.length * LH)) + 8;
  const legendH = o.legendLines ? o.legendLines * 20 + 16 : 0;
  const cardH = columnsBottom + CARD_PAD + legendH;

  const width = Math.round(MARGIN * 2 + cardW);
  const height0 = Math.round(MARGIN * 2 + cardH);

  return {
    opts: o, t, charW, LH,
    card: { x: MARGIN, y: MARGIN, w: cardW, h: cardH },
    headerH, brandH, titleH, metaH, hasTitle, legendH,
    columns, cols,
    width, height: height0,
    source,
  };
}

/** Pixel rect of a visual line's trimmed content (for the Mark box). */
export function lineContentRect(snap, colKey, lineIdx) {
  const col = snap.columns.find((c) => c.key === colKey);
  if (!col || !col.lines[lineIdx]) return null;
  const ln = col.lines[lineIdx];
  const x1 = col.textX + (ln.c1 - ln.off) * snap.charW;
  const x2 = col.textX + (ln.c2 - ln.off) * snap.charW;
  const y = col.codeTop + lineIdx * snap.LH;
  return { x: x1 - 3, y: y - 1, w: Math.max(12, x2 - x1) + 6, h: snap.LH, cx: col.textX, charW: snap.charW, lineY: y };
}

/** Hit-test a point to a {col, line, char} (char = column index within the visual line). */
export function hitTest(snap, px, py) {
  for (const col of snap.columns) {
    if (px < col.x || px > col.x + col.w) continue;
    const i = Math.floor((py - col.codeTop) / snap.LH);
    if (i < 0 || i >= col.lines.length) continue;
    const ch = Math.max(0, Math.round((px - col.textX) / snap.charW));
    return { col: col.key, line: i, char: ch };
  }
  return null;
}

/** Word (value token) char range [c1,c2) around a char index on a visual line. */
export function wordAt(snap, colKey, lineIdx, ch) {
  const col = snap.columns.find((c) => c.key === colKey);
  const ln = col && col.lines[lineIdx];
  if (!ln) return null;
  const s = ln.cells.map((x) => x.ch).join('');
  let a = Math.min(Math.max(0, ch), s.length), b = a;
  const stop = (c) => c === undefined || /[\s{}\[\],:"=;]/.test(c);
  while (a > 0 && !stop(s[a - 1])) a--;
  while (b < s.length && !stop(s[b])) b++;
  if (b <= a) return null;
  return { c1: a, c2: b };
}

// ---- Drawing ----------------------------------------------------------------
function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

function drawRedBar(ctx, t, style, x, y, w, h) {
  if (style === 'solid') { ctx.fillStyle = t.bar; roundRect(ctx, x, y + 1, w, h - 3, 3); ctx.fill(); return; }
  if (style === 'dots') {
    ctx.fillStyle = t.dim;
    const r = 1.4, step = 5; let cx = x + 3;
    while (cx < x + w - 2) { ctx.beginPath(); ctx.arc(cx, y + h / 2, r, 0, Math.PI * 2); ctx.fill(); cx += step; }
    return;
  }
  // blur: frosted bar with streaks (no secret pixels exist underneath)
  const g = ctx.createLinearGradient(x, y, x, y + h);
  g.addColorStop(0, t.barHi); g.addColorStop(1, t.bar);
  ctx.fillStyle = g; roundRect(ctx, x, y + 1, w, h - 3, 3); ctx.fill();
  ctx.save(); roundRect(ctx, x, y + 1, w, h - 3, 3); ctx.clip();
  ctx.strokeStyle = t.barHi; ctx.globalAlpha = 0.5; ctx.lineWidth = 1.5;
  for (let sx = x - h; sx < x + w; sx += 4) { ctx.beginPath(); ctx.moveTo(sx, y + h); ctx.lineTo(sx + h, y); ctx.stroke(); }
  ctx.restore();
}

function drawColumnText(ctx, snap, col) {
  const t = snap.t, charW = snap.charW;
  for (let i = 0; i < col.lines.length; i++) {
    const ln = col.lines[i];
    const y = col.codeTop + i * snap.LH;
    // line number
    if (ln.no != null) {
      ctx.font = MONO; ctx.fillStyle = t.faint; ctx.textAlign = 'right'; ctx.textBaseline = 'top';
      ctx.fillText(String(ln.no), col.x + col.gutter - GUTTER_PAD, y + 2);
      ctx.textAlign = 'left';
    }
    // chars: draw non-redacted runs as text, redacted runs as bars
    ctx.textBaseline = 'top';
    let i0 = 0;
    while (i0 < ln.cells.length) {
      if (ln.rd[i0]) {
        let j = i0; while (j < ln.cells.length && ln.rd[j]) j++;
        drawRedBar(ctx, t, snap.opts.redactStyle, col.textX + i0 * charW, y, (j - i0) * charW, snap.LH);
        i0 = j;
      } else {
        let j = i0; while (j < ln.cells.length && !ln.rd[j]) j++;
        // draw run char by char grouped by colour/bold
        let k = i0;
        while (k < j) {
          const { c, b } = ln.cells[k]; let m = k;
          let s = '';
          while (m < j && ln.cells[m].c === c && ln.cells[m].b === b) { s += ln.cells[m].ch; m++; }
          ctx.font = b ? MONO_BOLD : MONO; ctx.fillStyle = c;
          ctx.fillText(s, col.textX + k * charW, y + 2);
          k = m;
        }
        i0 = j;
      }
    }
  }
}

function drawWatermark(ctx, snap) {
  const text = resolveWatermark(snap.opts.watermark, snap.source);
  if (!text) return;
  const t = snap.t;
  ctx.save();
  ctx.globalAlpha = snap.opts.theme === 'light' ? 0.08 : 0.07;
  ctx.fillStyle = t.wm;
  ctx.font = `bold 13px ${SANS}`;
  ctx.translate(snap.width / 2, snap.height / 2);
  ctx.rotate(-Math.PI / 9);
  ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
  const stepX = Math.max(180, ctx.measureText(text).width + 90), stepY = 46;
  const n = Math.ceil(Math.hypot(snap.width, snap.height) / 2);
  for (let y = -n; y < n; y += stepY) for (let x = -n; x < n; x += stepX) ctx.fillText(text, x + ((Math.round(y / stepY) % 2) ? stepX / 2 : 0), y);
  ctx.restore();
}
function resolveWatermark(tpl, source) {
  if (!tpl) return '';
  const host = (() => { try { return new URL(source.url || source.target || '').host; } catch { return source.host || ''; } })();
  const date = new Date().toISOString().slice(0, 10);
  return tpl.replace(/\{host\}/gi, host).replace(/\{date\}/gi, date);
}

/**
 * Draw the whole snapshot to `ctx` at `scale`. `state` carries the editor's marks and
 * manual redactions: { marks:[{col,line,color,n,note,dx,dy}], redacts:[{col,line,c1,c2}] }.
 * Returns extra geometry (legend rects) for hit-testing the editor doesn't need here.
 */
export function drawSnapshot(ctx, snap, state = {}, scale = 1) {
  const t = snap.t, o = snap.opts;
  ctx.setTransform(scale, 0, 0, scale, 0, 0);
  ctx.imageSmoothingEnabled = true;
  ctx.clearRect(0, 0, snap.width, snap.height);
  // backdrop
  ctx.fillStyle = t.backdrop; ctx.fillRect(0, 0, snap.width, snap.height);
  // card
  const c = snap.card;
  ctx.fillStyle = t.card; roundRect(ctx, c.x, c.y, c.w, c.h, 12); ctx.fill();
  ctx.strokeStyle = t.border; ctx.lineWidth = 1; roundRect(ctx, c.x + 0.5, c.y + 0.5, c.w - 1, c.h - 1, 12); ctx.stroke();
  // watermark tiled faintly OVER the card (behind the content, clipped to the card)
  ctx.save(); roundRect(ctx, c.x, c.y, c.w, c.h, 12); ctx.clip(); drawWatermark(ctx, snap); ctx.restore();

  ctx.save();
  ctx.translate(c.x, c.y);
  let hy = CARD_PAD;
  // brand wordmark (kept from the classic capture)
  ctx.font = `bold 11px ${SANS}`; ctx.fillStyle = BRAND_ORANGE; ctx.textBaseline = 'top'; ctx.textAlign = 'left';
  ctx.fillText(BRAND.name, CARD_PAD, hy);
  hy += snap.brandH;
  // title
  if (snap.hasTitle) {
    ctx.font = `bold 15px ${SANS}`; ctx.fillStyle = t.text; ctx.textBaseline = 'top'; ctx.textAlign = 'left';
    ctx.fillText(o.title, CARD_PAD, hy + 1);
    hy += snap.titleH;
  }
  // meta row: method pill + url + status + time
  const s = snap.source;
  let mx = CARD_PAD;
  ctx.textBaseline = 'middle';
  const midY = hy + snap.metaH / 2;
  if (s.method) {
    ctx.font = `bold 11px ${SANS}`;
    const label = s.method.toUpperCase();
    const w = ctx.measureText(label).width + 16;
    ctx.fillStyle = methodColor(t, s.method);
    ctx.globalAlpha = 0.16; roundRect(ctx, mx, midY - 10, w, 20, 5); ctx.fill(); ctx.globalAlpha = 1;
    ctx.fillStyle = methodColor(t, s.method); ctx.fillText(label, mx + 8, midY + 0.5);
    mx += w + 10;
  }
  const url = s.url || s.target || '';
  ctx.font = MONO; ctx.fillStyle = t.dim;
  const timeW = o.showTime && s.durationMs != null ? 70 : 0;
  const statusW = s.status ? 60 : 0;
  const urlMax = c.w - mx - CARD_PAD - timeW - statusW;
  ctx.fillText(ellipsize(ctx, url, urlMax), mx, midY + 0.5);
  ctx.textAlign = 'right';
  let rx = c.w - CARD_PAD;
  if (o.showTime && s.durationMs != null) { ctx.font = MONO; ctx.fillStyle = t.dim; ctx.fillText(`${s.durationMs} ms`, rx, midY + 0.5); rx -= timeW; }
  if (s.status) { ctx.font = MONO_BOLD; ctx.fillStyle = statusColor(t, s.status); ctx.fillText(`${s.status}${statusText(s.status)}`, rx, midY + 0.5); }
  ctx.textAlign = 'left';
  ctx.restore();

  // columns
  for (const col of snap.columns) {
    // divider strip between columns (side layout): a subtle top rule under the title
    ctx.fillStyle = t.soft; roundRect(ctx, c.x + col.x, c.y + col.y, col.w, col.colTitleH, 4); ctx.fill();
    ctx.font = `bold 10.5px ${SANS}`; ctx.fillStyle = t.dim; ctx.textBaseline = 'middle'; ctx.textAlign = 'left';
    ctx.fillText(col.title.toUpperCase(), c.x + col.x + 10, c.y + col.y + col.colTitleH / 2 + 0.5);
    // gutter divider line
    ctx.strokeStyle = t.border; ctx.lineWidth = 1;
    const gx = c.x + col.x + col.gutter - GUTTER_PAD / 2;
    ctx.beginPath(); ctx.moveTo(gx, c.y + col.codeTop - 2); ctx.lineTo(gx, c.y + col.codeTop + col.lines.length * snap.LH); ctx.stroke();
    // code (translate into card space)
    ctx.save(); ctx.translate(c.x, c.y); drawColumnText(ctx, snap, col); ctx.restore();
  }
  // vertical divider between request & response (side layout)
  if (o.layout !== 'stacked' && snap.columns[1]) {
    ctx.strokeStyle = t.border; ctx.lineWidth = 1;
    const dx = c.x + snap.columns[1].x - COL_GAP;
    ctx.beginPath(); ctx.moveTo(dx, c.y + snap.columns[0].y); ctx.lineTo(dx, c.y + c.h - CARD_PAD); ctx.stroke();
  }

  // manual redactions (c1/c2 are visual-line-relative character indices)
  for (const r of state.redacts || []) {
    const col = snap.columns.find((x) => x.key === r.col); if (!col || !col.lines[r.line]) continue;
    const x = c.x + col.textX + r.c1 * snap.charW;
    drawRedBar(ctx, t, o.redactStyle, x, c.y + col.codeTop + r.line * snap.LH, (r.c2 - r.c1) * snap.charW, snap.LH);
  }

  // marks + notes
  const marks = state.marks || [];
  const noteRects = [];
  marks.forEach((mk, i) => {
    const rect = lineContentRect(snap, mk.col, mk.line); if (!rect) return;
    const bx = c.x + rect.x, by = c.y + rect.y;
    if (state.sel === i) { ctx.save(); ctx.strokeStyle = mk.color; ctx.globalAlpha = 0.33; ctx.lineWidth = 6; roundRect(ctx, bx - 2, by - 2, rect.w + 4, rect.h + 4, 6); ctx.stroke(); ctx.restore(); }
    ctx.strokeStyle = mk.color; ctx.lineWidth = 2; roundRect(ctx, bx, by, rect.w, rect.h, 4); ctx.stroke();
    badge(ctx, bx - 2, by - 2, mk.n, mk.color);
    if (o.notesMode === 'callout' && (mk.note || '').trim()) {
      const nx = bx + rect.w + 16 + (mk.dx || 0), ny = by + (mk.dy || 0);
      const nr = drawNote(ctx, snap, mk, nx, ny, bx + rect.w, by + rect.h / 2);
      noteRects.push({ i, ...nr });
    }
  });
  // legend
  if (o.notesMode === 'legend') {
    const withNotes = marks.filter((m) => (m.note || '').trim());
    if (withNotes.length) {
      let ly = c.y + c.h - (snap.legendH || withNotes.length * 20 + 16) + 8;
      ctx.strokeStyle = t.border; ctx.lineWidth = 1; ctx.beginPath(); ctx.moveTo(c.x + CARD_PAD, ly - 8); ctx.lineTo(c.x + c.w - CARD_PAD, ly - 8); ctx.stroke();
      ctx.textBaseline = 'middle'; ctx.textAlign = 'left';
      for (const m of withNotes) { badge(ctx, c.x + CARD_PAD, ly, m.n, m.color, true); ctx.font = `12px ${SANS}`; ctx.fillStyle = t.text; ctx.fillText(ellipsize(ctx, m.note, c.w - CARD_PAD * 2 - 28), c.x + CARD_PAD + 26, ly + 9); ly += 20; }
    }
  }
  return { noteRects };
}

function drawNote(ctx, snap, mk, nx, ny, fromX, fromY) {
  const t = snap.t;
  ctx.font = `12px ${SANS}`;
  const padX = 9, padY = 6, maxW = 240;
  const text = mk.note;
  const w = Math.min(maxW, ctx.measureText(text).width + 26 + padX * 2);
  const h = 24;
  // leader line
  ctx.strokeStyle = mk.color; ctx.lineWidth = 1.5; ctx.globalAlpha = 0.8;
  ctx.beginPath(); ctx.moveTo(fromX, fromY); ctx.lineTo(nx, ny + h / 2); ctx.stroke(); ctx.globalAlpha = 1;
  // box
  ctx.fillStyle = t.card; roundRect(ctx, nx, ny, w, h, 6); ctx.fill();
  ctx.strokeStyle = mk.color; ctx.lineWidth = 1.5; roundRect(ctx, nx + 0.5, ny + 0.5, w - 1, h - 1, 6); ctx.stroke();
  badge(ctx, nx + 5, ny + 4, mk.n, mk.color, true);
  ctx.fillStyle = t.text; ctx.textBaseline = 'middle'; ctx.textAlign = 'left';
  ctx.fillText(ellipsize(ctx, text, w - 26 - padX * 2 + 6), nx + 28, ny + h / 2 + 0.5);
  return { x: nx, y: ny, w, h }; // logical rect, for hit-testing note drags
}

// ---- Freehand annotations (the classic capture tools, drawn over the snapshot) ----
// Shapes are in logical snapshot coords; the ctx is already scaled by the caller.
export function drawShapes(ctx, shapes) {
  for (const s of shapes || []) drawShape(ctx, s);
}
export function drawShape(ctx, s) {
  if (!s) return;
  ctx.save();
  ctx.lineWidth = s.width || 3;
  ctx.strokeStyle = s.color; ctx.fillStyle = s.color;
  ctx.lineCap = 'round'; ctx.lineJoin = 'round';
  const x = Math.min(s.x, s.x2), y = Math.min(s.y, s.y2), w = Math.abs(s.x2 - s.x), h = Math.abs(s.y2 - s.y);
  if (s.type === 'rect') ctx.strokeRect(x, y, w, h);
  else if (s.type === 'ellipse') { ctx.beginPath(); ctx.ellipse(x + w / 2, y + h / 2, w / 2, h / 2, 0, 0, Math.PI * 2); ctx.stroke(); }
  else if (s.type === 'line') seg(ctx, s.x, s.y, s.x2, s.y2);
  else if (s.type === 'arrow') { seg(ctx, s.x, s.y, s.x2, s.y2); arrowHead(ctx, s.x, s.y, s.x2, s.y2, s.width || 3); }
  else if (s.type === 'pen') penPath(ctx, s.points);
  else if (s.type === 'highlight') { ctx.globalAlpha = 0.3; ctx.lineWidth = (s.width || 3) * 3.5; penPath(ctx, s.points); }
  else if (s.type === 'text') {
    const size = 13 + (s.width || 3) * 2;
    ctx.font = `bold ${size}px ${SANS}`;
    const tw = ctx.measureText(s.text || '').width;
    ctx.globalAlpha = 0.55; ctx.fillStyle = '#000'; roundRect(ctx, s.x - 4, s.y - 3, tw + 8, size + 6, 4); ctx.fill(); ctx.globalAlpha = 1;
    ctx.fillStyle = s.color; ctx.textBaseline = 'top'; ctx.textAlign = 'left';
    ctx.fillText(s.text || '', s.x, s.y);
  }
  ctx.restore();
}
function seg(ctx, x1, y1, x2, y2) { ctx.beginPath(); ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.stroke(); }
function penPath(ctx, pts) { if (!pts || pts.length < 2) { if (pts && pts[0]) { ctx.beginPath(); ctx.arc(pts[0].x, pts[0].y, (ctx.lineWidth || 2) / 2, 0, Math.PI * 2); ctx.fillStyle = ctx.strokeStyle; ctx.fill(); } return; } ctx.beginPath(); ctx.moveTo(pts[0].x, pts[0].y); for (const p of pts.slice(1)) ctx.lineTo(p.x, p.y); ctx.stroke(); }
function arrowHead(ctx, x1, y1, x2, y2, w) {
  const len = 9 + w * 2, a = Math.atan2(y2 - y1, x2 - x1);
  ctx.beginPath(); ctx.moveTo(x2, y2);
  ctx.lineTo(x2 - len * Math.cos(a - Math.PI / 6), y2 - len * Math.sin(a - Math.PI / 6));
  ctx.lineTo(x2 - len * Math.cos(a + Math.PI / 6), y2 - len * Math.sin(a + Math.PI / 6));
  ctx.closePath(); ctx.fill();
}

function badge(ctx, x, y, n, color, small) {
  const r = small ? 8 : 9;
  ctx.fillStyle = color; ctx.beginPath(); ctx.arc(x + r, y + r, r, 0, Math.PI * 2); ctx.fill();
  ctx.fillStyle = '#fff'; ctx.font = `bold ${small ? 10 : 11}px ${SANS}`; ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
  ctx.fillText(String(n), x + r, y + r + 0.5); ctx.textAlign = 'left';
}

function ellipsize(ctx, text, maxW) {
  if (maxW <= 0 || ctx.measureText(text).width <= maxW) return text;
  let s = text;
  while (s.length > 1 && ctx.measureText(s + '…').width > maxW) s = s.slice(0, -1);
  return s + '…';
}
function statusText(code) {
  const map = { 200: ' OK', 201: ' Created', 204: ' No Content', 301: ' Moved', 302: ' Found', 304: ' Not Modified', 400: ' Bad Request', 401: ' Unauthorized', 403: ' Forbidden', 404: ' Not Found', 500: ' Server Error', 502: ' Bad Gateway', 503: ' Unavailable' };
  return map[code] || '';
}
