// @vitest-environment jsdom
// Mounts the whole UI against a fake backend and drives every view, to catch
// runtime errors that `vite build` can't see. Not a substitute for trying the app.
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, tick, unmount } from 'svelte';

const listeners = {};
const calls = [];
const flows = [
  { id: 1, ts: 1, method: 'GET', scheme: 'https', host: 'api.example.com', path: '/v1/users?id=1', status: 200, length: 42, reqLength: 0, mime: 'json', durationMs: 12, error: null, note: '', color: '', streaming: false, modified: false },
  { id: 2, ts: 2, method: 'POST', scheme: 'https', host: 'api.example.com', path: '/v1/login', status: 401, length: 10, reqLength: 24, mime: 'json', durationMs: 30, error: null, note: 'cek', color: 'red', streaming: false, modified: true },
  { id: 3, ts: 3, method: 'GET', scheme: 'wss', host: 'chat.example.com', path: '/socket', status: 101, length: 0, reqLength: 0, mime: '', durationMs: 5, error: null, note: '', color: '', streaming: false, modified: false },
];
// Fake Repeater history, like the repeater_history table.
const sent = [];
// Paths handed to save_project_as, and files that "exist" for path_exists.
const savedAs = [];
const existing = new Set();
// Fake proxy listeners (the backend returns the full status list from every mutation).
let mockListeners = [{ id: 'default', host: '0.0.0.0', port: 8080, enabled: true, running: true }];
// Fake Organizer: the backend owns it (src-tauri/src/organizer.rs); same ops and rules.
const org = { rev: 0, collections: [], activeId: null };
let orgSeq = 0;
function orgFind(itemId) {
  for (const c of org.collections) {
    const it = c.items.find((i) => i.id === itemId);
    if (it) return [c, it];
  }
  return [null, null];
}
function orgApply(a) {
  const hasActive = () => org.collections.some((c) => c.id === org.activeId);
  const create = (name) => {
    const c = { id: `col-${++orgSeq}`, name: String(name ?? '').trim() || 'Collection', items: [] };
    org.collections.push(c);
    if (!hasActive()) org.activeId = c.id;
    return c;
  };
  let collectionId = null;
  let itemId = null;
  switch (a.op) {
    case 'createCollection': {
      const c = create(a.name);
      if (a.activate) org.activeId = c.id;
      collectionId = c.id;
      break;
    }
    case 'renameCollection': {
      const c = org.collections.find((x) => x.id === a.id);
      if (!c) throw `no collection ${a.id}`;
      if (String(a.name).trim()) c.name = String(a.name).trim();
      collectionId = c.id;
      break;
    }
    case 'deleteCollection':
      if (!org.collections.some((c) => c.id === a.id)) throw `no collection ${a.id}`;
      org.collections = org.collections.filter((c) => c.id !== a.id);
      if (org.activeId === a.id) org.activeId = null;
      break;
    case 'setActive':
      if (a.id != null && !org.collections.some((c) => c.id === a.id)) throw `no collection ${a.id}`;
      org.activeId = a.id ?? null;
      collectionId = a.id ?? null;
      break;
    case 'save': {
      const want = String(a.collection ?? '').trim();
      let c = want
        ? (org.collections.find((x) => x.id === want || x.name.toLowerCase() === want.toLowerCase()) ?? create(want))
        : org.collections.find((x) => x.id === org.activeId);
      if (!c) {
        c = org.collections.find((x) => x.name === 'Saved') ?? create('Saved');
        org.activeId = c.id;
      }
      const it = a.item ?? {};
      const [method = 'GET', path = '/'] = String(it.request ?? '').split('\n')[0].trim().split(/\s+/).filter(Boolean);
      const host = String(it.target ?? '').replace(/^[a-z]+:\/\//i, '').split('/')[0];
      const saved = { id: `req-${++orgSeq}`, name: it.name || `${method} ${host}${path}`, method, host, path, target: it.target ?? '', request: it.request ?? '', response: it.response ?? '', status: it.status ?? null, label: it.label ?? '', note: it.note ?? '', color: it.color ?? '', source: it.source ?? '', savedAt: Date.now() };
      c.items.push(saved);
      collectionId = c.id;
      itemId = saved.id;
      break;
    }
    case 'update': {
      const [c, it] = orgFind(a.itemId);
      if (!it) throw `no saved request ${a.itemId}`;
      for (const k of ['label', 'note', 'color']) if (a[k] != null) it[k] = a[k];
      collectionId = c.id;
      itemId = it.id;
      break;
    }
    case 'remove': {
      const [c] = orgFind(a.itemId);
      if (!c) throw `no saved request ${a.itemId}`;
      c.items = c.items.filter((i) => i.id !== a.itemId);
      collectionId = c.id;
      break;
    }
    case 'move': {
      const to = org.collections.find((x) => x.id === a.to);
      const [from, it] = orgFind(a.itemId);
      if (!to || !it) throw 'bad move';
      if (from !== to) {
        from.items = from.items.filter((i) => i.id !== a.itemId);
        to.items.push(it);
      }
      collectionId = to.id;
      itemId = it.id;
      break;
    }
    default:
      throw `unknown op ${a.op}`;
  }
  org.rev += 1;
  return { state: structuredClone(org), collectionId, itemId };
}
const backend = {
  send_raw: ({ target, raw, tab }) => {
    const line = raw.split('\n')[0];
    const res = line.includes('/redirect')
      ? { status: 302, durationMs: 1, length: 0, responseRaw: 'HTTP/1.1 302 Found\nLocation: /after\n\n', responseImage: null }
      : { status: 200, durationMs: 1, length: 2, responseRaw: `HTTP/1.1 200 OK\n\nsent: ${line}`, responseImage: null };
    if (tab != null) sent.push({ id: sent.length + 1, tabId: tab, ts: Date.now(), target, raw, error: null, ...res });
    return res;
  },
  cancel_send: () => true,
  repeater_history: ({ tab }) => sent.filter((e) => e.tabId === tab).map((e) => ({ ...e, responseRaw: '' })),
  repeater_entry: ({ id }) => sent.find((e) => e.id === id),
  clear_repeater_history: () => null,
  project_info: () => ({ name: 'demo', path: '/tmp/demo.intip' }),
  blank_project: () => ({ name: 'scratch', path: '' }),
  save_project_as: ({ path }) => (savedAs.push(path), { name: 'demo', path: '/tmp/demo.cegat', rawSize: 1048576, compressedSize: 131072, percent: 88, summary: '1.0 MB -> 128.0 KB (88% smaller)' }),
  default_project_dir: () => '/Users/me/Documents/Ngintip',
  path_exists: ({ path }) => existing.has(path),
  save_project: () => ({ name: 'demo', path: '/tmp/demo.cegat', rawSize: 1048576, compressedSize: 131072, percent: 88, summary: '1.0 MB -> 128.0 KB (88% smaller)' }),
  get_config: () => ({ auth: { enabled: false, username: '', password: '' }, upstream: '', invisible: false, autoContentLength: true, hostOverrides: [] }),
  set_auto_content_length: () => null,
  get_scope: () => ['*.example.com'],
  set_scope: ({ patterns }) => patterns,
  get_rules: () => [{ enabled: true, target: 'requestHeader', match: '', replace: 'X-Test: 1', comment: '' }],
  get_setting: () => null,
  set_setting: () => null,
  set_invisible: () => null,
  mcp_status: () => ({ running: false, port: 8082, docsPath: '/docs' }),
  set_mcp: ({ enabled, port, docsPath }) => ({ running: enabled, port: port || 8082, docsPath: docsPath || '/docs' }),
  browser_status: () => ({ path: '', exists: false, flags: '', canDownload: true }),
  set_browser: ({ path, flags }) => ({ path, exists: !!path, flags, canDownload: true }),
  detect_browser: () => ({ path: '/usr/bin/google-chrome', exists: true, flags: '', canDownload: true }),
  download_browser: () => ({ path: '/tmp/browser/chrome', exists: true, flags: '', canDownload: true }),
  launch_browser: () => null,
  close_launched_browsers: () => null,
  browser_running: () => false,
  list_flows: () => flows,
  logs: () => [],
  clear_logs: () => null,
  intercept_status: () => ({ enabled: true, responses: false, pending: [] }),
  list_listeners: () => mockListeners,
  start_listeners: () => (mockListeners = mockListeners.map((l) => (l.enabled ? { ...l, running: true } : l))),
  stop_listeners: () => (mockListeners = mockListeners.map((l) => ({ ...l, running: false }))),
  add_listener: ({ host, port }) => (mockListeners = [...mockListeners, { id: `l${mockListeners.length + 1}`, host, port, enabled: true, running: true }]),
  remove_listener: ({ id }) => (mockListeners = mockListeners.filter((l) => l.id !== id)),
  set_listener_enabled: ({ id, enabled }) => (mockListeners = mockListeners.map((l) => (l.id === id ? { ...l, enabled, running: enabled } : l))),
  update_listener: ({ id, host, port }) => (mockListeners = mockListeners.map((l) => (l.id === id ? { ...l, host, port } : l))),
  set_host_overrides: () => null,
  organizer_state: () => structuredClone(org),
  organizer_op: ({ action }) => orgApply(action),
  get_flow: ({ id }) => ({
    summary: flows.find((f) => f.id === id),
    requestRaw: `GET /v1/users?id=1 HTTP/2\nHost: api.example.com\nAccept: */*\n\n`,
    responseRaw:
      id === 3
        ? 'HTTP/1.1 101 Switching Protocols\nUpgrade: websocket\n\n'
        : id === 2
        ? 'HTTP/2 200 OK\nContent-Type: application/json\nSet-Cookie: sid=abc123\nAccess-Control-Allow-Origin: *\n\n{"token":"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c","name":"a"}'
        : 'HTTP/2 200 OK\nContent-Type: application/json\n\n{"id":1,"name":"a"}',
    responseIsImage: false,
    requestIsBinary: false,
    responseIsBinary: false,
    requestTruncatedView: id === 1,
    responseTruncatedView: false,
    // Flow 2 was edited in Intercept, so it keeps an Original request variant.
    variants: id === 2 ? { reqOriginal: 'GET /v1/login-ORIGINAL HTTP/2\nHost: api.example.com\n\n' } : {},
  }),
  flow_body: ({ id, side }) => `FULL ${side} body for ${id} ` + 'x'.repeat(50),
  flow_image: () => null,
  flow_hex: () => '00000000  61 62 63  |abc|',
  ws_messages: () => [
    { id: 1, flowId: 3, ts: 10, direction: 'client', opcode: 1, length: 11, text: '{"hi":true}' },
    { id: 2, flowId: 3, ts: 11, direction: 'server', opcode: 2, length: 2, text: '00000000  00 01' },
  ],
  search_flows: () => [2],
  ca_info: () => ({ path: '/tmp/ca.pem', pem: '-----BEGIN CERTIFICATE-----' }),
  usage_stats: () => ({ memBytes: 134217728, totalMemBytes: 17179869184, cpuPercent: 12.5, cpuCount: 8, uptimeSecs: 3725 }),
  crash_info: () => ({ dir: '/tmp/ngintip/crashes', count: 0, latest: null }),
  extensions_status: () => ({ enabled: false, python: 'python3', venv: '/tmp/demo_venv_extension', venvReady: false, extensions: [] }),
  detect_python: () => 'python3',
  set_python_path: ({ path }) => ({ enabled: false, python: path || 'python3', venv: '/tmp/demo_venv_extension', venvReady: false, extensions: [] }),
  set_extensions: ({ extensions }) => ({ enabled: false, python: 'python3', venv: '/tmp/demo_venv_extension', venvReady: false, extensions: (extensions || []).map((e) => ({ ...e, running: false, error: null })) }),
  set_extensions_enabled: ({ enabled }) => ({ enabled, python: 'python3', venv: '/tmp/demo_venv_extension', venvReady: false, extensions: [] }),
  setup_extension_venv: () => 'venv created\npip install ok\n',
  set_extension_venv_path: ({ path }) => ({ enabled: false, python: 'python3', venv: path || '/tmp/demo_venv_extension', venvReady: false, extensions: [] }),
  install_extension_package: ({ packages }) => `installed ${packages}`,
  extension_packages: () => ['pycryptodome==3.23.0'],
  extension_template: () => 'def process_request(req):\n    return None\n',
  test_extension: ({ id, phase }) => ({ output: `TRANSFORMED ${phase} by ${id}`, changed: true, logs: ['ran'], error: null }),
  extension_logs: () => [],
  clear_extension_logs: () => null,
  list_interfaces: () => [
    { name: 'All interfaces', ip: '0.0.0.0' },
    { name: 'Loopback', ip: '127.0.0.1' },
    { name: 'en0', ip: '192.168.1.5' },
  ],
};

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    calls.push(cmd);
    if (!(cmd in backend)) return null;
    return backend[cmd](args ?? {});
  }),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name, fn) => {
    listeners[name] = fn;
    return () => delete listeners[name];
  }),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
const { open: openDialog, save: saveDialog } = await import('@tauri-apps/plugin-dialog');

const { default: App } = await import('./App.svelte');
const { store } = await import('./lib/store.svelte.js');

async function settle() {
  for (let i = 0; i < 10; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}
const text = () => document.body.textContent;
const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => [...document.querySelectorAll(sel)];
const click = async (el) => {
  el.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  await settle();
};
const buttonWith = (label) => $$('button').find((b) => b.textContent.trim().startsWith(label));
const buttonLabeled = (label) => $$('button').find((b) => b.getAttribute('aria-label') === label);
// The Yes/No warning shown before anything destructive (ConfirmDialog).
const dialog = () => $('[role="alertdialog"]');
const answer = async (yes) => {
  expect(dialog()).toBeTruthy();
  await click(dialog().querySelector(yes ? '.yes' : '.no'));
  expect(dialog()).toBeNull();
};

let app;
const errors = [];
beforeAll(async () => {
  // jsdom lacks these; the UI only needs them to exist.
  // Flow events are batched per animation frame; run frames right away here.
  window.requestAnimationFrame = (fn) => setTimeout(fn, 0);
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
  Object.defineProperty(navigator, 'clipboard', { value: { writeText: vi.fn().mockResolvedValue(undefined) }, configurable: true });
  window.addEventListener('error', (e) => errors.push(e.error));
  app = mount(App, { target: document.body });
  await settle();
  // Leave the startup project chooser.
  const cont = [...document.querySelectorAll('.chooser button')].find((b) => b.textContent.includes('Continue'));
  if (cont) await click(cont);
});
afterAll(() => unmount(app));

describe('UI smoke test', () => {
  it('loads History with flows, project and LAN warning', () => {
    expect($$('.table .row:not(.head)').length).toBe(3);
    expect(text()).toContain('api.example.com');
    expect(text()).toContain('demo');
    expect(text()).toContain('LAN · no auth');
  });

  it('toggles History columns from the Advanced panel (persisted per project)', async () => {
    expect($('.table .row.head').textContent).not.toContain('Query');
    await click(buttonWith('Advanced'));
    const colChip = () => $$('.adv .fchip').find((b) => b.textContent.trim() === 'Query');
    await click(colChip());
    expect(store.historyColumns).toContain('query');
    expect(calls).toContain('set_setting');
    expect($('.table .row.head').textContent).toContain('Query');
    // Turn it back off so later tests see the default layout.
    await click(colChip());
    expect(store.historyColumns).not.toContain('query');
    await click(buttonWith('Advanced')); // close the panel
    await settle();
  });

  it('shows the (non-default) Modified column with a check for edited flows', async () => {
    expect(store.historyColumns).not.toContain('modified'); // off by default
    await click(buttonWith('Advanced'));
    const chip = () => $$('.adv .fchip').find((b) => b.textContent.trim() === 'Modified');
    expect(chip()).toBeTruthy();
    await click(chip());
    expect(store.historyColumns).toContain('modified');
    await click(buttonWith('Advanced'));
    await settle();
    // Flow #2 was modified (intercept/extension) -> a check; flow #1 was not.
    const row = (path) => $$('.table .row:not(.head)').find((r) => r.textContent.includes(path));
    expect(row('/v1/login').querySelector('.flag.yes')).toBeTruthy();
    expect(row('/v1/users').querySelector('.flag.yes')).toBeNull();
    // Turn it back off so later tests see the default layout.
    await click(buttonWith('Advanced'));
    await click(chip());
    await click(buttonWith('Advanced'));
    await settle();
  });

  it('filters History with the Advanced panel', async () => {
    expect($$('.table .row:not(.head)').length).toBe(3);
    await click(buttonWith('Advanced'));
    expect($('.adv')).toBeTruthy();
    // Keep only POST requests: flow #2 survives.
    const post = $$('.adv .fchip').find((b) => b.textContent.trim() === 'POST');
    await click(post);
    expect(store.adv.methods).toContain('POST');
    expect($$('.table .row:not(.head)').length).toBe(1);
    // The button shows the active dot.
    expect(buttonWith('Advanced').textContent).toContain('•');
    // Clear restores all rows.
    await click(buttonWith('Clear filters'));
    expect($$('.table .row:not(.head)').length).toBe(3);
    await click(buttonWith('Advanced')); // close the panel
    await settle();
  });

  it('shows request/response detail with Pretty mode', async () => {
    await click($$('.table .row:not(.head)')[0]);
    expect(text()).toContain('Accept: */*');
    // Detail actions live in the row right-click menu, not the pane header.
    const request = $$('.msgs .panel')[0];
    const reqButtons = [...request.querySelectorAll('button')].map((b) => b.textContent.trim());
    expect(reqButtons).not.toContain('Copy as cURL');
    expect(reqButtons).not.toContain('Capture');
    const response = $$('.msgs .panel')[1];
    await click([...response.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Pretty'));
    expect(response.querySelector('pre.code').textContent).toContain('"name": "a"');
  });

  it('shows the Original/Final version switcher for an edited flow (item 6)', async () => {
    store.view = 'history';
    await settle();
    const row2 = $$('.table .row:not(.head)').find((r) => r.textContent.includes('/v1/login'));
    await click(row2);
    const request = $$('.msgs .panel')[0];
    const tabs = () => [...request.querySelectorAll('.modes.variants .mode')].map((b) => b.textContent.trim());
    expect(tabs()).toEqual(['Final', 'Original']);
    await click([...request.querySelectorAll('.modes.variants .mode')].find((b) => b.textContent.trim() === 'Original'));
    expect(request.querySelector('pre.code').textContent).toContain('/v1/login-ORIGINAL');
    // A flow with no variants shows no switcher.
    const row1 = $$('.table .row:not(.head)').find((r) => r.textContent.includes('/v1/users'));
    await click(row1);
    expect($$('.msgs .panel')[0].querySelector('.modes.variants')).toBeNull();
  });

  it('closes the request/response detail with the X button', async () => {
    await click($$('.table .row:not(.head)')[0]);
    expect(store.selectedId).not.toBeNull();
    expect($('.close-detail')).toBeTruthy();
    await click($('.close-detail'));
    expect(store.selectedId).toBeNull();
    // The detail collapses to its placeholder (no meta row / close button).
    expect($('.close-detail')).toBeNull();
  });

  it('expands a truncated body with Show all and searches it', async () => {
    await click($$('.table .row:not(.head)')[0]);
    const req = $$('.msgs .panel')[0];
    const showAll = [...req.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Show all');
    expect(showAll).toBeTruthy();
    await click(showAll);
    expect(req.querySelector('pre.code').textContent).toContain('FULL request body');

    const search = [...req.querySelectorAll('button')].find((b) => b.getAttribute('aria-label') === 'Search');
    await click(search);
    const input = req.querySelector('.find-input');
    expect(input).toBeTruthy();
    input.value = 'body';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect(req.querySelector('.find-count').textContent).toMatch(/\/\s*[1-9]/);
    expect(req.querySelector('mark.mk')).toBeTruthy();
  });

  it('opening a passive finding highlights its match in the right pane', async () => {
    // Flow #2's response contains "sid=abc123" (Set-Cookie). Jump to it as a finding.
    store.showFinding(2, { where: 'response', match: 'sid=abc123' });
    await settle();
    expect(store.view).toBe('history');
    const res = $$('.msgs .panel')[1]; // response pane
    const input = res.querySelector('.find-input');
    expect(input).toBeTruthy();
    expect(input.value).toBe('sid=abc123');
    expect(res.querySelector('mark.mk')).toBeTruthy();
    // Close the find bar for later tests.
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    store.selectedId = null;
    await settle();
  });

  it('history opens request/response on Pretty by default; a picked Raw sticks', async () => {
    store.view = 'history';
    await settle();
    const row = (path) => $$('.table .row:not(.head)').find((r) => r.textContent.includes(path));
    const res = () => $$('.msgs .panel')[1];
    const active = () => res().querySelector('.mode.active').textContent.trim();
    await click(row('/v1/users'));
    await settle();
    // The JSON response opens formatted.
    expect(active()).toBe('Pretty');
    expect(res().querySelector('pre.code').textContent).toContain('"name": "a"');
    // Picking Raw is respected, also for the next request opened.
    await click([...res().querySelectorAll('.mode')].find((b) => b.textContent.trim() === 'Raw'));
    expect(active()).toBe('Raw');
    expect(res().querySelector('pre.code').textContent).toContain('{"id":1,"name":"a"}');
    await click(row('/v1/login'));
    await settle();
    expect(active()).toBe('Raw');
    store.selectedId = null;
    await settle();
  });

  it('defers a binary response body behind a Load button (no auto-render)', async () => {
    store.view = 'history';
    await settle();
    const orig = backend.get_flow;
    const savedFlows = store.flows;
    // A fresh flow id (never cached) so our binary detail is actually fetched.
    backend.get_flow = ({ id }) =>
      id === 100
        ? {
            summary: store.flows.find((f) => f.id === 100),
            requestRaw: 'GET /logo.png HTTP/2\nHost: api.example.com\n\n',
            responseRaw: 'HTTP/2 200 OK\nContent-Type: image/png\nContent-Length: 10\n\nBINARYDATA',
            responseIsImage: true,
            requestIsBinary: false,
            responseIsBinary: true,
            requestTruncatedView: false,
            responseTruncatedView: false,
            variants: {},
          }
        : orig({ id });
    listeners.flow({ payload: { id: 100, ts: 9, method: 'GET', scheme: 'https', host: 'api.example.com', path: '/logo.png', status: 200, length: 10, reqLength: 0, mime: 'image/png', durationMs: 2, error: null, note: '', color: '', streaming: false } });
    await settle();
    const hexCalls = calls.filter((c) => c === 'flow_hex').length;
    await click($$('.table .row:not(.head)').find((r) => r.textContent.includes('/logo.png')));
    await settle();
    const res = $$('.msgs .panel')[1];
    // History defaults to Pretty, but that default never pulls a binary body's hex dump.
    expect(res.querySelector('.mode.active').textContent.trim()).toBe('Raw');
    expect(calls.filter((c) => c === 'flow_hex').length).toBe(hexCalls);
    // The body isn't rendered; a placeholder with load actions is shown instead.
    expect(res.querySelector('.load-body')).toBeTruthy();
    expect(res.textContent).toContain('Binary body');
    expect(res.textContent).not.toContain('BINARYDATA');
    // "Load raw" reveals the body on demand.
    const loadRaw = [...res.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Load raw');
    expect(loadRaw).toBeTruthy();
    await click(loadRaw);
    expect(res.querySelector('.load-body')).toBeNull();
    expect(res.textContent).toContain('BINARYDATA');
    // Restore.
    backend.get_flow = orig;
    store.flows = savedFlows;
    store.selectedId = null;
    await settle();
  });

  it('previews a huge request fast and expands with Show all / Show less', async () => {
    store.view = 'history';
    await settle();
    const orig = backend.get_flow;
    const savedFlows = store.flows;
    const bigBody = 'A'.repeat(80_000) + 'ENDMARKER';
    backend.get_flow = ({ id }) =>
      id === 101
        ? {
            summary: store.flows.find((f) => f.id === 101),
            requestRaw: `POST /big HTTP/2\nHost: api.example.com\n\n${bigBody}`,
            responseRaw: 'HTTP/2 200 OK\n\nok',
            responseIsImage: false,
            requestIsBinary: false,
            responseIsBinary: false,
            requestTruncatedView: false,
            responseTruncatedView: false,
            variants: {},
          }
        : orig({ id });
    listeners.flow({ payload: { id: 101, ts: 9, method: 'POST', scheme: 'https', host: 'api.example.com', path: '/big', status: 200, length: 2, reqLength: 80_009, mime: '', durationMs: 2, error: null, note: '', color: '', streaming: false } });
    await settle();
    await click($$('.table .row:not(.head)').find((r) => r.textContent.includes('/big')));
    await settle();
    const req = $$('.msgs .panel')[0];
    // Opens on a preview only (not the whole body), with a Show all button.
    expect(req.querySelector('pre.code').textContent).toContain('preview');
    expect(req.textContent).not.toContain('ENDMARKER');
    const showAll = [...req.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Show all');
    expect(showAll).toBeTruthy();
    await click(showAll);
    expect(req.textContent).toContain('ENDMARKER'); // full body now rendered
    // Show less collapses back to the fast preview.
    await click([...req.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Show less'));
    expect(req.textContent).not.toContain('ENDMARKER');
    backend.get_flow = orig;
    store.flows = savedFlows;
    store.selectedId = null;
    await settle();
  });

  it('finds text in the Repeater request editor (Ctrl+F) and has a bottom Find bar', async () => {
    store.view = 'repeater';
    await settle();
    const reqPanel = $$('.repeater .grid .panel')[0];
    const ta = reqPanel.querySelector('textarea.editor');
    expect(ta).toBeTruthy();
    ta.value = 'GET /findme HTTP/1.1\nHost: x\n\nfindme-body';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    // Ctrl+F while the editor holds focus opens the find bar for that pane.
    ta.focus();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, bubbles: true }));
    await settle();
    const input = reqPanel.querySelector('.find-input');
    expect(input).toBeTruthy();
    input.value = 'findme';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect(reqPanel.querySelector('.find-count').textContent).toMatch(/\/\s*[1-9]/);
    expect(reqPanel.querySelector('.layer mark.mk')).toBeTruthy();
    // A Find button lives on the bottom bar too.
    expect([...$$('.repeater .foot button')].some((b) => b.textContent.includes('Find'))).toBe(true);
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    store.view = 'history';
    await settle();
  });

  it('Repeater request pane has a right-click menu (send / copy)', async () => {
    store.view = 'repeater';
    await settle();
    const t = store.repeater.tabs.find((x) => x.id === store.repeater.activeId);
    t.raw = 'GET /ctx HTTP/1.1\nHost: example.com\n\n';
    t.target = 'https://example.com';
    await settle();
    const reqPanel = $$('.repeater .grid .panel')[0];
    reqPanel.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 30, clientY: 30 }));
    await settle();
    const labels = $$('.menu .item').map((b) => b.textContent.trim());
    expect(labels).toContain('Send to Intruder');
    expect(labels).toContain('Copy as cURL');
    expect(labels).toContain('Copy URL');
    const copyUrl = $$('.menu .item').find((b) => b.textContent.trim() === 'Copy URL');
    await click(copyUrl);
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('https://example.com/ctx');
    store.view = 'history';
    await settle();
  });

  it('shows a Cancel button for an in-flight Repeater send', async () => {
    store.view = 'repeater';
    await settle();
    const t = store.repeater.tabs.find((x) => x.id === store.repeater.activeId);
    t.sending = true; // simulate a slow send in flight
    await settle();
    const cancel = $('.cancel-send');
    expect(cancel).toBeTruthy();
    await click(cancel);
    expect(calls).toContain('cancel_send');
    t.sending = false;
    await settle();
    expect($('.cancel-send')).toBeNull();
    store.view = 'history';
    await settle();
  });

  it('Ctrl/Cmd+R duplicates the active Repeater tab', async () => {
    store.view = 'repeater';
    await settle();
    const beforeIds = store.repeater.tabs.map((t) => t.id);
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'r', ctrlKey: true, bubbles: true }));
    await settle();
    expect(store.repeater.tabs.length).toBe(beforeIds.length + 1);
    const dup = store.repeater.tabs.find((t) => !beforeIds.includes(t.id));
    expect(dup).toBeTruthy();
    // Clean up so later tests see the original tab count.
    store.closeRepeaterTab(dup.id);
    await settle();
    expect(store.repeater.tabs.length).toBe(beforeIds.length);
    store.view = 'history';
    await settle();
  });

  it('remembers collapsed Repeater groups across view switches', async () => {
    store.view = 'repeater';
    await settle();
    const t = store.repeater.tabs.find((x) => x.id === store.repeater.activeId);
    t.group = 'Auth';
    await settle();
    const label = () => $$('.group-label').find((b) => b.textContent.includes('Auth'));
    await click(label());
    expect(store.repeater.collapsedGroups).toContain('Auth');
    // Leave and return: the group stays collapsed (not auto-opened).
    store.view = 'history';
    await settle();
    store.view = 'repeater';
    await settle();
    expect(store.repeater.collapsedGroups).toContain('Auth');
    expect(label().querySelector('.caret').textContent).toBe('▸');
    // Cleanup.
    t.group = '';
    store.repeater.collapsedGroups = [];
    store.view = 'history';
    await settle();
  });

  it('toggles the color theme', async () => {
    const before = document.documentElement.getAttribute('data-theme');
    const themeBtn = [...document.querySelectorAll('.topbar button')].find((b) => b.title.startsWith('Theme:'));
    await click(themeBtn);
    expect(document.documentElement.getAttribute('data-theme')).not.toBe(before);
  });

  it('shows WebSocket messages for a 101 flow', async () => {
    await click($$('.table .row:not(.head)')[2]);
    expect(text()).toContain('2 messages');
    expect(text()).toContain('{"hi":true}');
    listeners.ws({ payload: { id: 3, flowId: 3, ts: 12, direction: 'server', opcode: 1, length: 2, text: 'ok' } });
    await settle();
    expect(text()).toContain('3 messages');
  });

  it('adds live flows and replaces streaming previews by id', async () => {
    const f = { ...flows[0], id: 4, path: '/live', streaming: true };
    listeners.flow({ payload: f });
    await settle();
    expect($$('.table .row:not(.head)').length).toBe(4);
    expect(text()).toContain('streaming');
    listeners.flow({ payload: { ...f, streaming: false, durationMs: 99 } });
    await settle();
    expect($$('.table .row:not(.head)').length).toBe(4);
    expect(text()).toContain('99 ms');
  });

  it('captures a flow from the History context menu', async () => {
    store.view = 'history';
    await settle();
    const row = $$('.table .row:not(.head)')[0];
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 15, clientY: 15 }));
    await settle();
    const cap = [...document.querySelectorAll('.menu .item')].find((b) => b.textContent.includes('Capture'));
    expect(cap).toBeTruthy();
    await click(cap);
    expect($('.overlay')).toBeTruthy();
    await click([...document.querySelectorAll('.overlay button')].find((b) => b.textContent.trim() === 'Close'));
    expect($('.overlay')).toBeNull();
  });

  it('capture of the open flow follows its Pretty view', async () => {
    store.view = 'history';
    await settle();
    await click($$('.table .row:not(.head)')[0]);
    const response = $$('.msgs .panel')[1];
    await click([...response.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Pretty'));
    await settle();
    // Capture the currently-viewed flow via the pane right-click (live = true, so it
    // uses each pane's chosen view). This exercises the Pretty display-text path.
    $('.msgs-panes').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 20, clientY: 20 }));
    await settle();
    await click([...document.querySelectorAll('.menu .item')].find((b) => b.textContent.includes('Capture')));
    expect($('.overlay')).toBeTruthy();
    await click([...document.querySelectorAll('.overlay button')].find((b) => b.textContent.trim() === 'Close'));
    expect($('.overlay')).toBeNull();
  });

  it('opens the row context menu with all actions', async () => {
    const row = $$('.table .row:not(.head)')[1];
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 10, clientY: 10 }));
    await settle();
    // The requested actions are all present.
    for (const label of ['Send to Repeater', 'Send to Intruder', 'Copy full URL', 'Copy as cURL', 'Copy request body', 'Copy response body', 'Capture image']) {
      expect($$('.menu .item').some((b) => b.textContent.includes(label))).toBe(true);
    }
    // "Copy response body" fetches the body and copies it.
    await click($$('.menu .item').find((b) => b.textContent.includes('Copy response body')));
    expect(calls).toContain('flow_body');
    document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
    await settle();
    expect($('.menu')).toBeNull();
  });

  it('Cmd/Ctrl+R sends the selected request to Repeater', async () => {
    store.view = 'history';
    await settle();
    await click($$('.table .row:not(.head)')[0]);
    expect(store.selectedId).not.toBeNull();
    const before = store.repeater.tabs.length;
    const ev = new KeyboardEvent('keydown', { key: 'r', metaKey: true, bubbles: true, cancelable: true });
    window.dispatchEvent(ev);
    await settle();
    expect(store.repeater.tabs.length).toBe(before + 1);
    expect(ev.defaultPrevented).toBe(true); // doesn't reload the app
    // Clean up the added tab so later Repeater tests start from one tab.
    store.closeRepeaterTab(store.repeater.tabs.at(-1).id);
    await settle();
  });

  it('adds a host to scope from the Settings sitemap', async () => {
    store.view = 'settings';
    store.settingsSection = 'scope';
    await settle();
    const box = $('.sitemap-box');
    expect(box).toBeTruthy();
    const name = [...box.querySelectorAll('.name')].find((n) => n.textContent.includes('api.example.com'));
    expect(name).toBeTruthy();
    await click(name);
    await settle();
    // Clicking a host node appends it to the scope textarea (ready to Save).
    expect($('textarea.scope').value).toContain('api.example.com');
  });

  it('one search box matches summary fields and bodies together', async () => {
    store.view = 'history';
    await settle();
    // No separate body-search button anymore.
    expect($$('.history .toolbar button').some((b) => b.textContent.includes('Search body'))).toBe(false);
    // A term only in a body (mock search_flows -> [2]) still narrows the table.
    store.filter = 'secret';
    store.searchFilter(); // debounced body search + immediate summary match
    await store.searchBodies(); // force the body search for the test
    await settle();
    expect(store.bodyHits?.has(2)).toBe(true);
    expect($$('.table .row:not(.head)').length).toBe(1);
    store.filter = '';
    store.searchFilter();
    await settle();
  });

  it('shows held requests and responses in Intercept', async () => {
    store.view = 'intercept';
    listeners.intercept({ payload: { key: 1, id: 9, phase: 'request', method: 'GET', scheme: 'https', host: 'a.com', path: '/x', status: null, raw: 'GET /x HTTP/1.1\nHost: a.com\n\n' } });
    listeners.intercept({ payload: { key: 2, id: 8, phase: 'response', method: 'GET', scheme: 'https', host: 'a.com', path: '/y', status: 500, raw: 'HTTP/1.1 500 Oops\n\n' } });
    await settle();
    expect(text()).toContain('REQ');
    expect(text()).toContain('RES');
    expect($('.count')?.textContent).toBe('2');
    await click(buttonWith('Forward + response'));
    expect(calls).toContain('intercept_forward');
    expect(store.intercept.queue.length).toBe(1);
  });

  it('intercept: colored phase tag and right-click send actions', async () => {
    store.view = 'intercept';
    listeners.intercept({ payload: { key: 11, id: 21, phase: 'request', method: 'POST', scheme: 'https', host: 'b.com', path: '/login', status: null, raw: 'POST /login HTTP/1.1\nHost: b.com\n\n{"u":1}' } });
    await settle();
    const target = [...document.querySelectorAll('.intercept .queue .item')].find((el) => el.textContent.includes('/login'));
    expect(target).toBeTruthy();
    // Right-click -> selects it and opens the send menu (item 3).
    target.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 30, clientY: 30 }));
    await settle();
    // The phase is labelled with a colored tag (item 2).
    expect($('.tag.req')?.textContent).toContain('REQUEST');
    const labels = [...document.querySelectorAll('.menu .item')].map((b) => b.textContent);
    for (const l of ['Send to Repeater', 'Send to Intruder', 'Send to Decoder']) {
      expect(labels.some((t) => t.includes(l))).toBe(true);
    }
    // The shared "+ Header" / "+ Body" dropdowns exist in the editor toolbar (item 1).
    expect(buttonWith('+ Header ▾')).toBeTruthy();
    expect(buttonWith('+ Body ▾')).toBeTruthy();
  });

  it('intercept: advanced filter modal pushes rules to the proxy (item 4)', async () => {
    store.view = 'intercept';
    await settle();
    await click(buttonWith('Filter'));
    await settle();
    await click([...document.querySelectorAll('.modal button')].find((b) => b.textContent.includes('Add rule')));
    await settle();
    const val = document.querySelector('.modal .frule .val');
    expect(val).toBeTruthy();
    val.value = 'admin';
    val.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(calls).toContain('set_intercept_filter');
    expect(store.intercept.filter.rules.length).toBe(1);
    // Clean up so later tests see an empty filter.
    await click([...document.querySelectorAll('.modal button')].find((b) => b.textContent.includes('Clear all')));
    await settle();
    expect(store.intercept.filter.rules.length).toBe(0);
  });

  it('edits and sends in Repeater', async () => {
    store.view = 'repeater';
    await settle();
    expect($('textarea')).not.toBeNull();
    await click(buttonLabeled('Duplicate tab'));
    expect(store.repeater.tabs.length).toBe(2);
  });

  it('edits the Repeater request freely (two-way binding)', async () => {
    store.view = 'repeater';
    await settle();
    const ta = document.querySelector('.repeater textarea.editor');
    expect(ta).toBeTruthy();
    // The whole message must be editable: request line, headers, and body.
    const edited = 'PATCH /free HTTP/1.1\nHost: x\nX-Custom: edited\nContent-Type: application/json\n\n{"body":true}';
    ta.value = edited;
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    expect(tab.raw).toBe(edited);
    // The colour layer under the editor highlights the method and header names.
    const method = document.querySelector('.repeater .layer .hl-method');
    expect(method?.textContent).toBe('PATCH');
    expect(method.className).toContain('m-patch');
    expect([...document.querySelectorAll('.repeater .layer .hl-hname')].some((s) => s.textContent === 'X-Custom')).toBe(true);
  });

  it('adds smart headers from the + Header menu (add and replace)', async () => {
    store.view = 'repeater';
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    tab.raw = 'GET /h HTTP/1.1\nHost: x\n\n';
    await settle();
    // Open the dropdown and add X-Forwarded-For (not present -> added).
    await click(buttonWith('+ Header'));
    await click($$('.menu .item').find((b) => b.textContent.includes('X-Forwarded-For')));
    expect(tab.raw).toContain('X-Forwarded-For: 127.0.0.1');
    // Random User-Agent (not present -> added), then replace it with a new one.
    await click(buttonWith('+ Header'));
    await click($$('.menu .item').find((b) => b.textContent.trim().startsWith('Random User-Agent')));
    const first = tab.raw.match(/^User-Agent: (.+)$/m)?.[1];
    expect(first).toBeTruthy();
    expect((tab.raw.match(/User-Agent:/g) || []).length).toBe(1);
    // Content-Length is part of the same menu now.
    tab.raw = 'POST /h HTTP/1.1\nHost: x\n\nhello';
    await settle();
    await click(buttonWith('+ Header'));
    await click($$('.menu .item').find((b) => b.textContent.includes('Content-Length')));
    expect(tab.raw).toContain('Content-Length: 5');
  });

  it('inserts a body skeleton from the + Body menu', async () => {
    store.view = 'repeater';
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    tab.raw = 'POST /b HTTP/1.1\nHost: x\n\n';
    await settle();
    await click(buttonWith('+ Body'));
    await click($$('.menu .item').find((b) => b.textContent.trim() === 'JSON'));
    expect(tab.raw).toContain('Content-Type: application/json');
    expect(tab.raw).toContain('"key": "value"');
    expect(tab.raw).toMatch(/Content-Length: \d+/);
  });

  it('steps through Repeater history and back to unsent edits', async () => {
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    const nav = () => $('.history-nav');
    const buttons = () => [...nav().querySelectorAll('button')];

    tab.raw = 'GET /first HTTP/1.1\nHost: example.com\n\n';
    await settle();
    await click(buttonWith('Send'));
    tab.raw = 'GET /second HTTP/1.1\nHost: example.com\n\n';
    await settle();
    await click(buttonWith('Send'));
    // After sending we stay in live editing mode; the counter shows the newest (N/N).
    expect(nav().textContent).toContain('2/2');
    expect(text()).toContain('sent: GET /second');
    // ms/size shown on the Repeater bottom bar.
    expect($('.repeater .foot .foot-stat')?.textContent).toMatch(/ms/);

    tab.raw = 'GET /draft HTTP/1.1\n\n';
    await settle();
    await click(buttons()[0]); // back -> newest sent entry
    expect(nav().textContent).toContain('2/2');
    expect(tab.raw).toContain('/second');
    await click(buttons()[0]); // back -> oldest
    expect(nav().textContent).toContain('1/2');
    expect(tab.raw).toContain('/first');
    expect(buttons()[0].disabled).toBe(true);

    await click(buttons()[1]); // forward -> /second
    expect(tab.raw).toContain('/second');
    await click(buttons()[1]); // forward -> back to the unsent draft
    expect(tab.raw).toContain('/draft');
    expect(nav().textContent).toContain('2/2');
    expect(buttons()[1].disabled).toBe(true);
  });

  it('shows the latest response again after a tab is reloaded (reopened project)', async () => {
    store.view = 'repeater';
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    tab.raw = 'GET /reload-latest HTTP/1.1\nHost: example.com\n\n';
    await settle();
    await click(buttonWith('Send'));
    expect(tab.response?.responseRaw).toContain('sent: GET /reload-latest');

    // Reopening a project keeps the request text but not the response (not persisted).
    tab.response = null;
    tab.error = null;
    tab.pos = null;
    tab.historyLoaded = false;
    await store.loadRepeaterHistory(tab);
    // The latest history response is shown without clicking the back arrow.
    expect(tab.response?.responseRaw).toContain('sent: GET /reload-latest');
    expect(tab.pos).toBe(null); // still the live edit, just with the latest response filled in
  });

  it('adds and clears Intruder payload positions', async () => {
    store.view = 'intruder';
    await settle();
    const t = store.intruder.tabs.find((x) => x.id === store.intruder.activeId);
    t.raw = 'GET /u?id=1 HTTP/1.1\nHost: h\n\n';
    await settle();
    // Select the "1" in the request editor, then Add position.
    const ta = document.querySelector('.intruder .req textarea.editor');
    ta.setSelectionRange(10, 11);
    await click(buttonWith('Add position'));
    expect(t.raw).toBe('GET /u?id=\u00a71\u00a7 HTTP/1.1\nHost: h\n\n');
    // Clear removes the markers.
    await click(buttonWith('Clear'));
    expect(t.raw).toBe('GET /u?id=1 HTTP/1.1\nHost: h\n\n');
  });

  it('runs an Intruder attack and shows a result', async () => {
    store.view = 'intruder';
    await settle();
    const t = store.intruder.tabs.find((x) => x.id === store.intruder.activeId);
    t.raw = `GET /u?id=\u00a7x\u00a7 HTTP/1.1\nHost: h\n\n`;
    t.sets = ['a\nb\nc'];
    t.concurrency = 2;
    await settle();
    await click(buttonWith('Start attack'));
    for (let i = 0; i < 60 && t.running; i++) await settle();
    expect(t.results.length).toBe(3);
    expect(t.results.every((r) => r.status === 200)).toBe(true);
    const row = document.querySelector('.results .rows .row:not(.head)');
    await click(row);
    // Both the sent request and the response are shown for the selected row.
    expect(text()).toContain('Request #');
    expect(text()).toContain('Response #');
    expect(t.results[0].requestRaw).toContain('GET /u?id=');
  });

  it('groups a Repeater tab via right-click and New group', async () => {
    store.view = 'repeater';
    await settle();
    const t = document.querySelector('.tabs .tab');
    t.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 20, clientY: 20 }));
    await settle();
    const newGroup = [...document.querySelectorAll('.menu .item')].find((b) => b.textContent.includes('New group'));
    expect(newGroup).toBeTruthy();
    await click(newGroup);
    const input = document.querySelector('.modal input');
    input.value = 'AuthFlow';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    await click([...document.querySelectorAll('.modal button')].find((b) => b.textContent.trim() === 'Create'));
    expect([...document.querySelectorAll('.tabs .group-label')].some((el) => el.textContent.includes('AuthFlow'))).toBe(true);
  });

  it('sends a whole Repeater group (sequential and parallel)', async () => {
    store.view = 'repeater';
    await settle();
    const rep = store.repeater;
    while (rep.tabs.length < 2) {
      store.addRepeaterTab();
      await settle();
    }
    const [t1, t2] = rep.tabs;
    t1.group = 'grp';
    t2.group = 'grp';
    t1.raw = 'GET /g1 HTTP/1.1\nHost: x\n\n';
    t2.raw = 'GET /g2 HTTP/1.1\nHost: x\n\n';
    await settle();

    // The group's context menu offers both send types.
    const label = $$('.group-label').find((el) => el.textContent.includes('grp'));
    expect(label).toBeTruthy();
    label.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 30, clientY: 30 }));
    await settle();
    expect($$('.menu .item').some((b) => b.textContent.includes('Send all sequentially'))).toBe(true);
    await click($$('.menu .item').find((b) => b.textContent.includes('Send all in parallel')));
    await settle();
    expect(t1.response).toBeTruthy();
    expect(t2.response).toBeTruthy();

    // Sequential also sends every tab.
    t1.response = null;
    t2.response = null;
    await store.sendGroup('grp', 'sequential');
    expect(t1.response && t2.response).toBeTruthy();
  });

  it('the Send button offers group modes on a grouped tab', async () => {
    store.view = 'repeater';
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    tab.group = 'grp';
    await settle();
    const caret = $('.send-caret');
    expect(caret).toBeTruthy();
    await click(caret);
    expect($$('.menu .item').some((b) => b.textContent.includes('Send multi'))).toBe(true);
    document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
    await settle();

    // The ▶ on the group label also opens the send-mode menu.
    const gsend = $('.group-send');
    expect(gsend).toBeTruthy();
    await click(gsend);
    expect($$('.menu .item').some((b) => b.textContent.includes('Sync send'))).toBe(true);
    expect($$('.menu .item').some((b) => b.textContent.includes('Send multi'))).toBe(true);
    document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
    await settle();
  });

  it('reflects the target into the Host header (one way)', async () => {
    store.view = 'repeater';
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    tab.group = '';
    tab.raw = 'GET / HTTP/1.1\nHost: old.example.com\n\n';
    tab.target = 'https://old.example.com';
    await settle();
    // Editing the target updates the Host header.
    const input = document.querySelector('.repeater .target');
    input.value = 'https://api.new.com:8443';
    input.dispatchEvent(new Event('input', { bubbles: true })); // bind:value updates target
    input.dispatchEvent(new Event('change', { bubbles: true })); // commit -> reflect Host
    await settle();
    expect(tab.raw).toContain('Host: api.new.com:8443');
    expect(tab.raw).not.toContain('old.example.com');
    // Editing the Host header does NOT change the target.
    tab.raw = tab.raw.replace('Host: api.new.com:8443', 'Host: spoofed.internal');
    await settle();
    expect(tab.target).toBe('https://api.new.com:8443');
  });

  it('follows a redirect in Repeater', async () => {
    store.view = 'repeater';
    await settle();
    const tab = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    tab.group = '';
    tab.target = 'https://example.com';
    tab.raw = 'GET /redirect HTTP/1.1\nHost: example.com\n\n';
    await settle();
    await click(buttonWith('Send'));
    await settle();
    expect(tab.response.status).toBe(302);
    const follow = buttonWith('Follow redirect');
    expect(follow).toBeTruthy();
    await click(follow);
    await settle();
    // The next request targets the Location (/after) and was sent.
    expect(tab.raw.split('\n')[0]).toBe('GET /after HTTP/1.1');
  });

  it('opens the Repeater snapshot editor', async () => {
    store.view = 'repeater';
    await settle();
    await click(buttonLabeled('Capture screenshot'));
    expect($('.overlay')).toBeTruthy();
    // Snapshot editor: Mark / Redact tools in the toolbar.
    const tools = [...document.querySelectorAll('.overlay .bar .tbtn')].map((b) => b.textContent.trim()).join(' ');
    expect(tools).toContain('Mark');
    expect(tools).toContain('Redact');
    // Snapshot options + export controls are present.
    const t = text();
    for (const s of ['Theme', 'Layout', 'Watermark', 'Redaction', 'Export scale', 'Save PNG', 'Copy to clipboard']) expect(t).toContain(s);
    // Escape closes the editor.
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    expect($('.overlay')).toBeNull();
  });

  it('snapshot editor: opens on Select, right-click is its own menu, right-click never places a mark', async () => {
    store.view = 'repeater';
    await settle();
    await click(buttonLabeled('Capture screenshot'));
    const canvas = $('.overlay canvas');
    const toolBtn = (name) => [...document.querySelectorAll('.overlay .bar .tbtn')].find((b) => b.textContent.trim().endsWith(name) || b.getAttribute('aria-label') === name);
    const marksHead = () => $('.overlay .marks-head').textContent.trim();
    // Neutral by default: the Select arrow is active, not Mark.
    expect(toolBtn('Select').classList.contains('active')).toBe(true);
    expect(toolBtn('Mark').classList.contains('active')).toBe(false);

    // Right-click on the backdrop: the editor's menu, not the browser's.
    const ev = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 1, clientY: 1 });
    canvas.dispatchEvent(ev);
    await settle();
    expect(ev.defaultPrevented).toBe(true);
    const labels = () => $$('.menu .item').map((b) => b.textContent.trim());
    for (const l of ['Undo', 'Redo', 'Tool', 'Clear all annotations', 'Zoom to fit', 'Copy image', 'Save PNG…']) {
      expect(labels().some((t) => t.startsWith(l))).toBe(true);
    }
    expect($$('.menu .item').find((b) => b.textContent.trim().startsWith('Undo')).disabled).toBe(true);
    // Escape closes only the menu; the editor stays open.
    $('.menu').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    expect($('.menu')).toBeNull();
    expect($('.overlay')).toBeTruthy();

    // Tool submenu switches to Mark.
    canvas.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 1, clientY: 1 }));
    await settle();
    const toolParent = $$('.menu .item.parent').find((b) => b.textContent.includes('Tool'));
    toolParent.dispatchEvent(new PointerEvent('pointerenter'));
    await settle();
    await click([...document.querySelectorAll('.menu.sub .item')].find((b) => b.textContent.trim().startsWith('Mark')));
    expect(toolBtn('Mark').classList.contains('active')).toBe(true);

    // Sweep the canvas: a right button press never places a mark, a left click does.
    const sweep = (button) => {
      for (let x = 1; x < 120; x += 4) {
        for (let y = 1; y < 80; y += 3) canvas.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button, clientX: x, clientY: y }));
      }
    };
    sweep(2);
    await settle();
    expect(marksHead()).toBe('Marks');
    sweep(0);
    await settle();
    expect(marksHead()).toMatch(/^Marks \(\d+\)$/);

    // ...and right-click Undo takes the last mark back.
    const before = Number(marksHead().match(/\d+/)[0]);
    canvas.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 1, clientY: 1 }));
    await settle();
    await click($$('.menu .item').find((b) => b.textContent.trim().startsWith('Undo')));
    expect(marksHead()).toBe(before > 1 ? `Marks (${before - 1})` : 'Marks');

    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    expect($('.overlay')).toBeNull();
  });

  it('decodes in Decoder with a multi-step recipe', async () => {
    store.view = 'decoder';
    store.decoder.recipe = [];
    await settle();
    // One step: Decode > Base64.
    store.decoder.input = 'aGVsbG8=';
    await settle();
    const decodeBtn = (i) => $$('.ops .group')[0].querySelectorAll('button')[i];
    await click(decodeBtn(0)); // Base64
    await settle();
    expect(store.decoder.output).toBe('hello');

    // Chain: double URL-decode "%2541" -> "%41" -> "A".
    store.decoder.recipe = [];
    store.decoder.input = '%2541';
    await settle();
    await click(decodeBtn(1)); // URL
    await click(decodeBtn(1)); // URL again
    await settle();
    expect(store.decoder.recipe.length).toBe(2);
    expect(store.decoder.output).toBe('A');

    // Removing a step recomputes the output.
    await click($$('.steps .step')[1].querySelector('.del'));
    await settle();
    expect(store.decoder.output).toBe('%41');
    store.decoder.recipe = [];
  });

  it('History right-click sends to Comparer, which diffs the two items', async () => {
    store.clearComparer();
    // The History flow menu offers Send request/response to Comparer.
    store.view = 'history';
    await settle();
    await click($$('.table .row:not(.head)')[0]);
    const panes = $('.msgs-panes');
    panes.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 20, clientY: 20 }));
    await settle();
    const labels = $$('.menu .item').map((b) => b.textContent);
    expect(labels.some((l) => l.includes('Send request to Comparer'))).toBe(true);
    expect(labels.some((l) => l.includes('Send response to Comparer'))).toBe(true);
    document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
    await settle();

    // Two differing items -> the Comparer shows a diff with added/removed rows.
    store.addToComparer('GET /a HTTP/1.1\nHost: x\n\nbody one', 'A');
    store.addToComparer('GET /b HTTP/1.1\nHost: x\n\nbody two', 'B');
    expect(store.comparer.items.length).toBe(2);
    store.view = 'comparer';
    await settle();
    expect($$('.item').length).toBe(2);
    expect($$('.drow.add').length + $$('.drow.del').length).toBeGreaterThan(0);

    // Removing an item clears its diff pick.
    await click($('.item .rm'));
    await settle();
    expect(store.comparer.items.length).toBe(1);
    store.clearComparer();
    store.view = 'history';
    await settle();
  });

  it('Comparer takes manual input and match options (case sensitivity)', async () => {
    store.clearComparer();
    store.view = 'comparer';
    await settle();
    // Add two items manually (each opens an editable textarea) and type into them.
    await click(buttonWith('+ Add'));
    await click(buttonWith('+ Add'));
    const areas = $$('.iedit');
    expect(areas.length).toBe(2);
    const type = (el, v) => { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); };
    type(areas[0], 'Hello\nworld');
    type(areas[1], 'hello\nworld');
    await settle();
    expect(store.comparer.items.length).toBe(2);
    // Case sensitive (default): the first line differs.
    expect($$('.drow.del').length + $$('.drow.add').length).toBeGreaterThan(0);
    // Turn off "Case sensitive": the two become identical.
    const cs = $$('.opt').find((l) => l.textContent.includes('Case sensitive')).querySelector('input');
    cs.click();
    await settle();
    expect($$('.drow.del').length + $$('.drow.add').length).toBe(0);
    store.clearComparer();
    store.view = 'history';
    await settle();
  });

  it('shows the in-app guide', async () => {
    store.view = 'help';
    await settle();
    expect(text()).toContain('Ngintip guide');
    expect(text()).toContain('full-text index');
    expect(text()).toContain('Cluster bomb');
    expect(text()).toContain('Keyboard shortcuts');
  });

  it('renders Settings as sub-tabs', async () => {
    store.view = 'settings';
    await settle();
    // Every section is listed in the left nav.
    const navText = $('.sub-nav').textContent;
    for (const label of ['Project', 'Auto Saved', 'Scope', 'Match & Replace', 'Proxy listener', 'MCP server', 'Browser Engine', 'CA certificate']) {
      expect(navText).toContain(label);
    }
    // Upstream, Authentication and Sending are merged into Proxy listener; Usage into
    // Project; the Test server feature was removed. None appear as their own nav item.
    for (const gone of ['Upstream proxy', 'Authentication', 'Sending', 'Usage', 'Test server']) {
      expect(navText).not.toContain(gone);
    }
    const navItem = (label) => $$('.sub-nav-item').find((b) => b.textContent.trim().startsWith(label));

    // Project shows by default; other sections are not mounted until selected.
    expect(text()).toContain('One');
    expect(text()).not.toContain('The proxy is reachable from the network');

    // Proxy listener section.
    await click(navItem('Proxy listener'));
    expect(text()).toContain('The proxy is reachable from the network');
    expect(text()).toContain('Invisible proxy');

    // Match & Replace section.
    await click(navItem('Match & Replace'));
    await click(buttonWith('+ Add rule'));
    expect($$('.rule').length).toBe(2);

    // Auto Saved section: on by default (hourly); changing it persists to the project.
    await click(navItem('Auto Saved'));
    const autoToggle = $$('input[type=checkbox]').find((c) => c.closest('label')?.textContent.includes('Enable auto-save'));
    expect(autoToggle.checked).toBe(true);
    const interval = $$('.card input[type=number]').find((i) => i.closest('label')?.textContent.includes('Every'));
    interval.value = '15';
    interval.dispatchEvent(new Event('change', { bubbles: true }));
    expect(store.autoSave.interval).toBe(15);
    expect(calls).toContain('set_setting');
    await click(autoToggle);
    expect(store.autoSave.enabled).toBe(false);

    // Sending (now merged into Proxy listener): the auto Content-Length toggle drives the command.
    await click(navItem('Proxy listener'));
    const autoCl = $$('input[type=checkbox]').find((c) => c.closest('label')?.textContent.includes('Auto add / recompute Content-Length'));
    expect(autoCl.checked).toBe(true);
    await click(autoCl);
    expect(calls).toContain('set_auto_content_length');
    expect(store.config.autoContentLength).toBe(false);

    // MCP section: toggling enables the server.
    await click(navItem('MCP server'));
    const mcpToggle = $$('input[type=checkbox]').find((c) => c.closest('label')?.textContent.includes('Enable MCP server'));
    expect(mcpToggle.checked).toBe(false);
    await click(mcpToggle);
    expect(calls).toContain('set_mcp');
    expect(store.mcp.running).toBe(true);

    // CA section.
    await click(navItem('CA certificate'));
    await click(buttonWith('Windows'));
    expect(text()).toContain('certutil -user -addstore Root');
    // "Open in browser" opens the CA download page via the backend.
    await click(buttonWith('Open in browser'));
    expect(calls).toContain('open_url');

    // Usage (now merged into Project): polls process/system stats and shows crash-dump info.
    await click(navItem('Project'));
    await settle();
    expect(calls).toContain('usage_stats');
    expect(calls).toContain('crash_info');
    expect(text()).toContain('Memory (this app)');
    expect(text()).toContain('128 MB'); // 134217728 bytes formatted
    expect(text()).toContain('Crash dumps');
  });

  it('extensions view: interpreter, scripts + scope + test, venv packages (item 5)', async () => {
    store.view = 'extensions';
    await settle();
    expect(text()).toContain('Python interpreter');
    expect(text()).toContain('Scripts');
    // Master on/off drives the backend.
    await click($('.ext .master input'));
    expect(calls).toContain('set_extensions_enabled');
    // Detect interpreter.
    await click(buttonWith('Detect'));
    expect(calls).toContain('detect_python');
    // Add a script (file dialog is mocked; drive the store directly).
    await store.addExtension({ path: '/tmp/decrypt.py', name: 'decrypt' });
    await settle();
    expect(calls).toContain('set_extensions');
    expect(store.extensions.list.some((e) => e.name === 'decrypt')).toBe(true);
    expect(text()).toContain('/tmp/decrypt.py');
    // Scope surface toggles (Proxy / Intercept / Repeater) -> set_extensions.
    const surfBtns = $$('.ext .trow .scope .surf');
    expect(surfBtns.length).toBe(3);
    // A new extension defaults to all three surfaces on.
    expect(surfBtns.every((b) => b.classList.contains('on'))).toBe(true);
    // Turning Proxy off narrows the scope to the other two surfaces.
    await click(surfBtns[0]);
    await settle();
    expect(store.extensions.list[0].scope).toBe('interceptor,repeater');
    // Test panel: open, run, see transformed output.
    await click(buttonWith('Test'));
    await click(buttonWith('Run test'));
    await settle();
    expect(calls).toContain('test_extension');
    expect(text()).toContain('TRANSFORMED request');
    // Venv: install a package + build.
    const pkg = $$('.ext input').find((i) => i.placeholder && i.placeholder.includes('package'));
    pkg.value = 'pycryptodome';
    pkg.dispatchEvent(new Event('input', { bubbles: true }));
    await click(buttonWith('Install'));
    await settle();
    expect(calls).toContain('install_extension_package');
    await click(buttonWith('Build venv'));
    expect(calls).toContain('setup_extension_venv');
    expect(calls).toContain('extension_packages');
  });

  it('shows process logs and clears the unread badge', async () => {
    listeners.log({ payload: { id: 1, ts: Date.now(), level: 'error', source: 'tls', message: 'handshake failed (pinning?)' } });
    await settle();
    expect(store.logsUnread).toBe(1);
    store.view = 'logs';
    await settle();
    expect(text()).toContain('handshake failed (pinning?)');
    expect(store.logsUnread).toBe(0);
  });

  it('deletes log lines individually and by selection', async () => {
    store.logs = [
      { id: 11, ts: Date.now(), level: 'error', source: 'tls', message: 'line one' },
      { id: 12, ts: Date.now(), level: 'warn', source: 'proxy', message: 'line two' },
      { id: 13, ts: Date.now(), level: 'info', source: 'proxy', message: 'line three' },
    ];
    store.view = 'logs';
    await settle();
    const rowFor = (msg) => $$('.logs .row').find((r) => r.textContent.includes(msg));
    // Delete one line via its × button.
    await click(rowFor('line one').querySelector('.del'));
    expect(calls).toContain('delete_logs');
    expect(store.logs.some((l) => l.id === 11)).toBe(false);
    // Select the rest and delete them.
    for (const l of [...store.logs]) rowFor(l.message).querySelector('.rowcheck').dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    await click(buttonWith('Delete ('));
    await answer(true);
    expect(store.logs.length).toBe(0);
  });

  it('pages logs: newest on top, latest page only, Retrieve all reveals the rest', async () => {
    // 250 lines (id ascending = oldest -> newest, like the store).
    store.logs = Array.from({ length: 250 }, (_, i) => ({ id: i + 1, ts: Date.now(), level: 'info', source: 'proxy', message: `line ${i + 1}` }));
    store.view = 'logs';
    await settle();
    const rows = () => $$('.logs .row');
    // Only the latest 200 render, and the newest line is at the top.
    expect(rows().length).toBe(200);
    expect(rows()[0].textContent).toContain('line 250');
    expect(rows()[0].textContent).not.toContain('line 1 ');
    // The oldest 50 are hidden behind "Retrieve all".
    const retrieve = buttonWith('Retrieve all');
    expect(retrieve).toBeTruthy();
    await click(retrieve);
    expect(rows().length).toBe(250);
    expect(buttonWith('Retrieve all')).toBeFalsy();
    store.logs = [];
  });

  it('opens a log detail modal and closes it with Esc', async () => {
    store.logs = [{ id: 21, ts: Date.now(), level: 'error', source: 'tls', message: 'detailed handshake failure with a long explanation' }];
    store.view = 'logs';
    await settle();
    const row = $$('.logs .row').find((r) => r.textContent.includes('detailed handshake'));
    await click(row);
    expect($('[role="dialog"]')).toBeTruthy();
    expect($('.dmsg').textContent).toContain('detailed handshake failure');
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    await settle();
    expect($('[role="dialog"]')).toBeFalsy();
    store.logs = [];
  });

  it('native File menu events trigger project actions', async () => {
    saveDialog.mockClear();
    // build_menu (lib.rs) emits a `menu` event; the store maps it to an action.
    listeners.menu({ payload: 'export_har' });
    await settle();
    expect(saveDialog).toHaveBeenCalled();
  });

  it('Project menu "Save As" writes a compressed .cegat and reports the size drop', async () => {
    saveDialog.mockClear();
    saveDialog.mockResolvedValueOnce('/tmp/demo.cegat');
    listeners.menu({ payload: 'save_as' });
    await settle();
    expect(saveDialog).toHaveBeenCalled();
    expect(calls).toContain('save_project_as');
    expect(store.notice?.text).toContain('88% smaller');
  });

  it('Project menu "Save" writes to the current document', async () => {
    listeners.menu({ payload: 'save' });
    await settle();
    expect(calls).toContain('save_project');
    expect(store.notice?.text).toContain('Saved demo');
  });

  it('shows an elapsed-time counter in the saving overlay', async () => {
    store.saving = true;
    await settle();
    const timer = $('.decompress-timer');
    expect(timer).toBeTruthy();
    expect(timer.textContent).toMatch(/\d+\.\d\s*s/); // e.g. "0.0s", counting up
    store.saving = false;
    await settle();
    expect($('.decompress-timer')).toBeNull();
  });

  it('browser CDP: off by default, toggled + port saved per project', async () => {
    // Default off, sane default port.
    expect(store.cdp.enabled).toBe(false);
    // Store-level: toggling persists (set_setting) and clamps bad ports.
    calls.length = 0;
    store.setCdp({ enabled: true, port: 9333 });
    expect(store.cdp).toEqual({ enabled: true, port: 9333 });
    expect(calls).toContain('set_setting');
    store.setCdp({ port: 0 }); // invalid -> falls back to the default
    expect(store.cdp.port).toBe(9222);
    store.setCdp({ enabled: false, port: 9222 });

    // UI: the Browser Engine section has the checkbox + port, wired to the store.
    store.settingsSection = 'browser';
    store.view = 'settings';
    await settle();
    const cb = $('.cdp-row input[type="checkbox"]');
    expect(cb).toBeTruthy();
    expect(cb.checked).toBe(false);
    cb.click();
    await settle();
    expect(store.cdp.enabled).toBe(true);
    const port = $('.cdp-port .port');
    port.value = '9444';
    port.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(store.cdp.port).toBe(9444);
    store.setCdp({ enabled: false, port: 9222 });
    store.view = 'history';
    await settle();
  });

  it('passively scans captured traffic for findings', async () => {
    store.view = 'passive';
    await settle();
    // Start clean (a background auto-scan may have run already).
    store.clearPassive();
    await settle();
    expect(store.passiveFindings.length).toBe(0);
    await click(buttonWith('Scan history'));
    await settle();
    await settle();
    // Flow #2 exposes a JWT, a permissive CORS header and a flagless cookie.
    expect(store.passiveFindings.length).toBeGreaterThanOrEqual(3);
    expect(store.passiveFindings.some((f) => f.type === 'jwt')).toBe(true);
    expect(store.passiveAlerts).toBeGreaterThan(0);
    // Findings are grouped by source; that group's findings show under one header.
    expect($$('.group').length).toBeGreaterThanOrEqual(1);
    expect($$('.gitem').length).toBeGreaterThanOrEqual(3);
    // Results are persisted to the project (settings).
    expect(calls).toContain('set_setting');
    // Clicking a group opens a detail modal (it does NOT navigate away).
    await click($('.ghead'));
    expect($('[role="dialog"]')).toBeTruthy();
    expect(store.view).toBe('passive');
    expect($('.dmatch')).toBeTruthy();
    // Esc closes the modal.
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    await settle();
    expect($('[role="dialog"]')).toBeFalsy();
    // The modal's explicit action is what jumps to History.
    await click($('.ghead'));
    await click(buttonWith('Open request in History'));
    expect(store.view).toBe('history');
    // Each match has "Lookup to Source" -> that exact flow, with the match highlighted.
    store.view = 'passive';
    await settle();
    await click($('.ghead'));
    await click(buttonWith('Lookup to Source'));
    expect(store.view).toBe('history');
    await settle();
    const resPane = $$('.msgs .panel')[1];
    expect(resPane.querySelector('.find-input')).toBeTruthy();
    expect(resPane.querySelector('mark.mk')).toBeTruthy();
    resPane.querySelector('.find-input')?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    // Clearing removes the findings.
    store.view = 'passive';
    await settle();
    await click(buttonWith('Clear'));
    await answer(true);
    expect(store.passiveFindings.length).toBe(0);
  });

  it('expands/collapses finding groups and persists the state', async () => {
    store.view = 'passive';
    await settle();
    store.clearPassive();
    await settle();
    await click(buttonWith('Scan history'));
    await settle();
    await settle();
    expect($$('.group').length).toBeGreaterThanOrEqual(1);
    const itemsBefore = $$('.gitem').length;
    expect(itemsBefore).toBeGreaterThanOrEqual(3);

    // Collapsing one group via its caret hides that group's items and persists.
    const firstGroupItems = $$('.group')[0].querySelectorAll('.gitem').length;
    calls.length = 0;
    await click($('.group .caret'));
    expect($$('.gitem').length).toBe(itemsBefore - firstGroupItems);
    expect(store.passiveCollapsed.length).toBe(1);
    expect(calls).toContain('set_setting');
    // Toggling the same caret expands it again (items come back).
    await click($('.group .caret'));
    expect($$('.gitem').length).toBe(itemsBefore);
    expect(store.passiveCollapsed.length).toBe(0);

    // "Collapse all" hides everything; the button flips to "Expand all" and restores.
    await click(buttonWith('Collapse all'));
    expect($$('.gitem').length).toBe(0);
    await click(buttonWith('Expand all'));
    expect($$('.gitem').length).toBe(itemsBefore);
    expect(store.passiveCollapsed.length).toBe(0);

    store.clearPassive();
    await settle();
  });

  it('selects findings to copy and delete', async () => {
    store.view = 'passive';
    await settle();
    store.clearPassive();
    await settle();
    await click(buttonWith('Scan history'));
    await settle();
    await settle();
    const before = store.passiveFindings.length;
    expect(before).toBeGreaterThanOrEqual(3);

    // Enter select mode: per-finding + per-group checkboxes appear.
    await click(buttonWith('Select'));
    expect($$('.gitem .icheck').length).toBeGreaterThanOrEqual(1);
    // Select everything, then copy.
    await click(buttonWith('All'));
    navigator.clipboard.writeText.mockClear();
    await click(buttonWith('Copy ('));
    expect(navigator.clipboard.writeText).toHaveBeenCalledTimes(1);
    expect(navigator.clipboard.writeText.mock.calls[0][0]).toContain('\t');

    // Delete one group's findings via its header checkbox.
    await click(buttonWith('None'));
    const firstGroupCount = $$('.group')[0].querySelectorAll('.gitem').length;
    await click($('.group .gcheck'));
    await click(buttonWith('Delete ('));
    await answer(true);
    expect(store.passiveFindings.length).toBe(before - firstGroupCount);
    // Deletions persist to the project.
    expect(calls).toContain('set_setting');

    store.clearPassive();
    await settle();
  });

  it('a global Clear filters brings hidden rows back', async () => {
    store.view = 'history';
    store.clearAllFilters();
    await settle();
    const rows = () => $$('.table .row:not(.head)').length;
    const total = store.flows.length;
    expect(rows()).toBe(total);
    // A search that matches nothing hides every row.
    store.filter = 'zzz-nomatch-zzz';
    await settle();
    expect(rows()).toBe(0);
    expect(store.filtersActive).toBe(true);
    expect(text()).toContain('No requests match the current filters');
    // The empty-state button resets all filters.
    await click($$('.empty button').find((b) => b.textContent.includes('Clear all filters')));
    expect(store.filter).toBe('');
    await settle();
    expect(rows()).toBe(total);
  });

  it('excludes a response type via a Filters rule', async () => {
    store.view = 'history';
    store.clearAllFilters();
    await settle();
    // Inject an image response that a MIME filter should be able to hide.
    listeners.flow({ payload: { id: 99, ts: 9, method: 'GET', scheme: 'https', host: 'cdn.example.com', path: '/logo.png', status: 200, length: 500, reqLength: 0, mime: 'image/png', durationMs: 3, error: null, note: '', color: '', streaming: false } });
    await settle();
    const rows = () => $$('.table .row:not(.head)').length;
    const before = rows();
    expect(before).toBeGreaterThanOrEqual(4);
    // Exclude images by MIME (hide-types folded into the include/exclude filters).
    store.adv.filters = [{ field: 'mime', mode: 'exclude', value: 'image', sensitive: false }];
    await settle();
    expect(rows()).toBe(before - 1);
    // Cleanup.
    store.flows = store.flows.filter((f) => f.id !== 99);
    store.resetAdvanced();
    await settle();
  });

  it('adds a custom rule and toggles a built-in rule from the Rules panel', async () => {
    store.view = 'passive';
    await settle();
    await click(buttonWith('Rules'));
    // Built-in rules are listed with on/off checkboxes.
    const builtinRows = $$('.rule').filter((el) => el.textContent.includes('built-in'));
    expect(builtinRows.length).toBe(store.builtinRules.length);
    // Turn the first built-in off.
    const firstId = store.builtinRules[0].id;
    await click(builtinRows[0].querySelector('input'));
    expect(store.ruleEnabled(firstId)).toBe(false);
    expect(store.effectiveRules().some((r) => r.id === firstId)).toBe(false);
    await click(builtinRows[0].querySelector('input')); // back on
    expect(store.ruleEnabled(firstId)).toBe(true);
    // Add a custom rule (per project).
    const pat = $('.rule-add .pat');
    pat.value = 'CUSTOM-[0-9]{3}';
    pat.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    await click(buttonWith('Add rule'));
    expect(store.passiveRules.length).toBe(1);
    expect(store.passiveRules[0].pattern).toBe('CUSTOM-[0-9]{3}');
    expect(calls).toContain('set_setting');
    // Deleting it (× button) empties the list.
    await click($$('.rule .del').at(-1));
    expect(store.passiveRules.length).toBe(0);
  });

  it('configures the passive scan scope from the Rules panel', async () => {
    store.view = 'passive';
    await settle();
    if (!$('.scope')) await click(buttonWith('Rules'));
    await settle();
    const scopeBtn = (label) => $$('.scope-add button').find((b) => b.textContent.trim() === label);
    const chips = () => $$('.scope .chip').map((c) => c.textContent.replace('×', '').trim());
    // Default scope is the static-asset set.
    expect(chips()).toContain('js');
    expect(chips()).toContain('json');
    // Add an extension (leading dot is stripped).
    const ext = $('.scope .ext');
    ext.value = '.pdf';
    ext.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    await click(scopeBtn('Add'));
    expect(store.passiveScope).toContain('pdf');
    expect(calls).toContain('set_setting');
    // Remove a chip via its × button.
    await click($$('.scope .chip').find((c) => c.textContent.includes('pdf')).querySelector('.chipx'));
    expect(store.passiveScope).not.toContain('pdf');
    // "All (*)" collapses to a single all-chip.
    await click(scopeBtn('All (*)'));
    expect(store.passiveScope).toEqual(['*']);
    expect($('.scope .chip.all')).toBeTruthy();
    // Reset restores the static default.
    await click(scopeBtn('Reset'));
    expect(store.passiveScope).toContain('js');
    expect(store.passiveScope).not.toContain('*');
  });

  it('project menu offers New/Close with a save-first modal', async () => {
    store.view = 'history';
    await settle();
    // Open the project dropdown.
    await click($('.project'));
    const items = () => $$('.menu .mi').map((b) => b.textContent.trim());
    expect(items()).toEqual(expect.arrayContaining(['Save', 'Save as…', 'Close project', 'New project']));

    // Close project → modal → "Close without saving" returns to the chooser.
    await click($$('.menu .mi').find((b) => b.textContent.trim() === 'Close project'));
    expect($('[role="dialog"]')).toBeTruthy();
    await click(buttonWith('Close without saving'));
    expect(store.chooser).toBe(true);
    store.chooser = false; // restore for the remaining assertions
    await settle();

    // New project → modal → "Save & continue" saves, then asks for the new name.
    await click($('.project'));
    await click($$('.menu .mi').find((b) => b.textContent.trim() === 'New project'));
    expect($('[role="dialog"]')).toBeTruthy();
    calls.length = 0;
    await click(buttonWith('Save & continue'));
    await settle();
    expect(calls).toContain('save_project');
    expect(calls).not.toContain('blank_project');
    expect($('#np-name')).toBeTruthy();
    $('#np-name').value = 'next one';
    $('#np-name').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    $('form.np').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await settle();
    expect(calls).toContain('blank_project');
    expect(savedAs.at(-1)).toMatch(/\/next_one\.ngintip$/);
    expect(store.chooser).toBe(false);
  });

  it('onboarding "New project" asks for a name and saves it as name_with_underscores.ngintip', async () => {
    // Back to the onboarding chooser; no folder used before, so the default one is offered.
    store.chooser = true;
    store.project = { name: '', path: '' };
    localStorage.removeItem('ngintip.projectDir');
    await settle();
    saveDialog.mockClear();
    calls.length = 0;
    await click($$('.chooser button').find((b) => b.textContent.includes('New project')));
    const name = $('#np-name');
    expect(name).toBeTruthy();
    expect(document.activeElement).toBe(name);
    expect($('.np .dir').textContent).toBe('/Users/me/Documents/Ngintip');
    // Nothing typed: no file name yet, Create is off.
    const create = () => buttonWith('Create project');
    expect(create().disabled).toBe(true);
    expect($('.np .file').textContent).toContain('Type a name or press Random');

    name.value = 'project pentest x';
    name.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect($('.np .file').textContent).toBe('project_pentest_x.ngintip');
    expect(create().disabled).toBe(false);
    await click(create());
    // It must create a blank project and WRITE it (not open a non-existent file),
    // without the native save dialog, then advance to the main view.
    expect(saveDialog).not.toHaveBeenCalled();
    expect(calls).toContain('blank_project');
    expect(calls).not.toContain('open_project');
    expect(savedAs.at(-1)).toBe('/Users/me/Documents/Ngintip/project_pentest_x.ngintip');
    expect($('#np-name')).toBeNull();
    expect(store.chooser).toBe(false);
    expect(store.project.path).toBe('/tmp/demo.cegat');
  });

  it('New project: Random gives a dated name, Change… picks the folder, an existing file asks first', async () => {
    store.chooser = true;
    await settle();
    await click($$('.chooser button').find((b) => b.textContent.includes('New project')));
    // The folder of the last saved project is offered next time.
    expect($('.np .dir').textContent).toBe('/tmp');

    await click(buttonWith('Random'));
    expect($('#np-name').value).toMatch(/^\d{2}-\d{2}-\d{4}-[a-z0-9]{6}$/);
    expect($('.np .file').textContent).toBe(`${$('#np-name').value}.ngintip`);

    openDialog.mockResolvedValueOnce('/work/clients');
    await click(buttonWith('Change…'));
    expect(openDialog).toHaveBeenLastCalledWith(expect.objectContaining({ directory: true }));
    expect($('.np .dir').textContent).toBe('/work/clients');

    // That name is taken there: Replace? No keeps the dialog open for another name.
    const n = savedAs.length;
    $('#np-name').value = 'taken';
    $('#np-name').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    existing.add('/work/clients/taken.ngintip');
    await click(buttonWith('Create project'));
    expect(dialog().textContent).toContain('Replace "taken.ngintip"?');
    await answer(false);
    expect($('#np-name')).toBeTruthy();
    expect(savedAs.length).toBe(n);
    // Yes replaces it.
    await click(buttonWith('Create project'));
    await answer(true);
    expect(savedAs.at(-1)).toBe('/work/clients/taken.ngintip');
    expect(store.chooser).toBe(false);
    existing.clear();

    // Cancel (Escape) creates nothing.
    calls.length = 0;
    const p = store.newProject();
    await settle();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    expect(await p).toBe(false);
    await settle();
    expect($('#np-name')).toBeNull();
    expect(calls).not.toContain('blank_project');
  });

  it('right-clicks the request/response detail for the flow actions', async () => {
    store.view = 'history';
    await settle();
    await click($$('.table .row:not(.head)')[0]);
    const panes = $('.msgs-panes');
    panes.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 20, clientY: 20 }));
    await settle();
    const labels = $$('.menu .item').map((b) => b.textContent);
    expect(labels.some((l) => l.includes('Copy as cURL'))).toBe(true);
    expect(labels.some((l) => l.includes('Capture image'))).toBe(true);
    document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
    await settle();
  });

  it('edits a Repeater note (scoped) via the notes popup', async () => {
    store.view = 'repeater';
    await settle();
    await click(buttonLabeled('Note'));
    expect($('[role="dialog"]')).toBeTruthy();
    const ta = $('.note-area');
    ta.value = 'check IDOR on this endpoint';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    await click($$('.note-actions button').find((b) => b.textContent.trim() === 'Save'));
    const active = store.repeater.tabs.find((t) => t.id === store.repeater.activeId);
    expect(active.note).toBe('check IDOR on this endpoint');
    // The note is included in the persisted snapshot (saved debounced).
    expect(store.repeater.tabs.every((t) => 'note' in t)).toBe(true);
  });

  it('browser engine: routes to setup when unset, detects, and launches', async () => {
    // Unset: the History button sends the user to the Browser Engine settings.
    store.browser = { ...store.browser, exists: false, path: '' };
    store.view = 'history';
    await settle();
    await click(buttonWith('🌐 Set up browser'));
    expect(store.view).toBe('settings');
    await settle();
    expect($('.card h2')?.textContent).toBe('Browser Engine');
    // Detect fills in an installed browser.
    await click(buttonWith('Detect installed'));
    expect(calls).toContain('detect_browser');
    expect(store.browser.exists).toBe(true);
    expect(store.browser.path).toBe('/usr/bin/google-chrome');
    // Now the History button launches it, wired to the proxy.
    store.view = 'history';
    await settle();
    await click(buttonWith('🌐 Open browser'));
    expect(calls).toContain('launch_browser');
  });

  it('passive: "In scope only" toggle persists', async () => {
    store.view = 'passive';
    await settle();
    const cb = $('.inscope input');
    expect(cb).toBeTruthy();
    expect(cb.checked).toBe(false);
    cb.checked = true;
    cb.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(store.passiveInScopeOnly).toBe(true);
    expect(calls).toContain('set_setting');
    // Restore for other tests.
    cb.checked = false;
    cb.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(store.passiveInScopeOnly).toBe(false);
  });

  it('filters modal adds multiple include/exclude rules', async () => {
    store.view = 'history';
    store.clearAllFilters();
    await settle();
    const rows = () => $$('.table .row:not(.head)').length;
    expect(rows()).toBe(3);
    // Filters live inside the Advanced panel now.
    await click(buttonWith('Advanced'));
    await click(buttonWith('Include / exclude rules'));
    expect($('[role="dialog"]')).toBeTruthy();

    // The modal is draggable by its title bar.
    const head = $('[role="dialog"] .mhead');
    head.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, clientX: 100, clientY: 100 }));
    head.dispatchEvent(new MouseEvent('pointermove', { bubbles: true, clientX: 140, clientY: 130 }));
    head.dispatchEvent(new MouseEvent('pointerup', { bubbles: true }));
    await settle();
    expect($('[role="dialog"]').style.transform).toContain('translate(40px, 30px)');

    // Add a rule (defaults to exclude, field = path).
    await click(buttonWith('+ Add filter'));
    const setVal = (row, v) => {
      const input = row.querySelector('.fm-value');
      input.value = v;
      input.dispatchEvent(new Event('input', { bubbles: true }));
    };
    setVal($('.fm-row'), '/v1/');
    await settle();
    // Exclude /v1/ hides flows 1 and 2, leaving /socket.
    expect(rows()).toBe(1);
    expect(store.advFilterCount).toBe(1);

    // Each rule has a case-sensitivity toggle (Aa).
    const caseBtn = $('.fm-row .toggle-btn.case');
    expect(caseBtn).toBeTruthy();
    await click(caseBtn);
    expect(store.adv.filters[0].sensitive).toBe(true);
    await click(caseBtn); // back to insensitive
    expect(store.adv.filters[0].sensitive).toBe(false);

    // Flip the rule to include: only the two /v1/ flows.
    await click($('.fm-row .toggle-btn.mode'));
    await settle();
    expect(rows()).toBe(2);

    // Add a second (exclude) rule to drop /v1/login.
    await click(buttonWith('+ Add filter'));
    setVal($$('.fm-row')[1], '/v1/login');
    await settle();
    expect(rows()).toBe(1);

    // Disable all -> nothing filters, all 3 flows return, both rules are retained.
    await click(buttonWith('Disable all'));
    await settle();
    expect(store.adv.filters.length).toBe(2);
    expect(store.advFiltersAllOn).toBe(false);
    expect(store.advFilterCount).toBe(0);
    expect(rows()).toBe(3);

    // The control now offers to switch everything back on (include /v1/ AND exclude /v1/login => 1 row).
    await click(buttonWith('Enable all'));
    await settle();
    expect(store.advFiltersAllOn).toBe(true);
    expect(rows()).toBe(1);

    // Mute just the exclude rule via its checkbox -> /v1/login returns, rule kept.
    const mute = (row) => row.querySelector('.fm-on').dispatchEvent(new Event('change', { bubbles: true }));
    mute($$('.fm-row')[1]);
    await settle();
    expect(store.adv.filters[1].enabled).toBe(false);
    expect(store.advFilterCount).toBe(1); // only the enabled include rule counts
    expect(rows()).toBe(2);

    // Un-mute it (the checkbox toggles back on) -> 1 row again.
    mute($$('.fm-row')[1]);
    await settle();
    expect(store.adv.filters[1].enabled).toBe(true);
    expect(rows()).toBe(1);

    // Clear all rules, then close the modal.
    await click(buttonWith('Clear all'));
    await settle();
    expect(store.advFilterCount).toBe(0);
    expect(rows()).toBe(3);
    await click(buttonWith('Done'));
    await settle();
    expect($('[role="dialog"]')).toBeFalsy();
    await click(buttonWith('Advanced')); // close the panel
    await settle();
  });

  it('"Exclude static assets" fills junk-extension excludes without duplicating', async () => {
    store.view = 'history';
    store.clearAllFilters();
    await settle();
    await click(buttonWith('Advanced'));
    await click(buttonWith('Include / exclude rules'));
    expect($('[role="dialog"]')).toBeTruthy();

    await click(buttonWith('Exclude static assets'));
    await settle();
    const n = store.adv.filters.length;
    expect(n).toBeGreaterThan(10);
    expect(store.adv.filters.every((r) => r.mode === 'exclude')).toBe(true);
    // JS is excluded by exact extension, and .json is never a rule, so JSON stays visible.
    expect(store.adv.filters.some((r) => r.field === 'ext' && r.value === 'js')).toBe(true);
    expect(store.adv.filters.some((r) => r.field === 'ext' && r.value === 'png')).toBe(true);
    expect(store.adv.filters.some((r) => r.value === 'json')).toBe(false);

    // Clicking again is a no-op (dedupe), not a second copy.
    await click(buttonWith('Exclude static assets'));
    await settle();
    expect(store.adv.filters.length).toBe(n);

    await click(buttonWith('Clear all'));
    await click(buttonWith('Done'));
    await click(buttonWith('Advanced'));
    await settle();
  });

  it('opening a finding keeps History filters and still shows its detail', async () => {
    store.view = 'history';
    store.clearAllFilters();
    await settle();
    const rows = () => $$('.table .row:not(.head)').length;
    // Hide flow 2 (/v1/login) with an exclude filter.
    store.adv.filters = [{ field: 'path', mode: 'exclude', value: '/v1/login' }];
    await settle();
    expect(rows()).toBe(2);
    // Jumping to the hidden flow's finding does NOT wipe the filter; it selects the
    // flow so its request/response detail shows (driven by selectedId, not the table).
    store.showFinding(2);
    await settle();
    expect(store.view).toBe('history');
    expect(store.selectedId).toBe(2);
    expect(store.advFilterCount).toBe(1); // filter preserved
    expect(rows()).toBe(2); // row still hidden, but detail is shown below
    expect($('.close-detail')).toBeTruthy();
    // A finding whose flow was deleted just notifies.
    store.showFinding(99999);
    expect(store.notice?.error).toBe(true);
    store.clearAllFilters();
    await settle();
  });

  it('deleting a flow drops its passive findings (no ghosts)', async () => {
    const saved = store.flows;
    store.passiveFindings = [{ type: 'x', severity: 'info', title: 'X', where: 'response', match: 'm', flowId: 2, key: '2|x|response|m' }];
    await store.deleteFlows([2]);
    expect(store.passiveFindings.some((f) => f.flowId === 2)).toBe(false);
    // Restore for later tests (deleteFlows made a filtered copy; `saved` is intact).
    store.flows = saved;
    store.passiveFindings = [];
    store.selectedId = null;
  });

  it('history: Ctrl+A then Delete (or Clear) asks Yes/No first; only Yes deletes', async () => {
    const saved = store.flows;
    const n = saved.length;
    store.view = 'history';
    await settle();
    const grid = $('.viewport[role="grid"]');
    const key = (k, mod = {}) => grid.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...mod }));
    const deletes = () => calls.filter((c) => c === 'delete_flows' || c === 'clear_flows').length;
    const before = deletes();

    // The blunder: select everything, hit Delete -> a warning, nothing deleted yet.
    grid.focus();
    key('a', { ctrlKey: true });
    await settle();
    expect(store.selectedIds.size).toBe(n);
    key('Delete');
    await settle();
    expect(dialog().textContent).toContain(`Delete all ${n} requests?`);
    expect(dialog().textContent).toContain('cannot be undone');
    // "No" has the focus, so a reflexive Enter keeps the data.
    expect(document.activeElement.textContent.trim()).toBe('No');
    // More key presses while it is open reach nothing behind it (no second delete).
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
    key('Backspace');
    await settle();
    expect($$('[role="alertdialog"]').length).toBe(1);
    // Escape = No.
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await settle();
    expect(dialog()).toBeNull();
    expect(store.flows.length).toBe(n);
    expect(deletes()).toBe(before);
    // Focus is handed back to the table, so the keyboard keeps working there.
    expect(document.activeElement).toBe(grid);

    // Delete again and press No.
    key('Delete');
    await settle();
    await answer(false);
    expect(store.flows.length).toBe(n);

    // The History "Clear" button asks too.
    await click(buttonWith('Clear'));
    expect(dialog().textContent).toContain(`Clear all ${n} requests from History?`);
    await answer(false);
    expect(store.flows.length).toBe(n);
    expect(deletes()).toBe(before);

    // Row menu "Delete…" on one request: Yes deletes exactly that one.
    store.selectedIds = new Set();
    await settle();
    const row = $$('.table .row:not(.head)')[0];
    const id = store.flows.find((f) => row.textContent.includes(`${f.id}`))?.id;
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 10, clientY: 10 }));
    await settle();
    await click($$('.menu .item').find((b) => b.textContent.trim().startsWith('Delete')));
    expect(dialog().textContent).toMatch(/Delete request #\d+\?/);
    await answer(true);
    expect(deletes()).toBe(before + 1);
    expect(store.flows.length).toBe(n - 1);
    if (id != null) expect(store.flows.some((f) => f.id === id)).toBe(false);

    store.flows = saved;
    store.selectedId = null;
    store.selectedIds = new Set();
    await settle();
  });

  it('sorting newest-first keeps new flows at the top (follows the newest end)', async () => {
    // Remount the table so the sort starts at its default (# ascending).
    store.view = 'repeater';
    await settle();
    store.view = 'history';
    store.clearAllFilters();
    await settle();
    const vp = $('.table .viewport');
    expect(vp).toBeTruthy();
    // jsdom reports 0 for scroll metrics; give the viewport a real scroll size.
    Object.defineProperty(vp, 'scrollHeight', { configurable: true, value: 1000 });
    Object.defineProperty(vp, 'clientHeight', { configurable: true, value: 200 });
    // Click # once -> descending (newest first).
    const idTh = () => $$('.table .row.head .th').find((b) => b.textContent.trim().startsWith('#'));
    await click(idTh());
    await settle();
    expect(idTh().textContent).toContain('↓');
    // A new request must follow to the TOP (newest), not the bottom (oldest).
    listeners.flow({ payload: { ...flows[0], id: 7000, path: '/newest', streaming: false } });
    await settle();
    expect(vp.scrollTop).toBe(0);
    expect($$('.table .row:not(.head)')[0].querySelector('.cell').textContent.trim()).toBe('7000');
    // Cleanup the injected flow and restore the default sort.
    store.flows = store.flows.filter((f) => f.id !== 7000);
    store.historySort = { key: 'id', dir: 1 };
    await settle();
  });

  it('History sort is persisted per project and restored on reload', async () => {
    store.view = 'history';
    await settle();
    // Clicking a column header (via the store) sorts; clicking again flips it.
    store.setHistorySort('status');
    expect(store.historySort).toEqual({ key: 'status', dir: 1 });
    store.setHistorySort('status');
    expect(store.historySort).toEqual({ key: 'status', dir: -1 });
    expect(calls).toContain('set_setting');
    // Reopening a project restores the saved sort (not the default).
    const saved = backend.get_setting;
    backend.get_setting = ({ key }) => (key === 'historySort' ? { key: 'host', dir: -1 } : null);
    await store.blankProject();
    expect(store.historySort).toEqual({ key: 'host', dir: -1 });
    backend.get_setting = saved;
    store.historySort = { key: 'id', dir: 1 };
    await settle();
  });

  it('History scope + advanced filter is persisted per project and restored on reload', async () => {
    store.view = 'history';
    await settle();
    // Saving: changing the scope checkbox / an advanced field writes under historyFilter.
    const savedKeys = [];
    const origSet = backend.set_setting;
    backend.set_setting = (args) => {
      savedKeys.push(args.key);
      return null;
    };
    store.scopeOnly = true;
    store.adv = { methods: ['POST'], status: ['4'], reqBody: 'any', resBody: 'any', minMs: '', maxMs: '', filters: [] };
    await settle(); // effect runs, schedules the debounced flush
    await new Promise((r) => setTimeout(r, 450)); // let the debounce fire
    await settle();
    expect(savedKeys).toContain('historyFilter');
    backend.set_setting = origSet;

    // Loading: reopening the project restores the saved filter (not the default).
    const origGet = backend.get_setting;
    backend.get_setting = ({ key }) =>
      key === 'historyFilter'
        ? { scopeOnly: true, adv: { methods: ['GET'], status: ['4'], reqBody: 'any', resBody: 'any', minMs: '', maxMs: '', filters: [{ field: 'host', mode: 'exclude', value: 'ads', sensitive: false }] } }
        : null;
    await store.blankProject();
    await settle();
    expect(store.scopeOnly).toBe(true);
    expect(store.adv.methods).toEqual(['GET']);
    expect(store.adv.status).toEqual(['4']);
    expect(store.adv.filters[0].value).toBe('ads');
    // Restore clean state for the final assertions.
    backend.get_setting = origGet;
    store.clearAllFilters();
    await settle();
  });

  it('proxy listener table: add, disable and remove a listener', async () => {
    store.view = 'settings';
    await settle();
    const navItem = (label) => $$('.sub-nav-item').find((b) => b.textContent.trim().startsWith(label));
    await click(navItem('Proxy listener'));
    await settle();
    // The default listener renders as a row.
    const before = store.listeners.length;
    expect($$('.lrow').length).toBe(before);
    // Add a second listener via the add form (set the port, then click Add).
    const portInput = $$('.add-listener input[type=number]')[0];
    portInput.value = '9090';
    portInput.dispatchEvent(new Event('input', { bubbles: true }));
    await click(buttonWith('Add listener'));
    expect(calls).toContain('add_listener');
    expect(store.listeners.length).toBe(before + 1);
    expect($$('.lrow').length).toBe(before + 1);
    // Disable the new row (its toggle button reads "Disable" while enabled).
    const newRow = () => $$('.lrow').at(-1);
    const toggle = [...newRow().querySelectorAll('button')].find((b) => b.textContent.trim() === 'Disable');
    await click(toggle);
    expect(calls).toContain('set_listener_enabled');
    expect(store.listeners.at(-1).enabled).toBe(false);
    // Remove it.
    await click(newRow().querySelector('[aria-label="Remove listener"]'));
    expect(calls).toContain('remove_listener');
    expect(store.listeners.length).toBe(before);
  });

  it('hostname overrides: add and edit persist to the backend', async () => {
    store.view = 'settings';
    await settle();
    const navItem = (label) => $$('.sub-nav-item').find((b) => b.textContent.trim().startsWith(label));
    await click(navItem('Proxy listener'));
    await settle();
    await click(buttonWith('+ Add override'));
    expect(store.hostOverrides.length).toBe(1);
    const hostIn = $$('.orow .ohost')[0];
    hostIn.value = 'api.example.com';
    hostIn.dispatchEvent(new Event('change', { bubbles: true }));
    const ipIn = $$('.orow .oip')[0];
    ipIn.value = '127.0.0.1';
    ipIn.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(calls).toContain('set_host_overrides');
    expect(store.hostOverrides[0]).toMatchObject({ host: 'api.example.com', ip: '127.0.0.1' });
    // Remove it again (keeps later assertions clean).
    await click($$('.orow [aria-label="Remove override"]')[0]);
    await settle();
    expect(store.hostOverrides.length).toBe(0);
  });

  it('vault: flexible typed fields from a template, masking, quick copy, search and safe delete', async () => {
    store.view = 'settings';
    await settle();
    const navItem = (label) => $$('.sub-nav-item').find((b) => b.textContent.trim().startsWith(label));
    await click(navItem('Vault'));
    await settle();
    expect(text()).toContain('No credentials yet');
    expect(text()).not.toContain('unencrypted'); // no plaintext banner any more
    // Start from the "Email login" template: email + password + URL.
    await click($$('.vault .tcard').find((b) => b.textContent.includes('Email login')));
    const e = () => store.vault.entries[0];
    expect(store.vault.entries.length).toBe(1);
    expect(e().fields.map((f) => f.kind)).toEqual(['email', 'password', 'url']);
    expect(calls).toContain('set_setting');
    const typeInto = (el, value) => {
      el.value = value;
      el.dispatchEvent(new Event('input', { bubbles: true }));
    };
    const row = (name) => $$('.vault .field').find((r) => r.querySelector('.fname').value === name);
    const val = (name) => row(name).querySelector('.fval');
    const menuOn = async (el) => {
      el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 20, clientY: 20 }));
      await settle();
    };
    const pick = (label) => click($$('.menu .item').find((b) => b.textContent.trim().startsWith(label)));
    typeInto($('.vault .dtitle .title'), 'Admin staging');
    typeInto($('.vault .dtitle .role-in'), 'admin');
    typeInto(val('Email'), 'admin@target.test');
    typeInto(val('Password'), 's3cret');
    await settle();
    expect(e()).toMatchObject({ label: 'Admin staging', role: 'admin' });
    expect(e().fields.find((f) => f.kind === 'email').value).toBe('admin@target.test');
    // Secrets are masked until revealed; the list never shows them.
    expect(val('Password').type).toBe('password');
    await click(buttonLabeled('Show Password'));
    expect(val('Password').type).toBe('text');
    const item = () => $('.vault .vitem');
    expect(item().textContent).toContain('Admin staging');
    expect(item().textContent).toContain('admin@target.test');
    expect(item().textContent).not.toContain('s3cret');
    // Add any field: an API key is a masked secret by default; rename it inline.
    await click($$('.vault .chip').find((b) => b.textContent.trim() === '+ API key'));
    expect(e().fields.map((f) => f.kind)).toEqual(['email', 'password', 'url', 'apikey']);
    typeInto(val('API key'), 'sk_live_123');
    await settle();
    expect(val('API key').type).toBe('password');
    typeInto(row('API key').querySelector('.fname'), 'Stripe key');
    await settle();
    expect(e().fields[3]).toMatchObject({ name: 'Stripe key', value: 'sk_live_123', secret: true });
    // Field menu: show a secret as plain text, change a type, remove a field.
    await menuOn(row('Stripe key'));
    await pick('Show as plain text');
    expect(e().fields[3].secret).toBe(false);
    await menuOn(row('Stripe key'));
    await click($$('.menu .item').find((b) => b.textContent.trim().startsWith('Type')));
    await click($$('.menu.sub .item').find((b) => b.textContent.trim().startsWith('Token')));
    expect(e().fields[3]).toMatchObject({ kind: 'token', name: 'Stripe key', secret: true }); // a custom name is kept
    await menuOn(row('URL'));
    await pick('Remove field');
    expect(e().fields.map((f) => f.kind)).toEqual(['email', 'password', 'token']);
    // Ready-made headers: Basic from email + password, Bearer from the token.
    navigator.clipboard.writeText.mockClear();
    await click(buttonWith('Copy Basic auth'));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(`Authorization: Basic ${btoa('admin@target.test:s3cret')}`);
    await click(buttonWith('Copy Bearer'));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('Authorization: Bearer sk_live_123');
    // Search: the role matches; a secret value never does.
    const search = $('.vault .search input');
    typeInto(search, 's3cret');
    await settle();
    expect($$('.vault .vitem').length).toBe(0);
    typeInto(search, 'ADMIN');
    await settle();
    expect($$('.vault .vitem').length).toBe(1);
    typeInto(search, '');
    await settle();
    // Right-click the credential: copy any field from the nested Copy menu (masked there).
    await menuOn(item());
    await click($('.menu .item.parent'));
    expect($('.menu.sub').textContent).toContain('Password');
    expect($('.menu.sub').textContent).not.toContain('s3cret');
    navigator.clipboard.writeText.mockClear();
    await click($$('.menu.sub .item').find((b) => b.textContent.startsWith('Email')));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('admin@target.test');
    // Duplicate (the copy is selected), then delete it: deleting asks first.
    await menuOn(item());
    await pick('Duplicate');
    expect(store.vault.entries.length).toBe(2);
    expect(navItem('Vault').textContent).toContain('2');
    expect($('.vault .dtitle .title').value).toBe('Admin staging (copy)');
    await click(buttonLabeled('Delete credential'));
    await answer(true);
    expect(store.vault.entries.length).toBe(1);
    expect(store.vault.entries[0].label).toBe('Admin staging');
    store.removeVaultEntry(store.vault.entries[0].id);
    await settle();
  });

  it('organizer: sortable/searchable table, note modal, right-click label/colour/capture/move/send', async () => {
    store.view = 'organizer';
    await settle();
    expect(text()).toContain('Organizer');
    store.addCollection('Targets');
    await settle();
    expect(calls).toContain('set_setting');
    // The first collection becomes the active save target, marked in the list.
    const targets = () => store.organizer.collections.find((c) => c.name === 'Targets');
    const colBtn = (name) => $$('.organizer .col-item').find((b) => b.textContent.includes(name));
    expect(store.organizer.activeId).toBe(targets().id);
    expect(colBtn('Targets').textContent).toContain('active');
    // Save two History flows (request AND response are captured).
    await store.saveFlowToOrganizer(1);
    await store.saveFlowToOrganizer(2);
    await settle();
    expect(targets().items.length).toBe(2);
    expect(targets().items[0].request).toContain('GET /v1/users');
    expect(targets().items[0].response).toContain('name');
    const rowsEl = () => $$('.organizer .rbody .rrow');
    expect(rowsEl().length).toBe(2);
    expect(rowsEl()[0].textContent).toContain('history'); // Source column
    // Sort: clicking Status twice sorts descending, so the 401 comes first.
    const th = (label) => $$('.organizer .th').find((b) => b.textContent.trim().startsWith(label));
    await click(th('Status'));
    await click(th('Status'));
    expect(rowsEl()[0].textContent).toContain('401');
    await click(th('#')); // back to saved order
    expect(rowsEl()[0].textContent).toContain('200');
    // Search filters the rows (matches the saved name /v1/login of flow #2).
    const search = $('.organizer .col-head .search input');
    search.value = 'login';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect(rowsEl().length).toBe(1);
    search.value = '';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect(rowsEl().length).toBe(2);
    // Selecting a row opens the request/response detail; no note/colour form under it.
    await click(rowsEl()[0]);
    expect($('.organizer .detail')).toBeTruthy();
    expect($('.organizer .detail textarea')).toBeNull();
    // Note: the row's note icon opens a modal to view / set it.
    await click(rowsEl()[0].querySelector('.note-btn'));
    const area = $('.note-modal textarea');
    area.value = 'creds in body';
    area.dispatchEvent(new Event('input', { bubbles: true }));
    await click($$('.note-modal button').find((b) => b.textContent.trim() === 'Save note'));
    expect(targets().items[0].note).toBe('creds in body');
    expect(rowsEl()[0].querySelector('.note-btn').classList.contains('has')).toBe(true);
    // Right-click: label (one-line modal) and colour (highlight swatch).
    const menuOn = async (el) => {
      el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 12, clientY: 12 }));
      await settle();
    };
    const pick = (label) => click($$('.menu .item').find((b) => b.textContent.trim().startsWith(label)));
    await menuOn(rowsEl()[0]);
    await pick('Set label');
    const labelIn = $('.entry input');
    labelIn.value = 'IDOR candidate';
    labelIn.dispatchEvent(new Event('input', { bubbles: true }));
    await click($('.entry button[type=submit]'));
    expect(targets().items[0].label).toBe('IDOR candidate');
    expect(rowsEl()[0].textContent).toContain('IDOR candidate');
    await menuOn(rowsEl()[0]);
    await click($('.menu .swatch.c-blue'));
    expect(targets().items[0].color).toBe('blue');
    // Capture (screenshot) from the detail header, same editor as History.
    await click(buttonLabeled('Capture screenshot'));
    expect($('.overlay')).toBeTruthy();
    await click([...document.querySelectorAll('.overlay button')].find((b) => b.textContent.trim() === 'Close'));
    expect($('.overlay')).toBeNull();
    // With a single collection there is nowhere to move to: "Move to" is greyed out.
    await menuOn(rowsEl()[0]);
    expect($$('.menu .item').find((b) => b.textContent.trim().startsWith('Move to')).disabled).toBe(true);
    // A new collection does not steal the active marker; move a request into it.
    store.addCollection('Other');
    await settle();
    expect(store.organizer.activeId).toBe(targets().id);
    await click(colBtn('Targets'));
    await menuOn(rowsEl()[1]);
    // "Move to" is ONE nested entry; the collections are listed inside it.
    const moveItems = $$('.menu .item').filter((b) => b.textContent.trim().startsWith('Move to'));
    expect(moveItems.length).toBe(1);
    expect($('.menu.sub')).toBeNull();
    await click(moveItems[0]);
    const subItems = $$('.menu.sub .item');
    expect(subItems.map((b) => b.textContent.trim())).toEqual([expect.stringContaining('Other')]);
    await click(subItems[0]);
    const other = () => store.organizer.collections.find((c) => c.name === 'Other');
    expect(targets().items.length).toBe(1);
    expect(other().items.length).toBe(1);
    // Collection right-click: make "Other" the active save target.
    await menuOn(colBtn('Other'));
    await pick('Set as active');
    expect(store.organizer.activeId).toBe(other().id);
    expect(colBtn('Other').textContent).toContain('active');
    expect(store.organizerTargetHint).toBe('Other');
    // Send to Repeater from the row menu (no inline buttons).
    const beforeTabs = store.repeater.tabs.length;
    await menuOn(rowsEl()[0]);
    await pick('Send to Repeater');
    expect(store.view).toBe('repeater');
    expect(store.repeater.tabs.length).toBe(beforeTabs + 1);
  });

  it('context menu: nested "Move to" opens on hover / ArrowRight and closes when leaving it', async () => {
    store.view = 'organizer';
    await settle();
    await click($$('.organizer .col-item').find((b) => b.textContent.includes('Targets')));
    $$('.organizer .rbody .rrow')[0].dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 12, clientY: 12 }));
    await settle();
    const move = $$('.menu .item').find((b) => b.textContent.trim().startsWith('Move to'));
    const copy = $$('.menu .item').find((b) => b.textContent.trim().startsWith('Copy full URL'));
    const wait = () => new Promise((r) => setTimeout(r, 260));
    // Hover opens it.
    move.dispatchEvent(new Event('pointerenter'));
    await settle();
    expect($('.menu.sub')).toBeTruthy();
    expect(move.getAttribute('aria-expanded')).toBe('true');
    // Passing over another item on the way into the submenu keeps it open (grace period).
    copy.dispatchEvent(new Event('pointerenter'));
    $('.menu.sub').dispatchEvent(new Event('pointerenter'));
    await wait();
    await settle();
    expect($('.menu.sub')).toBeTruthy();
    // Settling on another item closes it.
    copy.dispatchEvent(new Event('pointerenter'));
    await wait();
    await settle();
    expect($('.menu.sub')).toBeNull();
    // Keyboard: ArrowRight opens and focuses the first collection, ArrowLeft goes back.
    move.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    await settle();
    expect(document.activeElement?.closest('.menu.sub')).toBeTruthy();
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    await settle();
    expect($('.menu.sub')).toBeNull();
    expect(document.activeElement).toBe(move);
    // A click outside closes the whole menu.
    document.body.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
    await settle();
    expect($('.menu')).toBeNull();
  });

  it('organizer notes: two-pane editor saves as you type; delete asks first', async () => {
    store.view = 'organizer';
    await settle();
    await click($$('.organizer .tab').find((b) => b.textContent.trim().startsWith('Notes')));
    expect(text()).toContain('No notes yet');
    await click(buttonWith('+ New note'));
    expect(store.organizer.notes.length).toBe(1);
    const title = $('.neditor .ne-title');
    title.value = 'Recon';
    title.dispatchEvent(new Event('input', { bubbles: true }));
    const body = $('.neditor .ne-body');
    body.value = 'admin panel at /manage\nweak creds';
    body.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect(store.organizer.notes[0]).toMatchObject({ title: 'Recon', body: 'admin panel at /manage\nweak creds' });
    // The list shows the title and the first line as a preview.
    expect($('.nlist .nitem').textContent).toContain('Recon');
    expect($('.nlist .nitem').textContent).toContain('admin panel at /manage');
    // Search notes.
    const ns = $('.nlist .search input');
    ns.value = 'zzz';
    ns.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    expect($$('.nlist .nitem').length).toBe(0);
    ns.value = '';
    ns.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    // Deleting a note with content asks for confirmation first.
    await click(buttonLabeled('Delete note'));
    await answer(true);
    expect(store.organizer.notes.length).toBe(0);
  });

  it('intruder: right-click a tab to save it to the Organizer', async () => {
    store.view = 'intruder';
    await settle();
    const before = store.organizerCount;
    const tabEl = $('.intruder .tabs .tab');
    tabEl.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 14, clientY: 14 }));
    await settle();
    // The item names the target collection as its hint.
    const save = $$('.menu .item').find((b) => b.textContent.trim().startsWith('Save to Organizer'));
    expect(save.querySelector('.hint').textContent).toBe(store.organizerTargetHint);
    await click(save);
    expect(store.organizerCount).toBe(before + 1);
  });

  it('intruder: "Add position" can be undone with Ctrl+Z', async () => {
    store.view = 'intruder';
    await settle();
    const activeRaw = () => store.intruder.tabs.find((x) => x.id === store.intruder.activeId).raw;
    const marks = (s) => (s.match(/§/g) || []).length;
    const before = activeRaw();
    // Add a payload position (no selection -> inserts a §payload§ placeholder).
    await click(buttonWith('Add position'));
    expect(marks(activeRaw())).toBe(marks(before) + 2);
    // Ctrl+Z restores the template (undoes the position).
    $('.intruder .req').dispatchEvent(new KeyboardEvent('keydown', { key: 'z', ctrlKey: true, bubbles: true }));
    await settle();
    expect(activeRaw()).toBe(before);
  });

  it('intruder: custom right-click on the template, result rows and result panes', async () => {
    store.view = 'intruder';
    await settle();
    const t = store.intruder.tabs.find((x) => x.id === store.intruder.activeId);
    const labels = () => $$('.menu .item').map((b) => b.textContent.trim());
    const ctx = async (el) => {
      el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 30, clientY: 30 }));
      await settle();
    };
    // Request template: position + send actions instead of the webview's Inspect menu.
    await ctx($('.intruder .req'));
    expect(labels().some((l) => l.startsWith('Insert position') || l.startsWith('Add position'))).toBe(true);
    expect(labels().some((l) => l.startsWith('Save to Organizer'))).toBe(true);
    expect(labels().some((l) => l.startsWith('Send to Repeater'))).toBe(true);
    // A finished result: right-click its row and save it (request + response, payload as label).
    t.results = [{ n: 1, status: 200, length: 2, timeMs: 5, payloads: ['admin'], requestRaw: 'GET /search?q=admin HTTP/1.1\nHost: example.com\n\n', responseRaw: 'HTTP/1.1 200 OK\n\nok' }];
    t.selected = 1;
    await settle();
    const before = store.organizerCount;
    await ctx($$('.intruder .results .rows .row')[0]);
    expect(labels().some((l) => l.startsWith('Copy payload'))).toBe(true);
    await click($$('.menu .item').find((b) => b.textContent.trim().startsWith('Save to Organizer')));
    expect(store.organizerCount).toBe(before + 1);
    const saved = store.organizer.collections.flatMap((c) => c.items).at(-1);
    expect(saved).toMatchObject({ source: 'intruder', label: 'admin', status: 200 });
    expect(saved.response).toContain('ok');
    // The result's response and request panes each get their own menu.
    const panes = $$('.intruder .resp [style*="contents"]');
    await ctx(panes[1]);
    expect(labels().some((l) => l.startsWith('Copy response body'))).toBe(true);
    await ctx(panes[0]);
    expect(labels().some((l) => l.startsWith('Copy as cURL'))).toBe(true);
    t.results = [];
    t.selected = null;
  });

  it('intruder: result detail fills a resizable pane; arrow keys browse the sorted results', async () => {
    store.view = 'intruder';
    await settle();
    const t = store.intruder.tabs.find((x) => x.id === store.intruder.activeId);
    const result = (n, status) => ({ n, status, length: n * 10, timeMs: n, payloads: [`p${n}`], requestRaw: `GET /?q=p${n} HTTP/1.1\nHost: example.com\n\n`, responseRaw: `HTTP/1.1 ${status} X\n\nbody ${n}` });
    t.results = [result(1, 200), result(2, 500), result(3, 404)];
    t.selected = null;
    await settle();
    const rowEl = (n) => $(`.intruder .rows .row[data-n="${n}"]`);
    // No selection: the table fills the pane, no divider.
    expect($('.intruder .rdivider')).toBeNull();
    // Selecting a row opens the detail under a draggable divider.
    await click(rowEl(1));
    expect(t.selected).toBe(1);
    expect($('.intruder .rdivider')).toBeTruthy();
    expect($('.intruder .results').getAttribute('style')).toContain('8px');
    expect($('.intruder .resp').textContent).toContain('Request #1');
    expect($('.intruder .resp').textContent).toContain('Response #1');
    // Arrow keys move the selection (and stop the table from scrolling).
    const key = async (k) => {
      const ev = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true });
      (document.activeElement?.closest('.rows') ? document.activeElement : rowEl(t.selected)).dispatchEvent(ev);
      await settle();
      return ev.defaultPrevented;
    };
    expect(await key('ArrowDown')).toBe(true);
    expect(t.selected).toBe(2);
    expect($('.intruder .resp').textContent).toContain('Response #2');
    await key('ArrowDown');
    expect(t.selected).toBe(3);
    await key('ArrowDown'); // stays on the last row
    expect(t.selected).toBe(3);
    await key('ArrowUp');
    expect(t.selected).toBe(2);
    await key('Home');
    expect(t.selected).toBe(1);
    await key('End');
    expect(t.selected).toBe(3);
    // It follows the displayed order: sort by Status descending (500, 404, 200).
    const th = (label) => $$('.intruder .th').find((b) => b.textContent.trim().startsWith(label));
    await click(th('Status'));
    await click(th('Status'));
    await click(rowEl(2)); // 500 is first
    await key('ArrowDown');
    expect(t.selected).toBe(3); // 404 next
    await key('ArrowDown');
    expect(t.selected).toBe(1); // then 200
    // Request / response can be stacked top-bottom, then closed.
    await click(buttonLabeled('Toggle request/response layout'));
    expect($('.intruder .resp .split.vertical')).toBeTruthy();
    await click(buttonLabeled('Toggle request/response layout'));
    expect($('.intruder .resp .split.vertical')).toBeNull();
    await click(buttonLabeled('Close result'));
    expect(t.selected).toBeNull();
    expect($('.intruder .rdivider')).toBeNull();
    await click(th('#')); // back to the default order
    t.results = [];
  });

  it('organizer: edits made over MCP show up live, and an older state is ignored', async () => {
    store.view = 'organizer';
    await settle();
    store.viewCollection(store.organizer.activeId);
    await settle();
    // The MCP server saves into the active collection behind the UI's back...
    const before = store.organizerCount;
    orgApply({ op: 'save', item: { request: 'GET /from-mcp HTTP/1.1', target: 'https://mcp.test', source: 'mcp', label: 'agent' } });
    listeners.organizer({ payload: { rev: org.rev } });
    await settle();
    // ...and the open Organizer shows it right away.
    expect(store.organizerCount).toBe(before + 1);
    expect(text()).toContain('/from-mcp');
    expect(text()).toContain('mcp');
    // A late event for an older revision neither refetches nor rolls back.
    calls.length = 0;
    listeners.organizer({ payload: { rev: 1 } });
    await settle();
    expect(calls).not.toContain('organizer_state');
    expect(store.organizerCount).toBe(before + 1);
  });

  it('organizer and repeater deletes ask Yes/No; No keeps the data', async () => {
    // Organizer: a saved request's row menu "Delete…".
    const itemId = await store.saveRequestToCollection({ name: '/keep-me', method: 'GET', host: 'h', path: '/keep-me', request: 'GET /keep-me HTTP/1.1\r\nHost: h\r\n\r\n', response: '', source: 'history' });
    const col = store.organizer.collections.find((c) => c.items.some((it) => it.id === itemId));
    store.viewCollection(col.id);
    store.view = 'organizer';
    await settle();
    const row = () => $$('.organizer .rbody .rrow').find((r) => r.textContent.includes('/keep-me'));
    const rowDelete = async () => {
      row().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 10, clientY: 10 }));
      await settle();
      await click($$('.menu .item').find((b) => b.textContent.trim() === 'Delete…'));
    };
    await rowDelete();
    expect(dialog().textContent).toContain('Delete saved request "/keep-me"?');
    await answer(false);
    expect(row()).toBeTruthy();
    await rowDelete();
    await answer(true);
    expect(row()).toBeFalsy();
    // Deleting a collection that still holds requests asks too.
    await store.saveRequestToCollection({ name: '/inside', method: 'GET', host: 'h', path: '/inside', request: 'GET /inside HTTP/1.1\r\n\r\n', response: '', source: 'history' }, col.id);
    await settle();
    const colItem = $$('.organizer .col-item').find((b) => b.textContent.includes(col.name));
    colItem.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 10, clientY: 10 }));
    await settle();
    await click($$('.menu .item').find((b) => b.textContent.trim() === 'Delete…'));
    expect(dialog().textContent).toContain(`Delete collection "${col.name}"?`);
    await answer(false);
    expect(store.organizer.collections.some((c) => c.id === col.id)).toBe(true);

    // Repeater: "Delete group + tabs…" asks; No keeps every tab.
    store.addRepeaterTab({ name: 'g1', group: 'Doomed', target: 'https://h', raw: 'GET / HTTP/1.1\r\nHost: h\r\n\r\n' });
    store.addRepeaterTab({ name: 'g2', group: 'Doomed', target: 'https://h', raw: 'GET / HTTP/1.1\r\nHost: h\r\n\r\n' });
    store.view = 'repeater';
    await settle();
    const inGroup = () => store.repeater.tabs.filter((t) => t.group === 'Doomed').length;
    const groupDelete = async () => {
      $('.group-head[data-group="Doomed"] .group-label').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 10, clientY: 10 }));
      await settle();
      await click($$('.menu .item').find((b) => b.textContent.trim() === 'Delete group + tabs…'));
    };
    await groupDelete();
    expect(dialog().textContent).toContain('Delete group "Doomed" and its 2 tabs?');
    await answer(false);
    expect(inGroup()).toBe(2);
    await groupDelete();
    await answer(true);
    expect(inGroup()).toBe(0);
  });

  it('MCP is per project: a new project starts with it off, a project that had it on gets it back', async () => {
    const origGet = backend.get_setting;
    const origSet = backend.set_setting;
    const settings = new Map(); // the open project's settings table
    backend.get_setting = ({ key }) => settings.get(key) ?? null;
    backend.set_setting = ({ key, value }) => (settings.set(key, value), null);
    try {
      // Switched on here: remembered by THIS project.
      await store.setMcp(true, 8082, '/docs');
      expect(store.mcp.running).toBe(true);
      expect(settings.get('mcp')).toEqual({ enabled: true });
      const mine = new Map(settings);

      // A new project has no switch of its own: MCP stops instead of carrying over,
      // and nothing is written into the new project.
      settings.clear();
      await store.blankProject();
      await settle();
      expect(store.mcp.running).toBe(false);
      expect(settings.has('mcp')).toBe(false);

      // Loading a project that had it on (same load path as Open) starts it again.
      for (const [k, v] of mine) settings.set(k, v);
      await store.blankProject();
      await settle();
      expect(store.mcp.running).toBe(true);
    } finally {
      backend.get_setting = origGet;
      backend.set_setting = origSet;
      await store.setMcp(false, 8082, '/docs');
    }
  });

  it('old Organizer notes and flat Vault entries migrate to the current shape on load', async () => {
    const origGet = backend.get_setting;
    const origSet = backend.set_setting;
    const written = [];
    backend.get_setting = ({ key }) =>
      key === 'organizer'
        ? { collections: [], notes: [{ id: 'n1', title: 'Legacy', body: 'keep me', updatedAt: 1 }] }
        : key === 'vault'
          ? { entries: [{ id: 'v1', label: 'Old', username: 'root', password: 'toor', url: '' }] }
          : null;
    backend.set_setting = ({ key }) => (written.push(key), null);
    try {
      await store.blankProject();
      await settle();
      // Notes move to their own key right away.
      expect(store.organizer.notes.map((n) => n.title)).toEqual(['Legacy']);
      expect(written).toContain('organizerNotes');
      // A flat vault entry becomes typed fields (password secret) and is written back.
      expect(store.vault.entries[0].fields.map((f) => [f.kind, f.value, f.secret])).toEqual([
        ['username', 'root', false],
        ['password', 'toor', true],
      ]);
      expect(written).toContain('vault');
    } finally {
      backend.get_setting = origGet;
      backend.set_setting = origSet;
    }
  });

  it('threw no uncaught errors', () => {
    expect(errors).toEqual([]);
  });
});
