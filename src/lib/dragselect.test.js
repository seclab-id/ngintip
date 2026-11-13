// @vitest-environment jsdom
// Dragging a selection in one message pane used to keep selecting through the next one,
// because request and response are two parts of the same document.
//
// The first attempt at this set `user-select: none` inline on the other pane's root and
// did nothing, because `.code` in app.css sets `user-select: text` on the very text that
// needed locking and beat the inherited value. So these check the attribute AND that the
// stylesheet rule behind it actually wins against `.code`.
import { beforeEach, describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { LOCK_ATTR, PANE_ATTR, isolateSelection } from './dragselect.js';

const build = (html) => {
  document.body.innerHTML = html;
  return Array.from(document.querySelectorAll(`[${PANE_ATTR}]`));
};
const locked = (el) => el.hasAttribute(LOCK_ATTR);

beforeEach(() => {
  document.body.innerHTML = '';
});

describe('isolateSelection', () => {
  it('locks the other panes and leaves the one being dragged in alone', () => {
    const [req, res] = build(`<div ${PANE_ATTR}>request</div><div ${PANE_ATTR}>response</div>`);

    const restore = isolateSelection(res);
    expect(locked(req)).toBe(true);
    expect(locked(res)).toBe(false);

    restore();
    expect(locked(req)).toBe(false);
  });

  it('locks every other pane, not just the neighbour', () => {
    const panes = build(`<div ${PANE_ATTR}>a</div><div ${PANE_ATTR}>b</div><div ${PANE_ATTR}>c</div>`);
    const restore = isolateSelection(panes[1]);
    expect(panes.map(locked)).toEqual([true, false, true]);
    restore();
    expect(panes.map(locked)).toEqual([false, false, false]);
  });

  it('touches nothing but its own attribute', () => {
    const [a, b] = build(`<div ${PANE_ATTR} style="user-select: text">a</div><div ${PANE_ATTR}>b</div>`);
    const restore = isolateSelection(b);
    expect(a.style.userSelect).toBe('text'); // inline styles are left exactly as found
    restore();
    expect(a.style.userSelect).toBe('text');
    expect(a.hasAttribute(LOCK_ATTR)).toBe(false);
  });

  it('is safe to call restore twice', () => {
    const [a, b] = build(`<div ${PANE_ATTR}>a</div><div ${PANE_ATTR}>b</div>`);
    const restore = isolateSelection(b);
    restore();
    a.setAttribute(LOCK_ATTR, ''); // something else locks it afterwards
    restore();
    expect(locked(a)).toBe(true); // the second call must not undo that
  });

  it('never locks an ancestor or a descendant of the dragged pane', () => {
    const panes = build(`<div ${PANE_ATTR} id="outer"><div ${PANE_ATTR} id="inner">x</div></div>`);
    const restore = isolateSelection(document.getElementById('inner'));
    // Locking the ancestor would make the pane being dragged in unselectable too.
    expect(panes.map(locked)).toEqual([false, false]);
    restore();
  });

  it('does nothing when there is only one pane, or no root', () => {
    const [only] = build(`<div ${PANE_ATTR}>alone</div>`);
    expect(() => isolateSelection(only)()).not.toThrow();
    expect(locked(only)).toBe(false);
    expect(() => isolateSelection(null)()).not.toThrow();
  });
});

describe('the stylesheet rule behind the lock', () => {
  // Read as a file: a `.css` import is handled by vite and comes back empty here.
  const css = readFileSync(resolve(process.cwd(), 'src/app.css'), 'utf8');

  it('reaches descendants, because the locked text sets user-select on itself', () => {
    // `.code` is the element that actually holds the message text. Locking only the
    // pane root leaves it selectable, which is the bug this rule exists to fix.
    expect(css).toMatch(/\.code\s*\{[^}]*user-select:\s*text/);
    expect(css).toContain(`[${LOCK_ATTR}] *`);
  });

  it('outranks it, and covers the WebKit prefix the app targets', () => {
    const rule = css.slice(css.indexOf(`[${LOCK_ATTR}],`));
    const block = rule.slice(0, rule.indexOf('}') + 1);
    expect(block).toMatch(/[^-]user-select:\s*none\s*!important/);
    expect(block).toMatch(/-webkit-user-select:\s*none\s*!important/);
  });
});
