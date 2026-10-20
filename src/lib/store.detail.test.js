// @vitest-environment jsdom
// The opened-flow detail cache (store.loadDetail / peekDetail / prefetchDetail):
// it makes re-opening and arrow-key navigation instant, so it must serve repeats
// from memory, share concurrent requests, evict LRU, and drop stale entries.
import { beforeEach, describe, expect, it, vi } from 'vitest';

let getFlowCalls;
const detail = (id) => ({
  summary: { id },
  requestRaw: `req ${id}`,
  responseRaw: `res ${id}`,
  responseImage: null,
  requestTruncatedView: false,
  responseTruncatedView: false,
});
const backend = {
  get_flow: ({ id }) => detail(id),
  delete_flows: () => null,
  clear_flows: () => null,
  set_setting: () => null,
  set_flow_meta: () => null,
};

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    if (cmd === 'get_flow') getFlowCalls++;
    return (backend[cmd] ?? (() => null))(args ?? {});
  }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const { store, DETAIL_CACHE_MAX } = await import('./store.svelte.js');

beforeEach(async () => {
  getFlowCalls = 0;
  await store.clear(); // also empties the detail cache
  getFlowCalls = 0;
});

describe('flow detail cache', () => {
  it('serves a repeat open from cache with one backend call', async () => {
    const a = await store.loadDetail(5);
    const b = await store.loadDetail(5);
    expect(a.responseRaw).toBe('res 5');
    expect(b).toBe(a); // same cached object, not a refetch
    expect(getFlowCalls).toBe(1);
    expect(store.peekDetail(5)).toBe(a);
    expect(store.peekDetail(999)).toBeNull();
    expect(store.peekDetail(null)).toBeNull();
  });

  it('shares one in-flight request for concurrent opens', async () => {
    const [a, b] = await Promise.all([store.loadDetail(7), store.loadDetail(7)]);
    expect(a).toBe(b);
    expect(getFlowCalls).toBe(1);
  });

  it('prefetch warms the cache and de-dupes an in-flight load', async () => {
    store.prefetchDetail(3);
    store.prefetchDetail(3); // already in flight -> no second request
    const d = await store.loadDetail(3);
    expect(d.responseRaw).toBe('res 3');
    expect(getFlowCalls).toBe(1);
    expect(store.peekDetail(3)).toBe(d);
  });

  it('evicts the least-recently-used entry beyond the cap', async () => {
    const cap = DETAIL_CACHE_MAX;
    for (let i = 1; i <= cap; i++) await store.loadDetail(i); // fills to the cap
    expect(store.peekDetail(1)).not.toBeNull();
    await store.loadDetail(cap + 1); // one over -> oldest (id 1) is evicted
    expect(store.peekDetail(1)).toBeNull();
    expect(store.peekDetail(cap + 1)).not.toBeNull();
  });

  it('a repeat open refreshes LRU position so it survives eviction', async () => {
    const cap = DETAIL_CACHE_MAX;
    for (let i = 1; i <= cap; i++) await store.loadDetail(i);
    await store.loadDetail(1); // touch id 1 -> now newest
    await store.loadDetail(cap + 1); // evicts the new oldest, which is id 2
    expect(store.peekDetail(1)).not.toBeNull();
    expect(store.peekDetail(2)).toBeNull();
  });

  it('invalidates on delete so the next open refetches fresh bytes', async () => {
    await store.loadDetail(9);
    expect(getFlowCalls).toBe(1);
    await store.deleteFlows([9]);
    expect(store.peekDetail(9)).toBeNull();
    await store.loadDetail(9);
    expect(getFlowCalls).toBe(2);
  });
});
