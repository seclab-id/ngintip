// @vitest-environment jsdom
// Proxy listeners are per project: creating or opening a project must adopt that
// project's set, not leave the previous project's listeners running. The rules live in
// the backend (commands.rs); what is pinned here is that the UI actually asks for the
// swap on every project switch and shows what comes back.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ONE = [{ id: 'default', host: '127.0.0.1', port: 8080, enabled: true, running: true }];
const THREE = [
  ...ONE,
  { id: 'lan', host: '0.0.0.0', port: 8081, enabled: true, running: true },
  { id: 'alt', host: '127.0.0.1', port: 8082, enabled: true, running: true },
];

let calls;
let listeners; // what the backend would report after a sync
let syncFails;

const backend = {
  list_listeners: () => listeners,
  start_listeners: () => listeners,
  sync_listeners: () => {
    if (syncFails) throw 'bind failed: port in use';
    return listeners;
  },
  blank_project: () => ({ name: 'scratch', path: '' }),
  open_project: () => ({ name: 'target-b', path: '/tmp/target-b.ngintip' }),
  get_config: () => ({ listeners, hostOverrides: [] }),
  get_scope: () => [],
  get_rules: () => [],
  list_flows: () => [],
  logs: () => [],
  organizer_state: () => ({ rev: 0, collections: [], activeId: null }),
};

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    calls.push(cmd);
    return (backend[cmd] ?? (() => null))(args ?? {});
  }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(async () => '/tmp/target-b.ngintip'),
  save: vi.fn(),
}));

const { store } = await import('./store.svelte.js');

beforeEach(() => {
  calls = [];
  syncFails = false;
  listeners = THREE;
  store.listeners = THREE;
  store.proxyError = '';
});

describe('listeners follow the project', () => {
  it('adopts the new project’s single listener instead of carrying three over', async () => {
    expect(store.listeners).toHaveLength(3);
    // What the backend reports for a brand-new project: the default set, one listener.
    listeners = ONE;

    await store.blankProject();

    expect(calls).toContain('sync_listeners');
    expect(store.listeners).toEqual(ONE);
  });

  it('adopts the opened project’s set', async () => {
    listeners = ONE;
    await store.openProject(); // the file dialog is mocked to pick target-b

    expect(calls).toContain('sync_listeners');
    expect(store.listeners).toEqual(ONE);
  });

  it('asks for the swap after the project is open, not before', async () => {
    listeners = ONE;
    await store.blankProject();

    const project = calls.indexOf('blank_project');
    const sync = calls.indexOf('sync_listeners');
    expect(project).toBeGreaterThanOrEqual(0);
    // The backend reads the set out of the open project's store, so the project has to
    // be switched first or it would answer from the previous one.
    expect(sync).toBeGreaterThan(project);
  });

  it('falls back to the reported state and surfaces the error when a bind fails', async () => {
    syncFails = true;
    listeners = ONE;

    await store.blankProject();

    expect(store.proxyError).toContain('bind failed');
    // Still shows reality (what list_listeners reports) rather than going blank.
    expect(store.listeners).toEqual(ONE);
  });
});
