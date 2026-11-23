// The WebSocket viewer's layout, asserted as CSS rather than as pixels.
//
// jsdom has no layout engine: it will happily report every element as 0x0, so a test
// that mounts the component cannot tell a scrolling pane from a clipped one. The bug
// this guards against was exactly that difference. The detail pane was a grid row sized
// `height: 45%`, and a percentage height against an `auto` grid track is indefinite, so
// it was ignored: the pane grew to fit the whole message, overflowed the panel, and
// `.panel { overflow: hidden }` cut the rest off with no scrollbar anywhere to reach it.
// A 118-byte JSON frame showed three of its lines and no way to see the rest.
//
// So the rules themselves are the test. Each assertion below is one of the three things
// that have to hold for a scrollable pane inside a flex column, and each one failing is
// a way the message detail goes back to being clipped.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const css = readFileSync(resolve(import.meta.dirname, 'WsView.svelte'), 'utf8').split('<style>')[1];

/** The declarations inside one rule, e.g. block('.detail'). */
function block(selector) {
  const at = css.indexOf(`\n  ${selector} {`);
  expect(at, `${selector} should exist`).toBeGreaterThan(-1);
  return css.slice(at, css.indexOf('\n  }', at));
}

describe('WsView layout: the message detail has to scroll, not clip', () => {
  it('lays the panel out as a flex column that may be shorter than its content', () => {
    const ws = block('.ws');
    expect(ws).toMatch(/display:\s*flex/);
    expect(ws).toMatch(/flex-direction:\s*column/);
    expect(ws).toMatch(/min-height:\s*0/);
  });

  it('gives the detail pane a share of the panel, not a percentage of nothing', () => {
    const detail = block('.detail');
    // flex-basis resolves against the panel's own height, which is definite. A plain
    // `height: NN%` here is the bug: it was silently dropped and the pane grew instead.
    expect(detail).toMatch(/flex:\s*0\s+0\s+\d+%/);
    expect(detail).not.toMatch(/\n\s*height:\s*\d+%/);
    // Floors and ceilings, so one huge frame cannot swallow the message list and a
    // short panel still shows a few lines.
    expect(detail).toMatch(/min-height:\s*\d+px/);
    expect(detail).toMatch(/max-height:/);
  });

  it('lets both scrollers be smaller than what they hold', () => {
    for (const selector of ['.list', 'pre']) {
      const rule = block(selector);
      expect(rule, `${selector} should scroll`).toMatch(/overflow:\s*auto/);
      expect(rule, `${selector} needs a zero minimum or it grows instead`).toMatch(/min-height:\s*0/);
    }
    // The list takes what the detail leaves; without this it keeps its content height.
    expect(block('.list')).toMatch(/flex:\s*1/);
    expect(block('pre')).toMatch(/flex:\s*1/);
  });
});
