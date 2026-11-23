// Organizer helpers (pure, unit-tested): the saved-request table's search + sort,
// and the Notes list's search + ordering.

/** Sortable columns of the saved-request table (Note is an icon column, not sortable). */
export const SAVED_COLUMNS = [
  { key: 'n', label: '#' },
  { key: 'source', label: 'Source' },
  { key: 'method', label: 'Method' },
  { key: 'host', label: 'Host' },
  { key: 'path', label: 'Path' },
  { key: 'status', label: 'Status' },
  // Same two numbers History shows, under the same names, so a saved record can be
  // read without going back to the flow it came from.
  { key: 'durationMs', label: 'Took' },
  { key: 'length', label: 'Resp size' },
  { key: 'label', label: 'Label' },
];

/** Columns sorted as numbers. The rest sort as text. */
const NUMERIC = new Set(['status', 'durationMs', 'length']);

const text = (v) => String(v ?? '').toLowerCase();

/** Table rows: each saved item with its 1-based position `n` in the collection,
 *  filtered by `query` (any column, the label or the note) and sorted by `key` / `dir`
 *  (1 = ascending, -1 = descending). Ties keep the saved order. */
export function savedRows(items, query = '', key = 'n', dir = 1) {
  const q = text(query).trim();
  let rows = (items ?? []).map((it, i) => ({ it, n: i + 1 }));
  if (q) {
    rows = rows.filter(({ it }) =>
      [it.source, it.method, it.host, it.path, it.status, it.label, it.note, it.name].some((v) => text(v).includes(q)),
    );
  }
  // A record with no measurement sorts below every measured one rather than beside a
  // genuine zero: -1 is unreachable for a duration or a size, so it cannot collide
  // with a real 0 ms or an empty body.
  const num = (v) => (v == null ? -1 : Number(v) || 0);
  const value = (r) => (key === 'n' ? r.n : NUMERIC.has(key) ? num(r.it[key]) : text(r.it[key]));
  return rows.sort((a, b) => {
    const x = value(a);
    const y = value(b);
    const c = typeof x === 'number' ? x - y : x.localeCompare(y, undefined, { numeric: true });
    return c * dir || a.n - b.n;
  });
}

/** Notes for the list: matching `query` (title or body), most recently edited first. */
export function notesView(notes, query = '') {
  const q = text(query).trim();
  return (notes ?? [])
    .filter((n) => !q || text(n.title).includes(q) || text(n.body).includes(q))
    .sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0));
}

/** First non-empty line of a note body, for the list preview. */
export function notePreview(body) {
  return (String(body ?? '').split('\n').find((l) => l.trim()) ?? '').trim();
}
