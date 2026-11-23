import { describe, it, expect } from 'vitest';
import { flowState, fmtWaited, LATE_AFTER_MS } from './format.js';
import { readFileSync } from 'node:fs';

// Five situations share "no status code" and only one of them is a failure. Flattening
// them is what made a healthy request in flight render as a red ERR, so these assert
// the separation rather than the exact wording.
describe('flowState', () => {
  const at = (over = {}) => ({ id: 1, ts: 1000, status: 0, error: null, pending: false, ...over });

  it('calls a request that is still out a wait, not an error', () => {
    const st = flowState(at({ pending: true }), 1000 + 5000);
    expect(st.kind).toBe('wait');
    expect(st.badge).toBe('WAIT');
    expect(st.cls).not.toContain('err');
    expect(st.title.toLowerCase()).toContain('nothing has failed');
  });

  it('still refuses to call it an error once the wait is long', () => {
    const st = flowState(at({ pending: true }), 1000 + LATE_AFTER_MS + 1);
    expect(st.kind).toBe('late');
    expect(st.badge).toBe('LATE');
    // The point of the whole design: the threshold changes the word, never the verdict.
    expect(st.cls).not.toContain('err');
    expect(st.title.toLowerCase()).toContain('nothing has failed');
  });

  it('crosses from wait to late only at the threshold', () => {
    expect(flowState(at({ pending: true }), 1000 + LATE_AFTER_MS - 1).kind).toBe('wait');
    expect(flowState(at({ pending: true }), 1000 + LATE_AFTER_MS).kind).toBe('late');
  });

  it('reports a request that never got an answer without blaming the target', () => {
    // No pending flag, because the process that was waiting is gone: a reloaded row.
    const st = flowState(at(), 9e9);
    expect(st.kind).toBe('noresp');
    expect(st.badge).toBe('NO RSP');
    expect(st.title).toMatch(/not a failure of the target/i);
  });

  it('keeps ERR for something that was actually observed to fail', () => {
    const st = flowState(at({ error: 'connection error: Operation timed out (os error 60)' }), 9e9);
    expect(st.kind).toBe('err');
    expect(st.cls).toBe('s-err');
    expect(st.title).toContain('os error 60');
  });

  it('shows a real response as its status', () => {
    expect(flowState(at({ status: 200 }), 9e9)).toMatchObject({ kind: 'done', badge: '200' });
  });

  it('never treats a finished response as waiting, whatever the flag says', () => {
    // The final write wins: a stale pending flag must not outrank a real status.
    expect(flowState(at({ status: 200, pending: true }), 9e9).kind).toBe('done');
  });
});

describe('fmtWaited', () => {
  it('reads as a wait, not as a measurement', () => {
    expect(fmtWaited(8_400)).toBe('8s');
    expect(fmtWaited(74_000)).toBe('1m 14s');
    expect(fmtWaited(7_380_000)).toBe('2h 3m');
    expect(fmtWaited(-5)).toBe('0s');
  });
});

// The threshold is an operator setting (Settings > Project > Performance), so it is
// a parameter, not a constant. These pin the two things that must hold whatever the
// operator picks: it can be moved, and it can be turned off.
describe('flowState with an operator-set threshold', () => {
  const waiting = (waited) => [{ id: 1, ts: 0, status: 0, error: null, pending: true }, waited];

  it('escalates at the number the operator chose, not the default', () => {
    const [f, t] = waiting(10_000);
    expect(flowState(f, t, 5_000).kind).toBe('late');
    expect(flowState(f, t, 30_000).kind).toBe('wait');
  });

  it('never escalates when the operator sets 0', () => {
    const [f] = waiting(0);
    // A day of waiting, and still only "waiting", because that is what was asked for.
    expect(flowState(f, 86_400_000, 0).kind).toBe('wait');
    expect(flowState(f, 86_400_000, 0).badge).toBe('WAIT');
  });

  it('keeps a real failure red no matter what the threshold says', () => {
    const failed = { id: 1, ts: 0, status: 0, error: 'connection refused', pending: false };
    expect(flowState(failed, 1, 0).kind).toBe('err');
    expect(flowState(failed, 9e9, 1).kind).toBe('err');
  });
});

describe('a request the target never saw is not a request the target ignored', () => {
  // Both have no status, no error and nothing waiting on them. One is a gap in the
  // record of a real exchange; the other is a request that never left Ngintip, so
  // there is no exchange with the target to read anything into at all.
  const base = { id: 1, ts: 0, status: 0, error: null, pending: false };

  it('reads an unsent request as its own state', () => {
    const s = flowState({ ...base, unsent: true }, 1000);
    expect(s.kind).toBe('unsent');
    expect(s.badge).toBe('UNSENT');
    expect(s.title).toMatch(/never saw it/);
  });

  it('still reads a sent one that went unanswered as a gap in the record', () => {
    const s = flowState({ ...base, unsent: false }, 1000);
    expect(s.kind).toBe('noresp');
    expect(s.badge).toBe('NO RSP');
  });

  it('is not red: nothing failed in either case', () => {
    expect(flowState({ ...base, unsent: true }, 1000).cls).not.toBe('s-err');
    expect(flowState({ ...base, unsent: false }, 1000).cls).not.toBe('s-err');
  });

  it('never outranks a row that is still waiting, answered, or observed to fail', () => {
    // `unsent` is a flag this process writes; a status, a wait and an error are things
    // that were watched happening. None of them may be silenced by the flag, today or
    // after some later writer sets both.
    expect(flowState({ ...base, unsent: true, pending: true }, 1000).kind).toBe('wait');
    expect(flowState({ ...base, unsent: true, error: 'connection refused' }, 1000).kind).toBe('err');
    expect(flowState({ ...base, unsent: true, status: 200 }, 1000).kind).toBe('done');
  });
});

describe('the duration column never claims a round trip that did not happen', () => {
  // FlowTable branches on `kind` to decide whether to print `durationMs`. A state
  // added without that branch prints "0 ms" for an exchange that never took place,
  // which reads as an instant answer. Source-level, because the string the cell
  // renders is the thing under test, not anything jsdom can be asked for.
  const src = readFileSync(new URL('../components/FlowTable.svelte', import.meta.url), 'utf8');

  it('shows a dash for every state that has no round trip', () => {
    const cell = src.split('\n').find((l) => l.includes('{f.durationMs} ms'));
    expect(cell).toBeTruthy();
    for (const kind of ['noresp', 'unsent']) expect(cell).toContain(`st.kind === '${kind}'`);
    for (const kind of ['wait', 'late']) expect(cell).toContain(`st.kind === '${kind}'`);
  });
});
