// Passive analysis of captured traffic (Burp-style): scan request/response text
// for secrets, tokens and interesting response headers. Pure and testable — no
// requests are made, it only reads what was already captured.
//
// The regex detectors live in passive-rules.js (BUILTIN_RULES); custom rules
// added in the UI use the same shape and are merged here at scan time.

import { BUILTIN_RULES } from './passive-rules.js';

export const SEVERITY_ORDER = { high: 0, medium: 1, low: 2, info: 3 };
export const SEVERITIES = ['high', 'medium', 'low', 'info'];

const MAX_PER_RULE = 20;

// Which resources passive scan looks at, by file extension / content-type. The
// default focuses on static & source assets and text/JSON APIs — where secrets,
// tokens and emails actually live — so images, fonts, video and binaries don't
// add noise. Add more extensions in the UI, or use '*' to scan everything.
export const DEFAULT_PASSIVE_SCOPE = [
  'js', 'mjs', 'cjs', 'jsx', 'ts', 'tsx', 'json', 'map', 'html', 'htm',
  'xml', 'css', 'scss', 'txt', 'vue', 'svelte', 'yaml', 'yml', 'env',
  'config', 'ini', 'toml',
];

// Content-type essence (as the backend's short mime stores it) → canonical ext.
const MIME_EXT = {
  javascript: 'js', ecmascript: 'js', typescript: 'ts', json: 'json',
  html: 'html', htm: 'html', xml: 'xml', css: 'css', plain: 'txt',
  'svg+xml': 'svg', svg: 'svg', yaml: 'yaml',
};

function mimeToExt(mime) {
  let m = String(mime || '').toLowerCase().split(';')[0].trim();
  const slash = m.lastIndexOf('/');
  if (slash >= 0) m = m.slice(slash + 1);
  m = m.replace(/^x-/, '');
  return m ? MIME_EXT[m] || m : '';
}

/** Extension/type tokens describing a flow (its path extension + content-type). */
export function flowTokens(flow = {}) {
  const out = new Set();
  const path = String(flow.path || '').split(/[?#]/)[0];
  const seg = path.slice(path.lastIndexOf('/') + 1);
  const dot = seg.lastIndexOf('.');
  if (dot > 0 && dot < seg.length - 1) out.add(seg.slice(dot + 1).toLowerCase());
  const mext = mimeToExt(flow.mime);
  if (mext) out.add(mext);
  return out;
}

/** Whether a flow is within the passive-scan scope. Empty scope or '*' = all. */
export function flowInScope(flow, scope) {
  if (!scope || (Array.isArray(scope) && !scope.length)) return true;
  const set = scope instanceof Set ? scope : new Set([...scope].map((s) => String(s).toLowerCase()));
  if (set.has('*')) return true;
  for (const t of flowTokens(flow)) if (set.has(t)) return true;
  return false;
}

/** Normalize a user-entered scope string ("js, .json  css" or "*") to a list. */
export function parseScope(text) {
  const parts = String(text || '')
    .split(/[\s,]+/)
    .map((s) => s.trim().toLowerCase())
    .map((s) => (s === '*' ? '*' : s.replace(/^\.+/, '')))
    .filter(Boolean);
  return parts.includes('*') ? ['*'] : [...new Set(parts)];
}

function trunc(s, n = 90) {
  const t = String(s).replace(/\s+/g, ' ').trim();
  return t.length > n ? t.slice(0, n) + '…' : t;
}

/** Error string if a rule's pattern/flags don't compile, else '' (for the UI). */
export function ruleError({ pattern, flags = '' } = {}) {
  if (!pattern) return 'Pattern is required';
  try {
    new RegExp(pattern, flags);
    return '';
  } catch (e) {
    return e.message ?? String(e);
  }
}

/** Compile data rules to RegExp, skipping any that don't parse. `g` is enforced. */
export function compileRules(rules) {
  const out = [];
  for (const r of rules ?? []) {
    const flags = (r.flags || '').includes('g') ? r.flags : `${r.flags || ''}g`;
    try {
      out.push({ ...r, re: new RegExp(r.pattern, flags) });
    } catch {
      // skip invalid rule
    }
  }
  return out;
}

const BUILTIN_COMPILED = compileRules(BUILTIN_RULES);

/** Findings from a blob of text (`where` is 'request' or 'response'). */
export function scanText(text, where, compiled = BUILTIN_COMPILED) {
  const out = [];
  if (!text) return out;
  for (const rule of compiled) {
    rule.re.lastIndex = 0;
    let m;
    let count = 0;
    while ((m = rule.re.exec(text)) !== null && count < MAX_PER_RULE) {
      const match = (rule.group != null ? m[rule.group] : undefined) ?? m[0];
      out.push({ type: rule.id, severity: rule.severity, title: rule.title, where, match: trunc(match) });
      count++;
      if (m.index === rule.re.lastIndex) rule.re.lastIndex++; // guard zero-width matches
    }
  }
  return out;
}

/** Findings from response headers (CORS, cookie flags, tech disclosure). */
export function scanResponseHeaders(responseRaw) {
  const out = [];
  if (!responseRaw) return out;
  const head = responseRaw.split(/\r?\n\r?\n/)[0] || '';
  for (const line of head.split(/\r?\n/).slice(1)) {
    const i = line.indexOf(':');
    if (i <= 0) continue;
    const name = line.slice(0, i).trim().toLowerCase();
    const val = line.slice(i + 1).trim();
    if (!val) continue;
    if (name === 'access-control-allow-origin' && val === '*') {
      out.push({ type: 'cors', severity: 'medium', title: 'Permissive CORS (Access-Control-Allow-Origin: *)', where: 'response', match: trunc(line) });
    } else if (name === 'set-cookie') {
      const low = val.toLowerCase();
      const missing = [];
      if (!/;\s*httponly/.test(low) && !low.endsWith('httponly')) missing.push('HttpOnly');
      if (!/;\s*secure/.test(low) && !low.endsWith('secure')) missing.push('Secure');
      if (missing.length) out.push({ type: 'cookie', severity: 'low', title: `Cookie missing ${missing.join(' & ')}`, where: 'response', match: trunc(val, 60) });
    } else if ((name === 'server' || name === 'x-powered-by' || name === 'x-aspnet-version') && val) {
      // A disclosed version (nginx/1.25.3, PHP/8.2, 4.0.30319) is worth more than
      // a bare product name (cloudflare, Varnish), so rank version-bearing ones up.
      const hasVersion = /\/\s*\d|\d+\.\d+/.test(val);
      out.push({
        type: 'tech',
        severity: hasVersion ? 'low' : 'info',
        title: `Tech: ${line.slice(0, i).trim()}: ${val}`,
        where: 'response',
        match: trunc(line),
      });
    }
  }
  return out;
}

/**
 * All (deduplicated) findings for one flow. `compiled` is a pre-compiled rule
 * list (builtins + custom); defaults to the built-in rules.
 */
export function scanFlow({ requestRaw = '', responseRaw = '', compiled = BUILTIN_COMPILED } = {}) {
  const all = [...scanText(requestRaw, 'request', compiled), ...scanText(responseRaw, 'response', compiled), ...scanResponseHeaders(responseRaw)];
  const seen = new Set();
  return all.filter((f) => {
    const k = `${f.type}|${f.where}|${f.match}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}
