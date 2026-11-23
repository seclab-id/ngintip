// The socket tab's layout, asserted as CSS and as markup rather than as pixels.
//
// jsdom reports every element as 0x0, so mounting the view cannot tell a filled panel
// from the empty white rectangle this replaced. That rectangle had two causes, and both
// are rules rather than pixels: the frame list only existed once a flow did (so an
// unconnected tab had nothing between the toolbar and the composer), and the composer
// was free to grow while nothing claimed the space above it.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const file = readFileSync(resolve(import.meta.dirname, 'RepeaterView.svelte'), 'utf8');
const css = file.slice(file.lastIndexOf('<style>'));

function block(selector) {
  const at = css.indexOf(`\n  ${selector} {`);
  expect(at, `${selector} should exist`).toBeGreaterThan(-1);
  return css.slice(at, css.indexOf('\n  }', at));
}

describe('Repeater socket tab: the frames fill the panel', () => {
  it('lays the pane out as a flex column that may be shorter than its content', () => {
    const pane = block('.wsrep');
    expect(pane).toMatch(/display:\s*flex/);
    expect(pane).toMatch(/flex-direction:\s*column/);
    expect(pane).toMatch(/min-height:\s*0/);
  });

  it('gives the frame list everything the composer does not take', () => {
    expect(block('.wsframes')).toMatch(/flex:\s*1/);
    expect(block('.wsframes')).toMatch(/min-height:\s*0/);
    // Without this the textarea's growth pushes the list away instead of the composer
    // keeping its own height.
    expect(block('.wscompose')).toMatch(/flex-shrink:\s*0/);
  });

  it('shows something in that space before the first connection', () => {
    // Both branches live inside .wsframes, so the area is never empty: either the
    // frames, or the panel that explains what the tab is for.
    const frames = file.slice(file.indexOf('<div class="wsframes">'), file.indexOf('<div class="wscompose">'));
    expect(frames).toMatch(/<WsView/);
    expect(frames).toMatch(/wsblank/);
    expect(block('.wsblank')).toMatch(/align-items:\s*center/);
    expect(block('.wsblank')).toMatch(/justify-content:\s*center/);
  });

  it('never shows the request toolbar on a socket tab', () => {
    // The HTTP toolbar's primary button says Send and answers "request is empty" on a
    // socket tab. The two toolbars are exclusive branches of one `isWs`.
    const toolbars = file.slice(file.indexOf('{#if isWs}'), file.indexOf('<div class="grid"'));
    expect(toolbars).toMatch(/class="toolbar wstoolbar"/);
    expect(toolbars.indexOf('send-split')).toBeGreaterThan(toolbars.indexOf('{:else}'));
  });
});
