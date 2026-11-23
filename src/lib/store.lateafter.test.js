// @vitest-environment jsdom
// The two patience settings (Settings > Project > Performance). Both live in the open
// project, like the MCP switch: how long to wait is a judgement about the system under
// test, so it travels with the engagement and not with the machine.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const settings = new Map();
const calls = [];
const failCommands = new Set();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    calls.push([cmd, args]);
    if (failCommands.has(cmd)) throw new Error(`${cmd} not allowed`);
    if (cmd === 'get_setting') return settings.get(args.key) ?? null;
    if (cmd === 'set_setting') return settings.set(args.key, args.value), null;
    return null;
  }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const { store, LATE_AFTER_SEC, GIVE_UP_AFTER_SEC } = await import('./store.svelte.js');

beforeEach(() => {
  settings.clear();
  calls.length = 0;
  failCommands.clear();
});

describe('the patience settings', () => {
  it('ships the defaults the operator asked for', () => {
    expect(LATE_AFTER_SEC).toBe(60);
    expect(GIVE_UP_AFTER_SEC).toBe(180);
  });

  it('writes the badge threshold into the project, not the machine', async () => {
    await store.setLateAfterSec(90);
    expect(store.lateAfterSec).toBe(90);
    expect(settings.get('waitLate')).toEqual({ seconds: 90 });
    // Nothing per-machine is touched, so another project does not inherit it.
    expect(localStorage.getItem('ngintip.lateAfter')).toBeNull();
  });

  it('writes the deadline into the project and tells the proxy at once', async () => {
    await store.setGiveUpAfter(240);
    expect(settings.get('giveUp')).toEqual({ seconds: 240 });
    expect(calls.some(([c, a]) => c === 'set_give_up_after' && a.seconds === 240)).toBe(true);
  });

  it('keeps both inside something a badge or a deadline can mean', async () => {
    await store.setLateAfterSec(-30);
    expect(store.lateAfterSec).toBe(0);
    await store.setLateAfterSec(999999);
    expect(store.lateAfterSec).toBe(86400);
    await store.setGiveUpAfter(45.6);
    expect(store.giveUpAfterSec).toBe(46);
  });

  it('survives the backend handing over a fresh config object', async () => {
    // This is how it broke: the value lived on `config`, and launch replaces `config`
    // wholesale with whatever get_config returns, a few lines after the project
    // settings were read. The value vanished and the form showed 0.
    await store.setGiveUpAfter(150);
    store.config = { auth: {}, upstream: '', invisible: false, autoContentLength: true };
    expect(store.giveUpAfterSec).toBe(150);
  });

  it('still saves the deadline when the proxy refuses to take it', async () => {
    // How it was lost: the live push ran first and was awaited, so one rejected invoke
    // skipped the save, dropped the value, and threw where nothing was catching. The
    // form went on showing a number that had reached neither the proxy nor the project.
    failCommands.add('set_give_up_after');
    await store.setGiveUpAfter(6);
    expect(settings.get('giveUp')).toEqual({ seconds: 6 });
    expect(store.giveUpAfterSec).toBe(6);
  });

  it('0 is a real choice and survives as one', async () => {
    // Not a missing value: it means "never escalate" and "never give up", and must not
    // be read back as the default on the next project load.
    await store.setLateAfterSec(0);
    await store.setGiveUpAfter(0);
    expect(settings.get('waitLate')).toEqual({ seconds: 0 });
    expect(settings.get('giveUp')).toEqual({ seconds: 0 });
  });
});
