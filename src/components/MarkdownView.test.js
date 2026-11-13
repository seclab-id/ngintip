// @vitest-environment jsdom
// The note preview attaches click listeners in an `$effect` so that links open in the
// browser instead of navigating the webview away. An effect that adds a listener and
// forgets to remove it leaks on every toggle between Write and Preview, which is a
// thing a user does constantly, so the balance is pinned here rather than trusted.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const opened = [];
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async (cmd, args) => {
    if (cmd === 'open_url') opened.push(args.url);
    return null;
  }),
}));

const { default: MarkdownView } = await import('./MarkdownView.svelte');

let added;
let removed;
const origAdd = Element.prototype.addEventListener;
const origRemove = Element.prototype.removeEventListener;

beforeEach(() => {
  opened.length = 0;
  added = 0;
  removed = 0;
  Element.prototype.addEventListener = function (...args) {
    added++;
    return origAdd.apply(this, args);
  };
  Element.prototype.removeEventListener = function (...args) {
    removed++;
    return origRemove.apply(this, args);
  };
});

afterEach(() => {
  Element.prototype.addEventListener = origAdd;
  Element.prototype.removeEventListener = origRemove;
  document.body.innerHTML = '';
});

describe('MarkdownView', () => {
  it('removes every listener it adds, over many mounts', () => {
    for (let i = 0; i < 50; i++) {
      const app = mount(MarkdownView, { target: document.body, props: { source: `# note ${i}` } });
      flushSync();
      unmount(app);
      flushSync();
    }
    expect(added).toBeGreaterThan(0);
    expect(removed).toBe(added);
    expect(document.body.querySelector('.md')).toBeNull();
  });

  it('opens a link through the backend instead of navigating the webview', () => {
    const app = mount(MarkdownView, {
      target: document.body,
      props: { source: 'see [policy](https://target.test/policy)' },
    });
    flushSync();

    const a = document.body.querySelector('a[href]');
    const ev = new MouseEvent('click', { bubbles: true, cancelable: true });
    a.dispatchEvent(ev);

    // Default prevented, so the webview does not follow the href and drop the session.
    expect(ev.defaultPrevented).toBe(true);
    expect(opened).toEqual(['https://target.test/policy']);

    unmount(app);
  });

  it('ignores a click that is not on a link', () => {
    const app = mount(MarkdownView, { target: document.body, props: { source: 'plain text' } });
    flushSync();
    document.body.querySelector('.md').dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(opened).toEqual([]);
    unmount(app);
  });
});

describe('the rendered note stays selectable', () => {
  // The app turns selection off on `body` and each text surface opts back in. A
  // preview you cannot select is a preview you cannot copy out of, which defeats
  // keeping notes in the first place - so the opt-in is pinned here, together with the
  // global rule that makes it necessary.
  const css = (f) => readFileSync(resolve(process.cwd(), f), 'utf8');

  it('opts back in, because the app disables selection globally', () => {
    expect(css('src/app.css')).toMatch(/body\s*\{[^}]*user-select:\s*none/);

    const view = css('src/components/MarkdownView.svelte');
    const block = view.slice(view.indexOf('  .md {'));
    const md = block.slice(0, block.indexOf('}') + 1);
    expect(md).toMatch(/[^-]user-select:\s*text/);
    expect(md).toMatch(/-webkit-user-select:\s*text/);
  });

  it('still yields to a pane locked mid-drag', () => {
    // `.md` opting in must not defeat the drag lock, which is marked important.
    const app = css('src/app.css');
    const lock = app.slice(app.indexOf('[data-selectlock],'));
    expect(lock.slice(0, lock.indexOf('}') + 1)).toMatch(/user-select:\s*none\s*!important/);
  });
});
