// Advanced intercept filter rules. The proxy (src-tauri/src/intercept.rs) is the
// source of truth for what actually gets held; this mirrors its logic for the UI
// (rule editor, preview) and normalizes rules before they are persisted/sent.

export const INTERCEPT_FILTER_FIELDS = [
  { id: 'host', label: 'Host' },
  { id: 'path', label: 'Path' },
  { id: 'url', label: 'URL' },
  { id: 'method', label: 'Method' },
];

const FIELD_IDS = INTERCEPT_FILTER_FIELDS.map((f) => f.id);

export const newInterceptRule = (patch = {}) => ({ field: 'host', mode: 'include', value: '', sensitive: false, ...patch });

export function normalizeInterceptRule(r = {}) {
  return {
    field: FIELD_IDS.includes(r.field) ? r.field : 'host',
    mode: r.mode === 'exclude' ? 'exclude' : 'include',
    value: String(r.value ?? ''),
    sensitive: !!r.sensitive,
  };
}

/**
 * Whether a message with these fields would be held, given the rules. Precedence:
 * a matching exclude skips it; otherwise, if there are includes it must match one;
 * with no rules everything is held. Mirrors InterceptFilter::allows in Rust.
 */
export function interceptAllows(rules, { method = '', host = '', path = '', url = '' }) {
  const field = (f) => (f === 'host' ? host : f === 'path' ? path : f === 'method' ? method : url);
  const matches = (r) => {
    if (!r.value) return false;
    const hay = field(r.field) ?? '';
    return r.sensitive ? hay.includes(r.value) : hay.toLowerCase().includes(r.value.toLowerCase());
  };
  const active = (rules ?? []).filter((r) => r.value);
  if (active.some((r) => r.mode === 'exclude' && matches(r))) return false;
  const includes = active.filter((r) => r.mode === 'include');
  if (!includes.length) return true;
  return includes.some(matches);
}
