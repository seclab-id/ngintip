// @vitest-environment jsdom
// The startup update check: it decides whether the user is interrupted on launch, and
// it writes to the Logs view, so both the decision and the log line are pinned here.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

let logged;
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    if (cmd === 'app_log') logged.push(args);
    return null;
  }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const { store } = await import('./store.svelte.js');
const { BRAND, brandKey } = await import('./brand.js');

const SKIP_KEY = brandKey('update.skipped');

/** Make the GitHub "latest release" call answer with `tag`, or fail when it is null. */
function github(tag, extra = {}) {
  globalThis.fetch = vi.fn(async () => {
    if (tag === null) throw new Error('offline');
    return { ok: true, json: async () => ({ tag_name: tag, html_url: 'https://example.test/rel', ...extra }) };
  });
}

/** Rewind the per-session state the check keeps, as a fresh launch would. */
function relaunch() {
  Object.assign(store.update, {
    current: BRAND.version,
    latest: null,
    newer: false,
    checking: false,
    checked: false,
    name: '',
    prompted: false,
  });
  store.updatePrompt = false;
}

beforeEach(() => {
  logged = [];
  localStorage.removeItem(SKIP_KEY);
  relaunch();
});

afterEach(() => {
  localStorage.removeItem(SKIP_KEY);
});

describe('startup update check', () => {
  it('prompts, and logs it, when a newer release exists', async () => {
    github('v99.0.0', { name: 'Ngintip 99.0.0' });
    await store.promptUpdateAtStartup();

    expect(store.update.latest).toBe('99.0.0');
    expect(store.update.newer).toBe(true);
    expect(store.update.name).toBe('Ngintip 99.0.0');
    expect(store.updatePrompt).toBe(true);
    expect(logged).toEqual([
      { level: 'info', source: 'update', message: `update available: v99.0.0 (running v${BRAND.version})` },
    ]);
  });

  it('stays quiet on the current version, and logs that at debug level', async () => {
    github(`v${BRAND.version}`);
    await store.promptUpdateAtStartup();

    expect(store.update.newer).toBe(false);
    expect(store.updatePrompt).toBe(false);
    expect(logged).toEqual([
      { level: 'debug', source: 'update', message: `up to date (v${BRAND.version})` },
    ]);
  });

  it('treats a release older than the running build as nothing to do', async () => {
    github('v0.0.1');
    await store.promptUpdateAtStartup();

    expect(store.update.newer).toBe(false);
    expect(store.updatePrompt).toBe(false);
  });

  it('says nothing at all when GitHub cannot be reached', async () => {
    github(null);
    await store.promptUpdateAtStartup();

    expect(store.updatePrompt).toBe(false);
    expect(store.update.checked).toBe(true);
    expect(logged).toEqual([]);
  });

  it('prompts once per session, not again on a second check', async () => {
    github('v99.0.0');
    await store.promptUpdateAtStartup();
    expect(store.updatePrompt).toBe(true);

    store.dismissUpdate();
    await store.promptUpdateAtStartup();
    expect(store.updatePrompt).toBe(false);
  });

  it('comes back on the next launch after "Remind me later"', async () => {
    github('v99.0.0');
    await store.promptUpdateAtStartup();
    store.dismissUpdate();
    expect(store.updatePrompt).toBe(false);

    relaunch();
    await store.promptUpdateAtStartup();
    expect(store.updatePrompt).toBe(true);
  });

  it('stays away for a skipped version, across launches', async () => {
    github('v99.0.0');
    await store.promptUpdateAtStartup();
    store.skipUpdate();
    expect(store.updatePrompt).toBe(false);
    expect(localStorage.getItem(SKIP_KEY)).toBe('99.0.0');
    expect(logged.at(-1)).toEqual({
      level: 'info',
      source: 'update',
      message: 'skipped v99.0.0; no reminder until a newer release',
    });

    relaunch();
    await store.promptUpdateAtStartup();
    expect(store.updatePrompt).toBe(false);
  });

  it('prompts again once something newer than the skipped version ships', async () => {
    localStorage.setItem(SKIP_KEY, '99.0.0');
    github('v99.1.0');
    await store.promptUpdateAtStartup();
    expect(store.updatePrompt).toBe(true);
  });

  it('keeps the newer flag even when the prompt is suppressed, so Settings still shows it', async () => {
    localStorage.setItem(SKIP_KEY, '99.0.0');
    github('v99.0.0');
    await store.promptUpdateAtStartup();

    expect(store.updatePrompt).toBe(false);
    expect(store.update.newer).toBe(true);
    expect(store.update.latest).toBe('99.0.0');
  });

  it('closes the dialog when Download is chosen', async () => {
    github('v99.0.0');
    await store.promptUpdateAtStartup();
    store.downloadUpdate();
    expect(store.updatePrompt).toBe(false);
  });
});
