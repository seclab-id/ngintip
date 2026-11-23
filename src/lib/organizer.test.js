import { describe, expect, it } from 'vitest';
import { notePreview, notesView, savedRows } from './organizer.js';

const items = [
  { source: 'history', method: 'GET', host: 'b.com', path: '/z', status: 200, label: '', note: '' },
  { source: 'repeater', method: 'POST', host: 'a.com', path: '/login', status: 401, label: 'auth', note: 'creds here' },
  { source: 'intruder', method: 'GET', host: 'c.com', path: '/a', status: null, label: '', note: '' },
];

describe('organizer', () => {
  it('numbers rows by saved position and keeps that order by default', () => {
    expect(savedRows(items).map((r) => r.n)).toEqual([1, 2, 3]);
  });

  it('sorts by a column ascending / descending (numeric for # and status)', () => {
    expect(savedRows(items, '', 'host', 1).map((r) => r.it.host)).toEqual(['a.com', 'b.com', 'c.com']);
    expect(savedRows(items, '', 'host', -1).map((r) => r.it.host)).toEqual(['c.com', 'b.com', 'a.com']);
    expect(savedRows(items, '', 'status', -1).map((r) => r.it.status)).toEqual([401, 200, null]);
    expect(savedRows(items, '', 'n', -1).map((r) => r.n)).toEqual([3, 2, 1]);
    expect(savedRows(items, '', 'source', 1).map((r) => r.it.source)).toEqual(['history', 'intruder', 'repeater']);
  });

  it('searches every column plus label and note, case-insensitively', () => {
    expect(savedRows(items, 'login').map((r) => r.n)).toEqual([2]);
    expect(savedRows(items, 'CREDS').map((r) => r.n)).toEqual([2]); // in the note
    expect(savedRows(items, 'auth').map((r) => r.n)).toEqual([2]); // the label
    expect(savedRows(items, 'intruder').map((r) => r.n)).toEqual([3]); // the source
    expect(savedRows(items, '401').map((r) => r.n)).toEqual([2]); // the status
    expect(savedRows(items, 'nothing-matches')).toEqual([]);
  });

  it('keeps each row original # when sorted or filtered', () => {
    const rows = savedRows(items, 'get', 'host', -1);
    expect(rows.map((r) => [r.n, r.it.host])).toEqual([
      [3, 'c.com'],
      [1, 'b.com'],
    ]);
  });

  it('lists notes newest-edited first, filtered by title or body', () => {
    const notes = [
      { id: 1, title: 'Old', body: 'alpha', updatedAt: 1 },
      { id: 2, title: 'New', body: 'beta\nsecond line', updatedAt: 5 },
    ];
    expect(notesView(notes).map((n) => n.id)).toEqual([2, 1]);
    expect(notesView(notes, 'ALPHA').map((n) => n.id)).toEqual([1]);
    expect(notesView(notes, 'new').map((n) => n.id)).toEqual([2]);
    expect(notePreview('\n\n  first real line \nnext')).toBe('first real line');
    expect(notePreview('')).toBe('');
  });
});

// Took / Resp size: the same two numbers History shows, kept on the record so a saved
// request can be read after History is cleared. A record saved before they existed has
// neither, and that has to stay tellable from a measured zero.
describe('savedRows with response measurements', () => {
  const items = [
    { id: 'a', host: 'h', path: '/a', durationMs: 1200, length: 500 },
    { id: 'b', host: 'h', path: '/b', durationMs: 30, length: 0 },
    { id: 'c', host: 'h', path: '/c' }, // saved before Took / Resp size existed
  ];
  const order = (key, dir) => savedRows(items, '', key, dir).map((r) => r.it.id);

  it('sorts by response time as a number, not as text', () => {
    // 30 before 1200: as text, "1200" would sort first.
    expect(order('durationMs', 1)).toEqual(['c', 'b', 'a']);
    expect(order('durationMs', -1)).toEqual(['a', 'b', 'c']);
  });

  it('sorts by size as a number', () => {
    expect(order('length', -1)).toEqual(['a', 'b', 'c']);
  });

  it('keeps an unmeasured record apart from a genuine zero', () => {
    // b really did come back with an empty body; c was never measured at all. Ascending,
    // the unmeasured one sorts below the real 0 rather than tying with it.
    const asc = savedRows(items, '', 'length', 1).map((r) => r.it.id);
    expect(asc.indexOf('c')).toBeLessThan(asc.indexOf('b'));
  });
});
