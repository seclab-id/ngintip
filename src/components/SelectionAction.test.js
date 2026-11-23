// @vitest-environment jsdom
// The decoder dock used to open itself the moment a selection looked decodable. It now
// waits for a click on a chip, which is a UX contract worth pinning: the whole point is
// that highlighting text to copy it moves nothing on screen.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => null) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));

const { default: SelectionAction } = await import('./SelectionAction.svelte');
const { default: MessageView } = await import('./MessageView.svelte');

let app = null;
let target = null;

function render(Component, props) {
  target = document.createElement('div');
  document.body.append(target);
  app = mount(Component, { target, props });
  flushSync();
  return target;
}

afterEach(() => {
  if (app) unmount(app);
  app = null;
  target?.remove();
  target = null;
});

const chipOf = (root) => [...root.querySelectorAll('button')].find((b) => b.textContent.includes('Decode'));
const mouse = (type, el, x = 40, y = 60) => el.dispatchEvent(new window.MouseEvent(type, { bubbles: true, clientX: x, clientY: y }));

describe('SelectionAction: the Decode chip', () => {
  it('reports the click, and nothing before it', () => {
    const onpick = vi.fn();
    const onclose = vi.fn();
    const root = render(SelectionAction, { x: 100, y: 200, hint: 'Base64', onpick, onclose });
    const chip = chipOf(root);
    expect(chip).toBeTruthy();
    expect(chip.textContent).toContain('Base64');
    expect(onpick).not.toHaveBeenCalled();
    chip.click();
    expect(onpick).toHaveBeenCalledTimes(1);
  });

  it('keeps the selection alive while it is being clicked', () => {
    // A mousedown on a button clears the document selection, and the operator would
    // watch their own highlight vanish under the pointer. The default is prevented, so
    // the text stays visibly selected while the dock opens.
    const root = render(SelectionAction, { x: 10, y: 10, onpick: vi.fn(), onclose: vi.fn() });
    const ev = new window.MouseEvent('mousedown', { bubbles: true, cancelable: true });
    chipOf(root).dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
  });

  it('gets out of the way of anything that would strand it', () => {
    const onclose = vi.fn();
    render(SelectionAction, { x: 10, y: 10, onpick: vi.fn(), onclose });

    mouse('mousedown', document.body);
    expect(onclose).toHaveBeenCalledTimes(1);

    window.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape' }));
    expect(onclose).toHaveBeenCalledTimes(2);

    // A pane scrolling under the chip. Scroll does not bubble, so this only arrives if
    // the listener is registered for the capture phase.
    const pane = document.createElement('div');
    document.body.append(pane);
    pane.dispatchEvent(new window.Event('scroll', { bubbles: false }));
    expect(onclose).toHaveBeenCalledTimes(3);
    pane.remove();
  });

  it('stops listening once it is gone', () => {
    const onclose = vi.fn();
    render(SelectionAction, { x: 10, y: 10, onpick: vi.fn(), onclose });
    unmount(app);
    app = null;
    document.createElement('div').dispatchEvent(new window.Event('scroll'));
    window.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape' }));
    expect(onclose).not.toHaveBeenCalled();
  });
});

describe('MessageView: highlighting no longer opens the decoder', () => {
  // jsdom has no real text selection, so the selection is stated rather than performed:
  // what is under test is what the pane does with one, not the browser's word finding.
  function selecting(text, pre) {
    window.getSelection = () => ({ isCollapsed: false, anchorNode: pre.firstChild ?? pre, toString: () => text });
  }

  it('shows a chip instead, and opens the dock only on the click', () => {
    const ondecode = vi.fn();
    const root = render(MessageView, { title: 'Request', raw: 'GET /x HTTP/1.1\nHost: t\n\ncm9sZT1hZG1pbg==', ondecode });
    const pre = root.querySelector('pre.code');
    expect(pre).toBeTruthy();

    selecting('cm9sZT1hZG1pbg==', pre);
    mouse('mouseup', pre);
    flushSync();

    expect(ondecode).not.toHaveBeenCalled();
    const chip = chipOf(document.body);
    expect(chip).toBeTruthy();
    // The hint names the likely decoding without committing to it.
    expect(chip.textContent).toContain('Base64');

    chip.click();
    flushSync();
    expect(ondecode).toHaveBeenCalledWith('cm9sZT1hZG1pbg==');
    expect(chipOf(document.body)).toBeFalsy();
  });

  it('offers text the detector cannot place, without a hint', () => {
    // Opening the decoder by hand is the point: an operator who wants to encode a word,
    // or to try an operation the detector does not guess, could not before.
    const ondecode = vi.fn();
    const root = render(MessageView, { title: 'Request', raw: 'GET /x HTTP/1.1\nHost: t\n\nproductType', ondecode });
    const pre = root.querySelector('pre.code');
    selecting('productType', pre);
    mouse('mouseup', pre);
    flushSync();

    const chip = chipOf(document.body);
    expect(chip).toBeTruthy();
    expect(chip.textContent.trim()).toBe('Decode');
    chip.click();
    flushSync();
    expect(ondecode).toHaveBeenCalledWith('productType');
  });

  it('shows nothing for an empty selection', () => {
    const ondecode = vi.fn();
    const root = render(MessageView, { title: 'Request', raw: 'GET /x HTTP/1.1\nHost: t\n\nbody', ondecode });
    const pre = root.querySelector('pre.code');
    window.getSelection = () => ({ isCollapsed: true, anchorNode: pre, toString: () => '' });
    mouse('mouseup', pre);
    flushSync();
    expect(chipOf(document.body)).toBeFalsy();
    expect(ondecode).not.toHaveBeenCalled();
  });
});
