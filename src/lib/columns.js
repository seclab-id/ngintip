// The configurable columns for the HTTP History table. Which ones show is
// stored per project (see store.historyColumns). FlowTable renders each key.

export const HISTORY_COLUMNS = [
  { key: 'id', label: '#', width: '52px', always: true },
  { key: 'ts', label: 'Time', width: '158px' },
  { key: 'method', label: 'Method', width: '72px' },
  { key: 'host', label: 'Host', width: 'minmax(120px, 1.1fr)' },
  { key: 'path', label: 'Path', width: 'minmax(160px, 2.2fr)' },
  { key: 'query', label: 'Query', width: 'minmax(120px, 1.3fr)' },
  { key: 'status', label: 'Status', width: '62px' },
  { key: 'length', label: 'Resp size', width: '86px' },
  { key: 'reqLength', label: 'Req size', width: '82px' },
  { key: 'resBody', label: 'Resp body', width: '84px' },
  { key: 'reqBody', label: 'Req body', width: '80px' },
  { key: 'mime', label: 'MIME', width: '90px' },
  { key: 'durationMs', label: 'Took', width: '66px' },
  { key: 'modified', label: 'Modified', width: '78px' },
  { key: 'note', label: 'Notes', width: 'minmax(90px, 1fr)' },
];

// Shown by default (mirrors the classic layout, plus nothing exotic).
export const DEFAULT_COLUMN_KEYS = ['id', 'ts', 'method', 'host', 'path', 'status', 'length', 'mime', 'durationMs', 'note'];

const CATALOG_ORDER = HISTORY_COLUMNS.map((c) => c.key);

/** Keep only known keys, in catalog order, always including the always-on ones. */
export function normalizeColumns(keys) {
  const wanted = new Set(Array.isArray(keys) ? keys : []);
  return CATALOG_ORDER.filter((k) => wanted.has(k) || HISTORY_COLUMNS.find((c) => c.key === k)?.always);
}

/** The query string of a path ("a=1&b=2"), or "" when there is none. */
export function queryOf(path) {
  const i = (path ?? '').indexOf('?');
  return i === -1 ? '' : path.slice(i + 1);
}
