// @vitest-environment jsdom
// What a 39K-flow project exposed. Two separate things are pinned here:
//
//  1. Highlighting a large selection is ONE command, not one per flow. It used to be
//     `Promise.all(ids.map(...))` with a `this.flows.find()` inside the map: 2.1 s of
//     lookup plus 39,662 commands in flight, each taking the store mutex on the IPC
//     thread. The context menu's "is any of these highlighted" check had the same shape.
//  2. A result that arrives after another project was opened is dropped. Every project
//     mints flow ids from 1, so the ids in a late reply look perfectly valid in the
//     project that is now open and point at somebody else's requests.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const calls = [];
const failCommands = new Set();
// `list_flows` answers through a function, so each call can be told apart by the rows it
// returns. `duringCall` runs while a command is in the air, which is where a project swap
// has to be survivable.
let listFlows = () => [];
let coloured = null;
let duringCall = null;
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    calls.push([cmd, args]);
    if (failCommands.has(cmd)) throw new Error(`${cmd} not allowed`);
    const hook = duringCall;
    if (hook) {
      duringCall = null;
      await hook(cmd, args);
    }
    if (cmd === 'list_flows') return listFlows();
    if (cmd === 'set_flow_colors') return coloured ?? args.ids.length;
    if (cmd === 'get_flow') return { id: args.id, requestRaw: `GET /flow-${args.id}` };
    if (cmd === 'get_setting') return null;
    return null;
  }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const { store } = await import('./store.svelte.js');

const flow = (id, extra = {}) => ({
  id,
  ts: 1700000000000 + id,
  method: 'GET',
  scheme: 'https',
  host: 'tdwhaqa.telkomsel.com',
  path: `/staging/api/payment/list/v3?x=${id}`,
  status: 200,
  length: 10,
  reqLength: 5,
  mime: 'application/json',
  durationMs: 7,
  error: null,
  note: '',
  color: '',
  streaming: false,
  pending: false,
  modified: false,
  ...extra,
});

beforeEach(() => {
  calls.length = 0;
  failCommands.clear();
  listFlows = () => [];
  coloured = null;
  duringCall = null;
  store.flows = [flow(1), flow(2), flow(3)];
  store.selectedIds = new Set();
});

describe('highlighting a large selection', () => {
  it('sends one command for the whole selection, not one per flow', async () => {
    const ids = store.flows.map((f) => f.id);
    await store.setColorMany(ids, 'red');
    expect(calls.filter(([c]) => c === 'set_flow_meta')).toHaveLength(0);
    const bulk = calls.filter(([c]) => c === 'set_flow_colors');
    expect(bulk).toHaveLength(1);
    expect(bulk[0][1]).toEqual({ ids, color: 'red' });
    expect(store.flows.map((f) => f.color)).toEqual(['red', 'red', 'red']);
  });

  it('leaves the notes alone, because it never reads them', async () => {
    store.flows = [flow(1, { note: 'kept' }), flow(2)];
    await store.setColorMany([1, 2], 'amber');
    expect(store.flows[0].note).toBe('kept');
    // The old shape looked a note up per id and sent it back; this one sends no notes.
    expect(calls.some(([, a]) => a && 'note' in a)).toBe(false);
  });

  it('does nothing at all for an empty selection', async () => {
    await store.setColorMany([], 'red');
    expect(calls).toHaveLength(0);
  });

  it('applies only as far as the backend got when the project changed mid-update', async () => {
    coloured = 2; // the backend stopped after two ids and said so
    await store.setColorMany([1, 2, 3], 'red');
    expect(store.flows.map((f) => f.color)).toEqual(['red', 'red', '']);
  });

  it('keeps History unchanged when the command fails', async () => {
    failCommands.add('set_flow_colors');
    await store.setColorMany([1, 2, 3], 'red');
    expect(store.flows.map((f) => f.color)).toEqual(['', '', '']);
  });

  it('decides "any highlighted" in one pass over History', () => {
    expect(store.hasColor([1, 2, 3])).toBe(false);
    expect(store.hasColor([2])).toBe(false);
    store.flows = [flow(1), flow(2, { color: 'red' }), flow(3)];
    expect(store.hasColor([1, 3])).toBe(false);
    expect(store.hasColor([1, 2, 3])).toBe(true);
    expect(store.hasColor([2])).toBe(true);
    // An id History does not hold is not a highlight.
    expect(store.hasColor([999])).toBe(false);
  });
});

describe('a result that arrives after the project moved', () => {
  it('keeps the flow list of the project that is open, not the one it was fetched from', async () => {
    // Each list_flows answers with rows naming its own call, so the loser is visible.
    let n = 0;
    listFlows = () => [flow(1, { host: `call-${++n}.example` })];
    // A swap runs to completion while the first project's list_flows is still in the air.
    duringCall = async (cmd) => {
      if (cmd !== 'list_flows') return;
      await store.blankProject();
    };
    await store.blankProject();
    // Two list_flows went out. The nested swap's is issued and answered FIRST (call-1),
    // under the newer generation; the outer, older one is answered second (call-2) and
    // therefore resolves LAST. Last writer wins is exactly the bug: without the
    // generation guard History would end up holding call-2, the project nobody has open.
    expect(store.flows.map((f) => f.host)).toEqual(['call-1.example']);
  });

  it('does not cache a flow detail fetched from the project that was open before', async () => {
    // Baseline: a detail that arrives while the same project is open is cached, so a
    // second open of the same row costs no second command.
    const first = await store.loadDetail(1);
    expect(first).toEqual({ id: 1, requestRaw: 'GET /flow-1' });
    const beforeCached = calls.filter(([c]) => c === 'get_flow').length;
    await store.loadDetail(1);
    expect(calls.filter(([c]) => c === 'get_flow')).toHaveLength(beforeCached);

    // Now with a swap in flight. Id 2 exists in both projects and means something
    // different in each, so the reply must be thrown away rather than cached.
    duringCall = async (cmd) => {
      if (cmd !== 'get_flow') return;
      await store.blankProject();
    };
    const late = await store.loadDetail(2);
    expect(late).toBeNull();
    // Nothing was cached under id 2: asking again really asks the backend again.
    const beforeRetry = calls.filter(([c]) => c === 'get_flow').length;
    await store.loadDetail(2);
    expect(calls.filter(([c]) => c === 'get_flow').length).toBe(beforeRetry + 1);
  });
});
