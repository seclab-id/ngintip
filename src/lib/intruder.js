// Intruder payload generation. Positions in the base request are marked by
// wrapping text in section signs, for example: id=<<0>> ... token=<<abc>>,
// where MARK is the section sign. Pure functions so they can be unit-tested.

export const MARK = '§'; // section sign

/** How many requests each attack type sends, given position/payload counts. */
export const ATTACK_TYPES = [
  { id: 'sniper', label: 'Sniper', hint: 'One payload set; one position at a time.' },
  { id: 'ram', label: 'Battering ram', hint: 'One payload set; same payload in every position.' },
  { id: 'pitchfork', label: 'Pitchfork', hint: 'One set per position; advance in parallel.' },
  { id: 'cluster', label: 'Cluster bomb', hint: 'One set per position; every combination.' },
];

/** Hard cap on generated requests, so a cluster bomb cannot lock up the UI. */
export const MAX_REQUESTS = 100000;

// ---- Payload types (Burp-style payload generators) --------------------------
// A payload set is either a raw wordlist string (a "simple list", back-compat) or
// a config object { type, list, numbers, brute, nul, process }. resolvePayloads()
// expands a config to the actual list of payload strings.

export const PAYLOAD_TYPES = [
  { id: 'simple', label: 'Simple list', hint: 'One payload per line.' },
  { id: 'numbers', label: 'Numbers', hint: 'A range from/to with a step (start position, how many).' },
  { id: 'brute', label: 'Brute forcer', hint: 'Every combination of a character set, min..max length.' },
  { id: 'null', label: 'Null payloads', hint: 'N requests with an empty payload (e.g. rate-limit / retry tests).' },
];

/** A fresh payload-set config with sensible defaults for every type. */
export function defaultPayloadConfig() {
  return {
    type: 'simple',
    list: '',
    numbers: { from: 1, to: 100, step: 1, hex: false, pad: 0 },
    brute: { chars: 'abcdefghijklmnopqrstuvwxyz', min: 1, max: 3 },
    nul: { count: 10 },
    // Processing applied to every generated payload, in this order:
    // case -> encode -> prefix -> suffix -> null byte.
    process: { prefix: '', suffix: '', case: 'none', encode: 'none', nullByte: false },
  };
}

const toInt = (v, d = 0) => {
  const n = Math.trunc(Number(v));
  return Number.isFinite(n) ? n : d;
};

function expandNumbers(n = {}) {
  const from = toInt(n.from, 0);
  const to = toInt(n.to, 0);
  const step = Math.abs(toInt(n.step, 1)) || 1;
  const pad = Math.max(0, toInt(n.pad, 0));
  const hex = !!n.hex;
  const dir = to >= from ? 1 : -1;
  const list = [];
  let truncated = false;
  for (let v = from; dir > 0 ? v <= to : v >= to; v += dir * step) {
    if (list.length >= MAX_REQUESTS) {
      truncated = true;
      break;
    }
    let s = hex ? (v < 0 ? '-' + Math.abs(v).toString(16) : v.toString(16)) : String(v);
    if (pad > 0) {
      const neg = s.startsWith('-');
      const body = neg ? s.slice(1) : s;
      s = (neg ? '-' : '') + body.padStart(pad, '0');
    }
    list.push(s);
  }
  return { list, truncated };
}

function expandBrute(b = {}) {
  const chars = [...(b.chars ?? '')];
  const min = Math.max(1, toInt(b.min, 1));
  const max = Math.max(min, toInt(b.max, min));
  if (!chars.length) return { list: [], truncated: false, error: 'Brute forcer needs at least one character.' };
  const list = [];
  let truncated = false;
  const build = (prefix, len) => {
    if (truncated) return;
    if (len === 0) {
      if (list.length >= MAX_REQUESTS) {
        truncated = true;
        return;
      }
      list.push(prefix);
      return;
    }
    for (const c of chars) {
      if (truncated) return;
      build(prefix + c, len - 1);
    }
  };
  for (let len = min; len <= max && !truncated; len++) build('', len);
  return { list, truncated };
}

function toBase64(s) {
  const bytes = new TextEncoder().encode(s);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

function urlEncodeAll(s) {
  const bytes = new TextEncoder().encode(s);
  let out = '';
  for (const b of bytes) out += '%' + b.toString(16).toUpperCase().padStart(2, '0');
  return out;
}

/** Apply payload processing rules to one payload string. */
export function processPayload(s, p = {}) {
  let out = s ?? '';
  if (p.case === 'lower') out = out.toLowerCase();
  else if (p.case === 'upper') out = out.toUpperCase();
  if (p.encode === 'url') out = encodeURIComponent(out);
  else if (p.encode === 'urlall') out = urlEncodeAll(out);
  else if (p.encode === 'base64') out = toBase64(out);
  if (p.prefix) out = p.prefix + out;
  if (p.suffix) out = out + p.suffix;
  if (p.nullByte) out = out + '\u0000';
  return out;
}

const hasProcessing = (p) =>
  !!p && (p.prefix || p.suffix || (p.case && p.case !== 'none') || (p.encode && p.encode !== 'none') || p.nullByte);

/** Expand a payload set (string or config) to its list of payload strings. */
export function resolvePayloads(cfg) {
  if (cfg == null) return { list: [], count: 0, truncated: false };
  if (typeof cfg === 'string') {
    const list = linesOf(cfg);
    return { list, count: list.length, truncated: false };
  }
  let base;
  if (cfg.type === 'numbers') base = expandNumbers(cfg.numbers);
  else if (cfg.type === 'brute') base = expandBrute(cfg.brute);
  else if (cfg.type === 'null') {
    const n = Math.max(0, toInt(cfg.nul?.count, 0));
    base = { list: Array(Math.min(n, MAX_REQUESTS)).fill(''), truncated: n > MAX_REQUESTS };
  } else base = { list: linesOf(cfg.list) };
  if (base.error) return { list: [], count: 0, truncated: false, error: base.error };
  const list = hasProcessing(cfg.process) ? base.list.map((s) => processPayload(s, cfg.process)) : base.list;
  return { list, count: list.length, truncated: !!base.truncated };
}

/** Payload count for a set without building the whole list (for the UI preview). */
export function countPayloads(cfg) {
  if (cfg == null) return 0;
  if (typeof cfg === 'string') return linesOf(cfg).length;
  if (cfg.type === 'numbers') {
    const from = toInt(cfg.numbers?.from, 0);
    const to = toInt(cfg.numbers?.to, 0);
    const step = Math.abs(toInt(cfg.numbers?.step, 1)) || 1;
    return Math.floor(Math.abs(to - from) / step) + 1;
  }
  if (cfg.type === 'brute') {
    const base = [...(cfg.brute?.chars ?? '')].length;
    if (!base) return 0;
    const min = Math.max(1, toInt(cfg.brute?.min, 1));
    const max = Math.max(min, toInt(cfg.brute?.max, min));
    let total = 0;
    for (let len = min; len <= max; len++) total += Math.pow(base, len);
    return total;
  }
  if (cfg.type === 'null') return Math.max(0, toInt(cfg.nul?.count, 0));
  return linesOf(cfg.list).length;
}

/** Split a base request into literal parts and payload positions (with defaults). */
export function parseTemplate(raw) {
  const parts = [];
  const positions = [];
  let i = 0;
  while (i < raw.length) {
    const s = raw.indexOf(MARK, i);
    if (s === -1) {
      parts.push({ lit: raw.slice(i) });
      break;
    }
    if (s > i) parts.push({ lit: raw.slice(i, s) });
    const e = raw.indexOf(MARK, s + 1);
    if (e === -1) {
      // Unmatched marker: keep the rest literal.
      parts.push({ lit: raw.slice(s) });
      break;
    }
    parts.push({ pos: positions.length });
    positions.push(raw.slice(s + 1, e));
    i = e + 1;
  }
  return { parts, positions };
}

/** The base request with the markers removed (payload positions left as their defaults). */
export function stripMarkers(raw) {
  const { parts, positions } = parseTemplate(raw);
  return parts.map((p) => (p.lit !== undefined ? p.lit : positions[p.pos])).join('');
}

/**
 * Wrap the text in [start, end) as a payload position (§…§). With no selection
 * (start == end) a `§payload§` placeholder is inserted at the caret. Returns the
 * new raw plus the caret offset just after the inserted closing marker.
 */
export function wrapPosition(raw, start, end) {
  const len = raw.length;
  const s = Math.max(0, Math.min(start ?? len, len));
  const e = Math.max(s, Math.min(end ?? s, len));
  if (e > s) {
    const next = raw.slice(0, s) + MARK + raw.slice(s, e) + MARK + raw.slice(e);
    return { raw: next, caret: e + 2 };
  }
  const placeholder = `${MARK}payload${MARK}`;
  return { raw: raw.slice(0, s) + placeholder + raw.slice(s), caret: s + placeholder.length };
}

/** Remove every payload marker, leaving the request text as-is. */
export function clearPositions(raw) {
  return (raw ?? '').split(MARK).join('');
}

function render(parts, values) {
  return parts.map((p) => (p.lit !== undefined ? p.lit : values[p.pos])).join('');
}

const linesOf = (text) => (text ?? '').split('\n').filter((l) => l.length > 0);

/**
 * Build the request list for an attack.
 * `payloadSets` is an array of raw wordlist strings (one per textarea).
 * Returns { requests: [{ payloads, request }], count, total, truncated }.
 */
export function generate(raw, type, payloadSets) {
  const { parts, positions } = parseTemplate(raw);
  const k = positions.length;
  if (k === 0) return { requests: [], count: 0, total: 0, truncated: false, error: 'No payload positions. Wrap text in section signs to mark them.' };

  const resolved = payloadSets.map(resolvePayloads);
  const badSet = resolved.findIndex((r) => r.error);
  if (badSet !== -1) return { requests: [], count: 0, total: 0, truncated: false, error: resolved[badSet].error };
  const sets = resolved.map((r) => r.list);
  const out = [];
  let total = 0;
  let truncated = resolved.some((r) => r.truncated);
  const push = (values, payloads) => {
    if (out.length >= MAX_REQUESTS) {
      truncated = true;
      return false;
    }
    out.push({ payloads, request: render(parts, values) });
    return true;
  };

  if (type === 'sniper') {
    const set = sets[0] ?? [];
    total = k * set.length;
    outer: for (let pi = 0; pi < k; pi++) {
      for (const v of set) {
        const values = positions.slice();
        values[pi] = v;
        if (!push(values, [v])) break outer;
      }
    }
  } else if (type === 'ram') {
    const set = sets[0] ?? [];
    total = set.length;
    for (const v of set) {
      if (!push(positions.map(() => v), [v])) break;
    }
  } else if (type === 'pitchfork') {
    const perPos = positions.map((_, i) => sets[i] ?? []);
    const len = Math.min(...perPos.map((s) => s.length));
    total = Number.isFinite(len) ? len : 0;
    for (let i = 0; i < total; i++) {
      const payloads = perPos.map((s) => s[i]);
      if (!push(payloads.slice(), payloads)) break;
    }
  } else if (type === 'cluster') {
    const perPos = positions.map((_, i) => (sets[i]?.length ? sets[i] : ['']));
    total = perPos.reduce((n, s) => n * s.length, 1);
    const idx = new Array(k).fill(0);
    const done = perPos.some((s) => s.length === 0);
    while (!done) {
      const payloads = perPos.map((s, p) => s[idx[p]]);
      if (!push(payloads.slice(), payloads)) break;
      // Odometer increment across positions.
      let p = k - 1;
      while (p >= 0) {
        idx[p]++;
        if (idx[p] < perPos[p].length) break;
        idx[p] = 0;
        p--;
      }
      if (p < 0) break;
    }
  } else {
    return { requests: [], count: 0, total: 0, truncated: false, error: `Unknown attack type ${type}` };
  }

  return { requests: out, count: out.length, total, truncated };
}
