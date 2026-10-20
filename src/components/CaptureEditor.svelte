<script>
  // ============================================================================
  //  Snapshot editor - presentable, auto-redacted evidence from a request/response.
  //  The card is RENDERED (not a screenshot) by src/lib/capture.js, so it exports as a
  //  crisp PNG and never leaks the pixels under a redaction. On top of that card you get
  //  the classic freehand tools (rectangle, ellipse, arrow, line, pen, highlighter,
  //  text) plus the new semantic ones:
  //   - Mark:   click a line to box it + add a numbered note (callout or legend).
  //   - Redact: click a value to hide it (on top of the automatic redaction).
  //  Theme / Layout / Watermark / Export scale on the right; Copy or Save PNG.
  // ============================================================================
  import { onMount } from 'svelte';
  import { save } from '@tauri-apps/plugin-dialog';
  import { BRAND } from '../lib/brand.js';
  import { store } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import {
    buildSnapshot, drawSnapshot, drawShapes, drawShape, hitTest, wordAt,
    DEFAULT_OPTS, MARK_COLORS, EXPORT_SCALES,
  } from '../lib/capture.js';
  import { prettyBody } from '../lib/message.js';

  let { source, onclose } = $props();

  // Default to the app's current theme (follow Ngintip's light/dark).
  const appTheme = (typeof document !== 'undefined' && document.documentElement.getAttribute('data-theme')) || 'dark';

  // Options that shape the snapshot + export. `source` is a stable prop for one open.
  // svelte-ignore state_referenced_locally
  let opts = $state({ ...DEFAULT_OPTS, theme: appTheme === 'light' ? 'light' : 'dark', title: source?.title ?? '', exportScale: 2 });
  let tool = $state('mark'); // mark | redact | rect | ellipse | arrow | line | pen | highlight | text
  let width = $state(4); // freehand stroke width
  let marks = $state([]); // { col, line, color, note, dx, dy }
  let redacts = $state([]); // { col, line, c1, c2 }
  let shapes = $state([]); // freehand annotations (see capture.js drawShape)
  let zoom = $state(0); // 0 = fit

  let textPop = $state(null); // placing / editing a freehand text shape
  let drawing = null; // in-progress freehand shape (not reactive)
  let noteDrag = null;
  let selMark = $state(-1); // highlighted mark (for the on-canvas pulse)
  let selShape = $state(-1); // selected freehand shape (Select tool)
  let shapeDrag = null; // { i, mode:'move'|'resize', handle?, sx, sy, orig, began }
  let liveShape = null; // the shape being moved/resized (drawn over the base live)
  let hideShape = -1; // index excluded from the base while it is being dragged

  const FREE_TOOLS = [
    { id: 'rect', label: 'Rectangle', icon: '<rect x="4.5" y="6.5" width="15" height="11" rx="1.5"/>' },
    { id: 'ellipse', label: 'Ellipse', icon: '<ellipse cx="12" cy="12" rx="8" ry="6"/>' },
    { id: 'arrow', label: 'Arrow', icon: '<path d="M4 13h12"/><path d="M12 8l5 5-5 5"/>' },
    { id: 'line', label: 'Line', icon: '<path d="M5 19 19 5"/>' },
    { id: 'pen', label: 'Pen', icon: '<path d="M4 20l1.2-4.4L15 6a2 2 0 0 1 3 3l-9.6 9.6L4 20z"/><path d="M13.5 7.5 16.5 10.5"/>' },
    { id: 'highlight', label: 'Highlighter', icon: '<path d="M5 20h5"/><path d="M8.5 14.5 15 8l3 3-6.5 6.5-3.8.6.8-3.6z"/>' },
    { id: 'text', label: 'Text', icon: '<path d="M6 6h12"/><path d="M12 6v12"/>' },
  ];
  const WIDTHS = [{ id: 2, r: 2 }, { id: 4, r: 3.4 }, { id: 7, r: 5 }];

  const measureCanvas = typeof document !== 'undefined' ? document.createElement('canvas') : null;
  let baseCanvas; // offscreen: snapshot + committed shapes
  let canvas; // visible
  let stageEl;
  let stageTick = $state(0);
  let curZf = 1, rs = 1;
  let noteRects = [];

  // The text each pane renders:
  //  - Follow UI on  -> exactly what the app shows (its Pretty/Raw view), cropped to the
  //    region currently scrolled into view.
  //  - Follow UI off -> the raw message, beautified per the Beautify toggles.
  const src = $derived(source ?? {});
  function beautify(raw) { return (raw && prettyBody(raw)) || raw || ''; }
  // `src.request` / `src.response` are RAW; `requestView` / `responseView` are what the UI shows.
  let effReq = $derived(opts.followUi ? (src.requestView ?? src.request ?? '') : opts.beautifyReq ? beautify(src.request) : src.request ?? '');
  let effRes = $derived(opts.followUi ? (src.responseView ?? src.response ?? '') : opts.beautifyRes ? beautify(src.response) : src.response ?? '');
  // Map a pane's visible scroll fraction to a 0-based logical line range of its text.
  function fracToRange(text, frac) {
    if (!frac) return null;
    const n = (text || '').split('\n').length;
    const start = Math.max(0, Math.floor(frac.top * n));
    const end = Math.min(n, Math.ceil(frac.bottom * n));
    return start <= 0 && end >= n ? null : { start, end };
  }
  let reqRange = $derived(opts.followUi ? fracToRange(effReq, src.reqFrac) : null);
  let resRange = $derived(opts.followUi ? fracToRange(effRes, src.resFrac) : null);
  // Whether "follow UI" actually has a scrolled region to honour (for the hint).
  let followActive = $derived(opts.followUi && !!(src.reqFrac || src.resFrac));

  let snapOpts = $derived({ ...opts, reqRange, resRange, legendLines: opts.notesMode === 'legend' ? marks.filter((m) => (m.note || '').trim()).length : 0 });
  let snap = $derived(buildSnapshot({ ...src, request: effReq, response: effRes }, snapOpts, measureCanvas));

  onMount(() => {
    const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(() => stageTick++) : null;
    if (ro && stageEl) ro.observe(stageEl);
    const onResize = () => stageTick++;
    window.addEventListener('resize', onResize);
    return () => { ro?.disconnect(); window.removeEventListener('resize', onResize); };
  });

  function fitFactor() {
    if (!stageEl || !snap) return 1;
    const availW = stageEl.clientWidth - 36, availH = stageEl.clientHeight - 36;
    return Math.max(0.1, Math.min(1, availW / snap.width, availH / snap.height)) || 1;
  }
  const zoomFactor = () => (zoom > 0 ? zoom : fitFactor());

  // Re-render whenever the layout, annotations, shapes, selection or zoom change.
  $effect(() => { stageTick; snap; marks; redacts; shapes; zoom; selMark; selShape; tool; renderAll(); });

  function renderAll() {
    if (!canvas || !snap) return;
    if (!baseCanvas) baseCanvas = document.createElement('canvas');
    const zf = zoomFactor();
    const dpr = (typeof window !== 'undefined' && window.devicePixelRatio) || 1;
    rs = zf * dpr; curZf = zf;
    const W = Math.round(snap.width * rs), H = Math.round(snap.height * rs);
    canvas.width = baseCanvas.width = W;
    canvas.height = baseCanvas.height = H;
    canvas.style.width = snap.width * zf + 'px';
    canvas.style.height = snap.height * zf + 'px';
    const bctx = baseCanvas.getContext && baseCanvas.getContext('2d');
    if (bctx) {
      const drawMarks = marks.map((m, i) => ({ ...m, n: i + 1 }));
      const res = drawSnapshot(bctx, snap, { marks: drawMarks, redacts, sel: selMark }, rs);
      noteRects = res?.noteRects || [];
      // The shape being dragged is drawn live in paint(), so leave it out of the base.
      drawShapes(bctx, hideShape >= 0 ? shapes.filter((_, i) => i !== hideShape) : shapes);
    }
    paint();
  }
  function paint() {
    const ctx = canvas?.getContext && canvas.getContext('2d');
    if (!ctx || !baseCanvas) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    if (baseCanvas.width) ctx.drawImage(baseCanvas, 0, 0);
    ctx.setTransform(rs, 0, 0, rs, 0, 0);
    if (drawing) drawShape(ctx, drawing);
    if (liveShape) drawShape(ctx, liveShape);
    if (tool === 'select' && selShape >= 0) drawSelection(ctx, liveShape ?? shapes[selShape]);
  }
  function drawSelection(ctx, s) {
    if (!s) return;
    const b = shapeBBox(s);
    ctx.save();
    ctx.strokeStyle = '#4f9dff'; ctx.lineWidth = 1; ctx.setLineDash([4, 3]);
    ctx.strokeRect(b.x - 3, b.y - 3, b.w + 6, b.h + 6);
    ctx.setLineDash([]);
    for (const h of handlesFor(s)) { ctx.fillStyle = '#fff'; ctx.strokeStyle = '#4f9dff'; ctx.lineWidth = 1.5; ctx.fillRect(h.x - 4, h.y - 4, 8, 8); ctx.strokeRect(h.x - 4, h.y - 4, 8, 8); }
    ctx.restore();
  }

  // ---- undo / redo ----
  const clone = (x) => JSON.parse(JSON.stringify(x));
  let past = $state([]), future = $state([]);
  const snapNow = () => ({ marks: clone(marks), redacts: clone(redacts), shapes: clone(shapes) });
  function beginChange() { past.push(snapNow()); if (past.length > 100) past.shift(); future = []; }
  function apply(s) { marks = s.marks; redacts = s.redacts; shapes = s.shapes; textPop = null; selMark = Math.min(selMark, marks.length - 1); selShape = -1; }
  function undo() { if (!past.length) return; future.push(snapNow()); apply(past.pop()); }
  function redoAct() { if (!future.length) return; past.push(snapNow()); apply(future.pop()); }
  function clearAll() { if (!marks.length && !redacts.length && !shapes.length) return; beginChange(); marks = []; redacts = []; shapes = []; selMark = -1; selShape = -1; }

  // ---- pointer -> logical snapshot coords ----
  function at(e) { const r = canvas.getBoundingClientRect(); return { x: (e.clientX - r.left) / curZf, y: (e.clientY - r.top) / curZf }; }
  const freehand = (t) => ['rect', 'ellipse', 'arrow', 'line', 'pen', 'highlight'].includes(t);
  function setTool(t) { tool = t; if (t !== 'select') selShape = -1; }

  // ---- shape geometry (Select tool): bbox, resize handles, hit-testing ----
  function textShapeWidth(s) {
    const ctx = measureCanvas && measureCanvas.getContext && measureCanvas.getContext('2d');
    const size = 13 + (s.width || 3) * 2;
    if (!ctx) return (s.text || '').length * size * 0.6;
    ctx.font = `bold ${size}px ui-sans-serif, system-ui, sans-serif`;
    return ctx.measureText(s.text || '').width;
  }
  function shapeBBox(s) {
    if (s.type === 'text') { const size = 13 + (s.width || 3) * 2; return { x: s.x - 4, y: s.y - 3, w: textShapeWidth(s) + 8, h: size + 6 }; }
    if (s.points) {
      let a = Infinity, b = Infinity, c = -Infinity, d = -Infinity;
      for (const p of s.points) { a = Math.min(a, p.x); b = Math.min(b, p.y); c = Math.max(c, p.x); d = Math.max(d, p.y); }
      return { x: a, y: b, w: c - a, h: d - b };
    }
    return { x: Math.min(s.x, s.x2), y: Math.min(s.y, s.y2), w: Math.abs(s.x2 - s.x), h: Math.abs(s.y2 - s.y) };
  }
  function handlesFor(s) {
    if (s.type === 'rect' || s.type === 'ellipse') return [{ id: 'x1y1', x: s.x, y: s.y }, { id: 'x2y1', x: s.x2, y: s.y }, { id: 'x1y2', x: s.x, y: s.y2 }, { id: 'x2y2', x: s.x2, y: s.y2 }];
    if (s.type === 'line' || s.type === 'arrow') return [{ id: 'x1y1', x: s.x, y: s.y }, { id: 'x2y2', x: s.x2, y: s.y2 }];
    return []; // pen / highlight / text: move only
  }
  function distSeg(px, py, ax, ay, bx, by) {
    const dx = bx - ax, dy = by - ay, l2 = dx * dx + dy * dy;
    if (!l2) return Math.hypot(px - ax, py - ay);
    let t = ((px - ax) * dx + (py - ay) * dy) / l2; t = Math.max(0, Math.min(1, t));
    return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
  }
  function hitShape(p) {
    const tol = 7;
    for (let i = shapes.length - 1; i >= 0; i--) {
      const s = shapes[i];
      if (s.type === 'text' || s.type === 'rect' || s.type === 'ellipse') {
        const b = shapeBBox(s);
        if (p.x >= b.x - tol && p.x <= b.x + b.w + tol && p.y >= b.y - tol && p.y <= b.y + b.h + tol) return i;
      } else if (s.type === 'line' || s.type === 'arrow') {
        if (distSeg(p.x, p.y, s.x, s.y, s.x2, s.y2) <= tol + (s.width || 3) / 2) return i;
      } else if (s.points) {
        for (let k = 1; k < s.points.length; k++) if (distSeg(p.x, p.y, s.points[k - 1].x, s.points[k - 1].y, s.points[k].x, s.points[k].y) <= tol + (s.width || 3)) return i;
      }
    }
    return -1;
  }
  function hitTextShape(p) { for (let i = shapes.length - 1; i >= 0; i--) { const s = shapes[i]; if (s.type !== 'text') continue; const b = shapeBBox(s); if (p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h) return i; } return -1; }
  function hitHandle(p, s) { for (const h of handlesFor(s)) if (Math.hypot(p.x - h.x, p.y - h.y) <= 8) return h.id; return null; }
  function moveShapeInto(dst, orig, dx, dy) {
    if (orig.points) dst.points = orig.points.map((pt) => ({ x: pt.x + dx, y: pt.y + dy }));
    else { dst.x = orig.x + dx; dst.y = orig.y + dy; if (orig.x2 != null) { dst.x2 = orig.x2 + dx; dst.y2 = orig.y2 + dy; } }
  }
  function resizeShapeInto(dst, handle, p) {
    if (handle === 'x1y1') { dst.x = p.x; dst.y = p.y; }
    else if (handle === 'x2y2') { dst.x2 = p.x; dst.y2 = p.y; }
    else if (handle === 'x2y1') { dst.x2 = p.x; dst.y = p.y; }
    else if (handle === 'x1y2') { dst.x = p.x; dst.y2 = p.y; }
  }
  function deleteShape(i) { if (i < 0) return; beginChange(); shapes = shapes.filter((_, j) => j !== i); selShape = -1; }

  function down(e) {
    const p = at(e);
    // Drag a callout note (any tool).
    for (const nr of noteRects) {
      if (p.x >= nr.x && p.x <= nr.x + nr.w && p.y >= nr.y && p.y <= nr.y + nr.h) {
        noteDrag = { i: nr.i, sx: p.x, sy: p.y, dx0: marks[nr.i].dx || 0, dy0: marks[nr.i].dy || 0, moved: false, began: false };
        canvas.setPointerCapture(e.pointerId);
        return;
      }
    }
    if (tool === 'select') {
      // Resize the selected shape by a handle, else pick (and start moving) a shape.
      if (selShape >= 0 && shapes[selShape]) {
        const h = hitHandle(p, shapes[selShape]);
        if (h) { shapeDrag = { i: selShape, mode: 'resize', handle: h, orig: clone(shapes[selShape]), began: false }; hideShape = selShape; liveShape = clone(shapes[selShape]); canvas.setPointerCapture(e.pointerId); renderAll(); return; }
      }
      const i = hitShape(p);
      selShape = i;
      if (i >= 0) { shapeDrag = { i, mode: 'move', sx: p.x, sy: p.y, orig: clone(shapes[i]), began: false }; hideShape = i; liveShape = clone(shapes[i]); canvas.setPointerCapture(e.pointerId); renderAll(); }
      return;
    }
    if (freehand(tool)) {
      drawing = tool === 'pen' || tool === 'highlight'
        ? { type: tool, color: opts.markColor, width, points: [p] }
        : { type: tool, color: opts.markColor, width, x: p.x, y: p.y, x2: p.x, y2: p.y };
      canvas.setPointerCapture(e.pointerId);
      return;
    }
    if (tool === 'text') {
      const cr = canvas.getBoundingClientRect();
      // Clamp the floating input into view so it never lands off-screen / behind the bar.
      const sx = Math.min(Math.max(12, cr.left + p.x * curZf), window.innerWidth - 272);
      const sy = Math.min(Math.max(96, cr.top + p.y * curZf), window.innerHeight - 90);
      textPop = { x: p.x, y: p.y, sx, sy, value: '' };
      // Focus AFTER this click settles (rAF), so the pointerup can't blur it away.
      requestAnimationFrame(() => document.getElementById('free-text-input')?.focus());
      return;
    }
    const hit = hitTest(snap, p.x, p.y);
    if (!hit) { selMark = -1; return; }
    if (tool === 'mark') {
      // If a mark already boxes this line, just select+edit it instead of stacking.
      const existing = marks.findIndex((m) => m.col === hit.col && m.line === hit.line);
      if (existing >= 0) { selectMark(existing); return; }
      beginChange();
      marks = [...marks, { col: hit.col, line: hit.line, color: opts.markColor, note: '', dx: 0, dy: 0 }];
      selectMark(marks.length - 1);
    } else if (tool === 'redact') {
      const w = wordAt(snap, hit.col, hit.line, hit.char);
      if (!w) return;
      beginChange();
      const ex = redacts.findIndex((r) => r.col === hit.col && r.line === hit.line && !(w.c2 <= r.c1 || w.c1 >= r.c2));
      redacts = ex >= 0 ? redacts.filter((_, i) => i !== ex) : [...redacts, { col: hit.col, line: hit.line, c1: w.c1, c2: w.c2 }];
    }
  }
  function move(e) {
    const p = at(e);
    if (noteDrag) {
      if (!noteDrag.began) { beginChange(); noteDrag.began = true; }
      noteDrag.moved = true;
      const mk = marks[noteDrag.i]; if (!mk) return;
      mk.dx = noteDrag.dx0 + (p.x - noteDrag.sx); mk.dy = noteDrag.dy0 + (p.y - noteDrag.sy);
      marks = [...marks];
      return;
    }
    if (shapeDrag) {
      if (!shapeDrag.began) { beginChange(); shapeDrag.began = true; }
      const ns = clone(shapeDrag.orig);
      if (shapeDrag.mode === 'move') moveShapeInto(ns, shapeDrag.orig, p.x - shapeDrag.sx, p.y - shapeDrag.sy);
      else resizeShapeInto(ns, shapeDrag.handle, p);
      liveShape = ns;
      paint();
      return;
    }
    if (drawing) { if (drawing.points) drawing.points.push(p); else { drawing.x2 = p.x; drawing.y2 = p.y; } paint(); }
  }
  function up() {
    if (noteDrag) { const nd = noteDrag; noteDrag = null; if (!nd.moved) selectMark(nd.i); return; }
    if (shapeDrag) {
      const sd = shapeDrag; shapeDrag = null;
      if (sd.began && liveShape) shapes[sd.i] = liveShape;
      liveShape = null; hideShape = -1;
      if (sd.began) shapes = [...shapes]; else renderAll();
      return;
    }
    if (drawing) {
      const d = drawing; drawing = null;
      const big = d.points ? d.points.length > 1 : Math.abs(d.x2 - d.x) + Math.abs(d.y2 - d.y) > 4;
      if (big) { beginChange(); shapes = [...shapes, d]; } else paint();
    }
  }
  function onDblClick(e) {
    const p = at(e);
    // Double-click a text annotation to edit it (any tool).
    const ti = hitTextShape(p);
    if (ti >= 0) { editTextShape(ti); return; }
    const hit = hitTest(snap, p.x, p.y);
    if (!hit) return;
    const i = marks.findIndex((m) => m.col === hit.col && m.line === hit.line);
    if (i >= 0) selectMark(i);
  }
  function editTextShape(i) {
    const s = shapes[i]; if (!s) return;
    const cr = canvas.getBoundingClientRect();
    const sx = Math.min(Math.max(12, cr.left + s.x * curZf), window.innerWidth - 272);
    const sy = Math.min(Math.max(96, cr.top + s.y * curZf), window.innerHeight - 90);
    textPop = { editing: i, x: s.x, y: s.y, sx, sy, value: s.text };
    requestAnimationFrame(() => document.getElementById('free-text-input')?.focus());
  }

  // Selecting a mark highlights it on the canvas and focuses its note field in the
  // right-hand "Marks" list - a stable element, so editing is reliable (no off-screen
  // popover, no focus/blur race with the click that created it).
  function selectMark(i) {
    selMark = i;
    requestAnimationFrame(() => {
      const el = document.getElementById('mark-note-' + i);
      if (el) { el.focus(); el.select?.(); el.scrollIntoView({ block: 'nearest' }); }
    });
  }
  function deleteMark(i) { beginChange(); marks = marks.filter((_, j) => j !== i); if (selMark >= marks.length) selMark = marks.length - 1; }
  function commitText() {
    const te = textPop; if (!te) return;
    const v = (te.value || '').trim();
    if (te.editing != null) {
      beginChange();
      if (v) shapes[te.editing].text = v; else shapes = shapes.filter((_, j) => j !== te.editing);
      shapes = [...shapes];
    } else if (v) {
      beginChange();
      shapes = [...shapes, { type: 'text', color: opts.markColor, width, x: te.x, y: te.y, text: v }];
    }
    textPop = null;
  }

  function onWheel(e) {
    if (!(e.metaKey || e.ctrlKey)) return;
    e.preventDefault();
    zoom = Math.max(0.2, Math.min(4, zoomFactor() * (e.deltaY < 0 ? 1.1 : 0.9)));
  }
  function onKey(e) {
    if (textPop) return;
    if (e.key === 'Escape') { e.preventDefault(); if (selShape >= 0) { selShape = -1; return; } onclose(); return; }
    if ((e.key === 'Delete' || e.key === 'Backspace') && tool === 'select' && selShape >= 0) { e.preventDefault(); deleteShape(selShape); return; }
    const mod = e.metaKey || e.ctrlKey;
    if (!mod) return;
    const k = e.key.toLowerCase();
    if (k === 'z' && !e.shiftKey) { e.preventDefault(); undo(); }
    else if (k === 'y' || (k === 'z' && e.shiftKey)) { e.preventDefault(); redoAct(); }
  }

  // ---- export ----
  function exportCanvas(scale) {
    const off = document.createElement('canvas');
    off.width = Math.round(snap.width * scale);
    off.height = Math.round(snap.height * scale);
    const ctx = off.getContext('2d');
    const drawMarks = marks.map((m, i) => ({ ...m, n: i + 1 }));
    drawSnapshot(ctx, snap, { marks: drawMarks, redacts }, scale);
    drawShapes(ctx, shapes);
    return off;
  }

  let toast = $state(null);
  let toastTimer;
  let copyLabel = $state('Copy to clipboard');
  function notify(text, error = false) {
    toast = { text, error };
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = null), 2200);
    store.notice = { text, error };
    setTimeout(() => (store.notice = null), 2500);
  }
  async function savePng() {
    const off = exportCanvas(opts.exportScale);
    const path = await save({ title: 'Save snapshot', defaultPath: `${BRAND.id}-snapshot.png`, filters: [{ name: 'PNG image', extensions: ['png'] }] });
    if (!path) return;
    try { await api.saveBinary(path, off.toDataURL('image/png').split(',')[1]); notify('Snapshot saved ✓'); }
    catch (e) { notify(String(e), true); }
  }
  async function copyImg() {
    const off = exportCanvas(opts.exportScale);
    const ok = () => { notify('Copied to clipboard ✓'); copyLabel = 'Copied ✓'; setTimeout(() => (copyLabel = 'Copy to clipboard'), 1500); };
    try {
      const ctx = off.getContext('2d');
      const { data, width: w, height: h } = ctx.getImageData(0, 0, off.width, off.height);
      let bin = ''; const bytes = new Uint8Array(data.buffer); const chunk = 0x8000;
      for (let i = 0; i < bytes.length; i += chunk) bin += String.fromCharCode(...bytes.subarray(i, i + chunk));
      await api.copyImage(w, h, btoa(bin));
      ok();
    } catch (e) {
      try { const blob = await new Promise((r) => off.toBlob(r, 'image/png')); await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]); ok(); }
      catch { notify('Copy failed: ' + (e?.message ?? e), true); }
    }
  }

  const zoomPct = $derived(Math.round(zoomFactor() * 100));
</script>

<svelte:window onkeydown={onKey} />

<div class="overlay" role="dialog" aria-label="Snapshot editor">
  {#if toast}<div class="cap-toast" class:error={toast.error} role="status" aria-live="polite">{toast.text}</div>{/if}

  <div class="bar">
    <div class="group">
      <button class="tbtn ic" class:active={tool === 'select'} title="Select / move / resize / delete an annotation" aria-label="Select" onclick={() => setTool('select')}>
        <svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="1" stroke-linejoin="round"><path d="M5 3l6.5 15.5 2.2-6 6-2.3z" /></svg>
      </button>
    </div>
    <div class="group" role="tablist">
      <button class="tbtn" class:active={tool === 'mark'} title="Box a line and add a note" onclick={() => setTool('mark')}>▭ Mark</button>
      <button class="tbtn" class:active={tool === 'redact'} title="Hide a value with a click" onclick={() => setTool('redact')}>▧ Redact</button>
    </div>
    <div class="group">
      {#each FREE_TOOLS as ft}
        <button class="tbtn ic" class:active={tool === ft.id} title={ft.label} aria-label={ft.label} onclick={() => setTool(ft.id)}>
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{@html ft.icon}</svg>
        </button>
      {/each}
    </div>
    <div class="group">
      {#each WIDTHS as w}
        <button class="tbtn ic" class:active={width === w.id} title="Stroke width" aria-label="Stroke width" onclick={() => (width = w.id)}>
          <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r={w.r} fill="currentColor" /></svg>
        </button>
      {/each}
    </div>
    <button class="btn ghost sm" title="Undo (Cmd/Ctrl+Z)" onclick={undo} disabled={!past.length}>Undo</button>
    <button class="btn ghost sm" title="Redo (Cmd/Ctrl+Y)" onclick={redoAct} disabled={!future.length}>Redo</button>
    <button class="btn ghost sm" onclick={clearAll} disabled={!marks.length && !redacts.length && !shapes.length}>Clear</button>
    <div class="group zoomg">
      <button class="tbtn" title="Zoom out" onclick={() => (zoom = Math.max(0.2, zoomFactor() * 0.9))}>−</button>
      <button class="tbtn" title="Fit" class:active={zoom === 0} onclick={() => (zoom = 0)}>{zoomPct}%</button>
      <button class="tbtn" title="Zoom in" onclick={() => (zoom = Math.min(4, zoomFactor() * 1.1))}>+</button>
    </div>
    <div class="spacer"></div>
    <span class="hint-dim">Cmd/Ctrl + scroll to zoom</span>
  </div>

  <div class="body">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="stage" bind:this={stageEl} onwheel={onWheel}>
      <canvas bind:this={canvas} class="canvas" class:cross={tool !== 'text' && tool !== 'select'} class:textc={tool === 'text'} class:selc={tool === 'select'}
        onpointerdown={down} onpointermove={move} onpointerup={up} ondblclick={onDblClick}></canvas>
    </div>

    <aside class="panel">
      <h3>Snapshot</h3>
      <div class="opt"><span>Theme</span><div class="seg">
        <button class:on={opts.theme === 'light'} onclick={() => (opts.theme = 'light')}>Light</button>
        <button class:on={opts.theme === 'dark'} onclick={() => (opts.theme = 'dark')}>Dark</button>
      </div></div>
      <div class="opt"><span>Layout</span><div class="seg">
        <button class:on={opts.layout === 'side'} onclick={() => (opts.layout = 'side')}>Side by side</button>
        <button class:on={opts.layout === 'stacked'} onclick={() => (opts.layout = 'stacked')}>Stacked</button>
      </div></div>
      <div class="opt"><span>Color</span><div class="swatches">
        {#each MARK_COLORS as c}<button class="sw" class:on={opts.markColor === c} style="background:{c}" aria-label="colour" onclick={() => (opts.markColor = c)}></button>{/each}
      </div></div>
      <div class="opt"><span>Notes</span><div class="seg">
        <button class:on={opts.notesMode === 'callout'} onclick={() => (opts.notesMode = 'callout')}>Callout</button>
        <button class:on={opts.notesMode === 'legend'} onclick={() => (opts.notesMode = 'legend')}>Legend</button>
      </div></div>

      <div class="marks-sec">
        <div class="marks-head">Marks{marks.length ? ` (${marks.length})` : ''}</div>
        {#if marks.length}
          {#each marks as m, i (i)}
            <div class="markrow" class:sel={selMark === i}>
              <button class="mbadge" style="background:{m.color}" title="Highlight on the snapshot" onclick={() => selectMark(i)}>{i + 1}</button>
              <input id="mark-note-{i}" class="minput" placeholder="Describe mark {i + 1}…" bind:value={m.note}
                onfocus={() => (selMark = i)}
                onkeydown={(e) => { e.stopPropagation(); if (e.key === 'Enter') e.currentTarget.blur(); }} />
              <button class="mdel" title="Delete mark" onclick={() => deleteMark(i)} aria-label="Delete mark {i + 1}">×</button>
            </div>
          {/each}
        {:else}
          <p class="marks-empty">Pick <b>Mark</b>, click a line in the snapshot, then type its note here.</p>
        {/if}
      </div>

      <div class="opt"><span>Redaction</span><div class="seg">
        <button class:on={opts.redactStyle === 'blur'} onclick={() => (opts.redactStyle = 'blur')}>Blur</button>
        <button class:on={opts.redactStyle === 'solid'} onclick={() => (opts.redactStyle = 'solid')}>Solid</button>
        <button class:on={opts.redactStyle === 'dots'} onclick={() => (opts.redactStyle = 'dots')}>Dots</button>
      </div></div>
      <label class="check" title="Pretty-print the request body (JSON / XML / HTML)"><input type="checkbox" bind:checked={opts.beautifyReq} disabled={opts.followUi} /> Beautify request</label>
      <label class="check" title="Pretty-print the response body (JSON / XML / HTML)"><input type="checkbox" bind:checked={opts.beautifyRes} disabled={opts.followUi} /> Beautify response</label>
      <label class="check" title="Capture only the part of the request/response currently scrolled into view in the app"><input type="checkbox" bind:checked={opts.followUi} /> Follow page scroll</label>
      {#if opts.followUi}
        <p class="follow-hint">{followActive ? 'Capturing exactly what is scrolled into view in the app (its Pretty/Raw view).' : 'Nothing is scrolled in the app, so the full view is captured.'}</p>
      {/if}
      <label class="check"><input type="checkbox" bind:checked={opts.autoRedact} /> Auto-redact secrets</label>
      <label class="check" title="Trim very long bodies, keeping the start, end and a count of what was cut"><input type="checkbox" bind:checked={opts.autoTrim} disabled={opts.followUi} /> Auto-trim long bodies</label>
      <label class="check"><input type="checkbox" bind:checked={opts.showTime} /> Show response time</label>
      <div class="opt col"><span>Watermark</span>
        <input class="inp" placeholder="e.g. ACME PENTEST · {'{date}'}" bind:value={opts.watermark} spellcheck="false" />
        <span class="hint-dim">{'{host}'} and {'{date}'} are filled in.</span>
      </div>
      <div class="opt"><span>Export scale</span><div class="seg">
        {#each EXPORT_SCALES as s}<button class:on={opts.exportScale === s} onclick={() => (opts.exportScale = s)}>{s}x</button>{/each}
      </div></div>
    </aside>
  </div>

  <div class="foot">
    <input class="title-inp" placeholder="Name the finding (becomes the heading)…" bind:value={opts.title} spellcheck="false" />
    <div class="spacer"></div>
    <button class="btn sm" onclick={copyImg}>{copyLabel}</button>
    <button class="btn primary sm" onclick={savePng}>Save PNG…</button>
    <button class="btn ghost sm" onclick={onclose}>Close</button>
  </div>

  {#if textPop}
    <input id="free-text-input" class="note-inp free-text" style="left:{textPop.sx}px; top:{textPop.sy}px" placeholder="Type, then Enter…"
      bind:value={textPop.value}
      onkeydown={(e) => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); commitText(); } else if (e.key === 'Escape') { e.preventDefault(); textPop = null; } }}
      onblur={commitText} />
  {/if}
</div>

<style>
  .overlay { position: fixed; inset: 0; z-index: 200; display: grid; grid-template-rows: auto 1fr auto; background: color-mix(in srgb, var(--bg) 92%, black); }
  .cap-toast { position: absolute; top: 56px; left: 50%; z-index: 10; transform: translateX(-50%); padding: 9px 18px; border: 1px solid var(--border); border-radius: 99px; background: var(--accent); color: #fff; font: 600 12.5px var(--sans); box-shadow: 0 6px 20px rgb(0 0 0 / 0.4); pointer-events: none; }
  .cap-toast.error { background: var(--red); }
  .bar { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--border); background: var(--bg-elev); }
  .group { display: flex; gap: 2px; padding: 2px; border-radius: 7px; background: var(--bg-soft); }
  .tbtn { height: 26px; padding: 0 10px; border: 0; border-radius: 5px; background: none; color: var(--text-dim); font: 600 12px var(--sans); cursor: pointer; }
  .tbtn.active { background: var(--bg-elev); color: var(--text); box-shadow: 0 1px 2px rgb(0 0 0 / 0.25); }
  .tbtn:hover:not(.active) { color: var(--text); }
  .tbtn.ic { display: grid; place-items: center; width: 30px; padding: 0; }
  .tbtn.ic svg { width: 17px; height: 17px; display: block; }
  .zoomg .tbtn { min-width: 30px; }
  .hint-dim { color: var(--text-faint); font-size: 11.5px; }
  .spacer { flex: 1; }

  .body { display: grid; grid-template-columns: 1fr 272px; min-height: 0; }
  .stage { min-height: 0; overflow: auto; display: grid; place-items: center; padding: 18px; }
  .canvas { border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.45); }
  .canvas.cross { cursor: crosshair; }
  .canvas.textc { cursor: text; }
  .canvas.selc { cursor: default; }

  .panel { overflow: auto; border-left: 1px solid var(--border); background: var(--bg-elev); padding: 14px 14px 40px; }
  .panel h3 { margin: 0 0 12px; font-size: 12px; font-weight: 700; letter-spacing: 0.05em; text-transform: uppercase; color: var(--text-dim); }
  .opt { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 12px; }
  .opt > span { color: var(--text-dim); font-size: 12px; flex: 0 0 auto; }
  .opt.col { flex-direction: column; align-items: stretch; }
  .opt.col > span:first-child { margin-bottom: 5px; }
  .seg { display: inline-flex; padding: 2px; border-radius: 7px; background: var(--bg-soft); flex-wrap: wrap; justify-content: flex-end; }
  .seg button { padding: 4px 9px; border: 0; border-radius: 5px; background: none; color: var(--text-dim); font: 600 11.5px var(--sans); cursor: pointer; }
  .seg button.on { background: var(--bg-elev); color: var(--text); box-shadow: 0 1px 2px rgb(0 0 0 / 0.25); }
  .swatches { display: flex; gap: 5px; }
  .sw { width: 18px; height: 18px; padding: 0; border: 2px solid transparent; border-radius: 50%; cursor: pointer; }
  .sw.on { border-color: var(--text); }
  .check { display: flex; align-items: center; gap: 7px; margin-bottom: 11px; color: var(--text); font-size: 12.5px; cursor: pointer; }
  .check input { accent-color: var(--accent); }
  .check input:disabled { opacity: 0.4; }
  .follow-hint { margin: -6px 0 11px 24px; color: var(--text-faint); font-size: 11px; line-height: 1.45; }
  .inp { width: 100%; height: 28px; padding: 0 8px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font: 12px var(--mono); outline: none; }
  .inp:focus { border-color: var(--accent); }

  .foot { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-top: 1px solid var(--border); background: var(--bg-elev); }
  .title-inp { flex: 1; max-width: 520px; height: 30px; padding: 0 10px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg); color: var(--text); outline: none; font: 13px var(--sans); }
  .title-inp:focus { border-color: var(--accent); }

  /* Freehand Text tool's floating input (placed at the click point, clamped on-screen). */
  .note-inp { position: fixed; z-index: 220; width: 240px; height: 28px; padding: 0 9px; border: 1px solid var(--accent); border-radius: 6px; background: var(--bg-elev); color: var(--text); font: 12.5px var(--sans); outline: none; box-shadow: 0 6px 20px rgb(0 0 0 / 0.4); }

  /* Marks list: the reliable place to write + manage each mark's note. */
  .marks-sec { margin: 4px 0 14px; padding: 10px; border: 1px solid var(--border); border-radius: 9px; background: var(--bg-soft); }
  .marks-head { font: 700 10.5px var(--sans); letter-spacing: 0.05em; text-transform: uppercase; color: var(--text-dim); margin-bottom: 8px; }
  .marks-empty { margin: 0; color: var(--text-faint); font-size: 11.5px; line-height: 1.5; }
  .markrow { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; padding: 3px; border-radius: 7px; }
  .markrow:last-child { margin-bottom: 0; }
  .markrow.sel { background: var(--accent-soft); box-shadow: inset 0 0 0 1px var(--accent); }
  .mbadge { flex: 0 0 auto; width: 20px; height: 20px; border: 0; border-radius: 50%; color: #fff; font: 700 11px var(--sans); cursor: pointer; }
  .minput { flex: 1; min-width: 0; height: 28px; padding: 0 8px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font: 12px var(--sans); outline: none; }
  .minput:focus { border-color: var(--accent); }
  .mdel { flex: 0 0 auto; width: 24px; height: 24px; border: 1px solid transparent; border-radius: 6px; background: none; color: var(--text-faint); font-size: 15px; cursor: pointer; }
  .mdel:hover { border-color: var(--red); color: var(--red); }
</style>
