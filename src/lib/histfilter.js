// Advanced History filter: a structured filter applied on top of the text
// filter / scope / sitemap. Kept pure so it can be unit-tested.

export const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS', 'HEAD'];
export const STATUS_CLASSES = [
  { key: '2', label: '2xx' },
  { key: '3', label: '3xx' },
  { key: '4', label: '4xx' },
  { key: '5', label: '5xx' },
  { key: 'err', label: 'Error' },
];

export function emptyAdvanced() {
  return {
    methods: [],
    status: [],
    reqBody: 'any', // 'any' | 'yes' | 'no'
    resBody: 'any',
    minMs: '',
    maxMs: '',
    // Include/exclude rules edited in the Filters modal (host/path/mime/etc. all
    // live here now instead of separate single fields).
    filters: [],
  };
}

/** Fields a Filters-modal rule can target. */
export const FILTER_FIELDS = [
  { key: 'path', label: 'Path' },
  { key: 'ext', label: 'Extension' },
  { key: 'host', label: 'Host' },
  { key: 'url', label: 'URL' },
  { key: 'method', label: 'Method' },
  { key: 'status', label: 'Status' },
  { key: 'mime', label: 'MIME' },
];

/**
 * The URL's real file extension (lowercase, no dot), e.g. "js" for
 * `/assets/app.min.js?v=2`. The query/fragment is stripped first and only the
 * last path segment is considered, so `/data.json` is "json" (never "js") and
 * `/api/users` is "". Used by the exact-match `ext` field below.
 */
export function urlExtension(path) {
  const seg = String(path ?? '')
    .split(/[?#]/)[0]
    .split('/')
    .pop();
  const m = /\.([a-z0-9]+)$/i.exec(seg);
  return m ? m[1].toLowerCase() : '';
}

/** A blank filter rule (defaults to a case-insensitive exclude, the common case). */
export const newFilter = (patch = {}) => ({ field: 'path', mode: 'exclude', value: '', sensitive: false, enabled: true, ...patch });

/**
 * Preset exclude rules for the "Exclude static assets" quick-fill button: the
 * usual junk (scripts, styles, images, fonts, source maps). Every entry targets
 * the `ext` field, which matches the URL's real file extension EXACTLY, so
 * excluding "js" hides app.js but leaves `.json` API traffic visible. Each entry
 * becomes a full exclude rule via newFilter().
 */
export const BASIC_EXCLUDE_FILTERS = [
  { field: 'ext', value: 'js' },
  { field: 'ext', value: 'mjs' },
  { field: 'ext', value: 'css' },
  { field: 'ext', value: 'png' },
  { field: 'ext', value: 'jpg' },
  { field: 'ext', value: 'jpeg' },
  { field: 'ext', value: 'gif' },
  { field: 'ext', value: 'svg' },
  { field: 'ext', value: 'ico' },
  { field: 'ext', value: 'webp' },
  { field: 'ext', value: 'woff' },
  { field: 'ext', value: 'woff2' },
  { field: 'ext', value: 'ttf' },
  { field: 'ext', value: 'otf' },
  { field: 'ext', value: 'eot' },
  { field: 'ext', value: 'map' },
];

/** The flow value a rule's field points at. */
function fieldValue(f, field) {
  switch (field) {
    case 'host':
      return f.host ?? '';
    case 'mime':
      return f.mime ?? '';
    case 'method':
      return f.method ?? '';
    case 'status':
      return String(f.status ?? '');
    case 'url':
      return `${f.scheme ?? ''}://${f.host ?? ''}${f.path ?? ''}`;
    case 'ext':
      return urlExtension(f.path);
    case 'path':
    default:
      return f.path ?? '';
  }
}

/** Rules that are enabled and have a non-empty value (a disabled rule is ignored). */
function activeFilters(a) {
  return (a?.filters ?? []).filter((r) => r && r.enabled !== false && String(r.value).trim() !== '');
}

/** The status-class key ('2'..'5' or 'err') of a numeric status. */
export function statusKey(status) {
  return status >= 200 && status < 600 ? String(Math.floor(status / 100)) : 'err';
}

/** True when any advanced filter field is set (drives the active badge). */
export function advActive(a) {
  if (!a) return false;
  return (
    a.methods.length > 0 ||
    a.status.length > 0 ||
    a.reqBody !== 'any' ||
    a.resBody !== 'any' ||
    String(a.minMs).trim() !== '' ||
    String(a.maxMs).trim() !== '' ||
    activeFilters(a).length > 0
  );
}

/** Whether a flow summary passes the advanced filter. */
export function matchAdvanced(f, a) {
  if (!advActive(a)) return true;
  if (a.methods.length && !a.methods.includes(f.method)) return false;
  if (a.status.length && !a.status.includes(statusKey(f.status))) return false;

  const reqHas = (f.reqLength ?? 0) > 0;
  if (a.reqBody === 'yes' && !reqHas) return false;
  if (a.reqBody === 'no' && reqHas) return false;

  const resHas = (f.length ?? 0) > 0;
  if (a.resBody === 'yes' && !resHas) return false;
  if (a.resBody === 'no' && resHas) return false;

  const min = Number(a.minMs);
  if (String(a.minMs).trim() !== '' && Number.isFinite(min) && f.durationMs < min) return false;
  const max = Number(a.maxMs);
  if (String(a.maxMs).trim() !== '' && Number.isFinite(max) && f.durationMs > max) return false;

  // Filters-modal rules. Exclude is a denylist (any match hides the flow);
  // include is an allowlist (when present, the flow must match at least one).
  const rules = activeFilters(a);
  if (rules.length) {
    const hit = (r) => {
      const hay = fieldValue(f, r.field);
      const needle = String(r.value).trim();
      // Extension is an exact match (leading dots in the value are ignored), so
      // excluding "js" hides app.js but never data.json.
      if (r.field === 'ext') {
        const want = needle.replace(/^\.+/, '').toLowerCase();
        return want !== '' && hay === want;
      }
      return r.sensitive ? hay.includes(needle) : hay.toLowerCase().includes(needle.toLowerCase());
    };
    if (rules.some((r) => r.mode === 'exclude' && hit(r))) return false;
    const includes = rules.filter((r) => r.mode !== 'exclude');
    if (includes.length && !includes.some(hit)) return false;
  }

  return true;
}
