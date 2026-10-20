import { describe, expect, it } from 'vitest';
import { customExts, emptyHide, extOf, hideActive, isHidden } from './hidefilter.js';

const flow = (o) => ({ method: 'GET', status: 200, host: 'x', path: '/a', mime: '', ...o });

describe('hidefilter', () => {
  it('extOf reads the extension, ignoring query/hash', () => {
    expect(extOf('/img/logo.PNG?v=2')).toBe('png');
    expect(extOf('/app.min.js')).toBe('js');
    expect(extOf('/v1/users')).toBe('');
    expect(extOf('/')).toBe('');
    expect(extOf('/.env')).toBe('');
  });

  it('customExts parses commas/spaces and strips dots/stars', () => {
    expect(customExts(' .map, *.txt  wasm ')).toEqual(['map', 'txt', 'wasm']);
    expect(customExts('')).toEqual([]);
  });

  it('an empty hide filter is inactive and hides nothing', () => {
    expect(hideActive(emptyHide())).toBe(false);
    expect(isHidden(flow({ path: '/x.png' }), emptyHide())).toBe(false);
  });

  it('hides by category via extension or MIME (exclude mode)', () => {
    const h = { cats: ['images'], ext: '', exclude: true };
    expect(hideActive(h)).toBe(true);
    expect(isHidden(flow({ path: '/logo.png' }), h)).toBe(true);
    expect(isHidden(flow({ path: '/x', mime: 'image/webp' }), h)).toBe(true);
    expect(isHidden(flow({ path: '/data.json', mime: 'application/json' }), h)).toBe(false);
  });

  it('include mode shows only the selected types (hides the rest)', () => {
    const h = { cats: ['images'], ext: '', exclude: false };
    // Matching types are shown, everything else is hidden.
    expect(isHidden(flow({ path: '/logo.png' }), h)).toBe(false);
    expect(isHidden(flow({ path: '/data.json', mime: 'application/json' }), h)).toBe(true);
    // Same for the custom extension list.
    const ext = { cats: [], ext: 'json', exclude: false };
    expect(isHidden(flow({ path: '/a.json' }), ext)).toBe(false);
    expect(isHidden(flow({ path: '/a.png' }), ext)).toBe(true);
  });

  it('hides by custom extension list', () => {
    const h = { cats: [], ext: 'map,wasm', exclude: true };
    expect(isHidden(flow({ path: '/app.js.map' }), h)).toBe(true);
    expect(isHidden(flow({ path: '/app.js' }), h)).toBe(false);
  });

  it('CSS/JS/fonts/media categories match', () => {
    expect(isHidden(flow({ path: '/a.css' }), { cats: ['css'], ext: '' })).toBe(true);
    expect(isHidden(flow({ mime: 'application/javascript' }), { cats: ['js'], ext: '' })).toBe(true);
    expect(isHidden(flow({ path: '/f.woff2' }), { cats: ['fonts'], ext: '' })).toBe(true);
    expect(isHidden(flow({ mime: 'video/mp4' }), { cats: ['media'], ext: '' })).toBe(true);
  });
});
